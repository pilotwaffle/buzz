/**
 * AgentControlsBar — Slice 2 structured-control UI (§6.4).
 *
 * Four buttons: Cancel, Steer…, Pause/Resume (toggle), Renew +5 min (while paused).
 * Each is a real <button> with aria-label, disabled while a same-kind control is
 * pending, keyboard reachable in DOM order, focus ring visible.
 *
 * State chip per last control: pending (spinner), acked (applied|queued|no_active_turn),
 * rejected (<reason>), expired.
 */

import * as React from "react";
import { Pause, Play, Clock, X, GripHorizontal } from "lucide-react";

import { Button } from "@/shared/ui/button";
import { Spinner } from "@/shared/ui/spinner";
import { Textarea } from "@/shared/ui/textarea";
import { Badge } from "@/shared/ui/badge";
import { cn } from "@/shared/lib/cn";
import { normalizePubkey } from "@/shared/lib/pubkey";
import {
  buildCancelCommand,
  buildPauseTransition,
  buildRenewTransition,
  buildResumeTransition,
  buildSteerCommand,
  type ControlTarget,
} from "@/shared/api/agentStructuredControl";
import type { RelayEvent } from "@/shared/api/types";
import { sendAgentObserverControl, retryPublishObserverControl } from "@/shared/api/observerRelay";
import { sendChannelMessage } from "@/shared/api/tauriMessages";
import {
  subscribeControlAcks,
  type ControlAckFrame,
} from "@/features/agents/observerRelayStore";
import {
  createControlState,
  controlReducer,
  pendingForKind,
  pendingExpired,
  pendingRetry,
  setSharedLeaseState,
  type ControlEntry,
  type ControlKind,
  type LeaseState,
} from "./controlState";
import { s1GateEnabled, s1GateLog } from "@/features/agents/liveActivity/LiveActivityTimeline";

// ── Types ────────────────────────────────────────────────────────────────────

export interface AgentControlsBarProps {
  agentPubkey: string;
  computerId: string;
  operatorPubkey: string;
  channelId: string | null;
  turnId: string | null;
  className?: string;
}

// ── Component ────────────────────────────────────────────────────────────────

export function AgentControlsBar({
  agentPubkey,
  computerId,
  operatorPubkey,
  channelId,
  turnId,
  className,
}: AgentControlsBarProps) {
  const [state, setState] = React.useState(createControlState);
  const [steerOpen, setSteerOpen] = React.useState(false);
  const [steerMessage, setSteerMessage] = React.useState("");
  const [steerSending, setSteerSending] = React.useState(false);
  const steerTextareaRef = React.useRef<HTMLTextAreaElement>(null);

  const normAgent = normalizePubkey(agentPubkey);
  const runId = turnId ?? "idle";
  const hasChannel = channelId !== null;

  // ── Subscribe to control acks ────────────────────────────────────────────
  React.useEffect(() => {
    return subscribeControlAcks(agentPubkey, (ack: ControlAckFrame) => {
      const ackId = ack.command_id ?? ack.transition_id;
      if (!ackId) return;

      const leaseState: LeaseState | undefined =
        ack.format === "buzz-agent-pause-lease-ack" && ack.queue_state
          ? {
              leaseId: ack.lease_id ?? null,
              generation: ack.generation ?? 0,
              leaseExpiresAt: 0, // updated from the sent entry's expires_at
              queueState: ack.queue_state as "paused" | "running",
            }
          : undefined;

      setState((prev) =>
        controlReducer(prev, {
          type: "control_acked",
          id: ackId,
          ack: {
            ackId: ack.ack_id,
            status: ack.status,
            reason: ack.reason,
            detailText: ack.detail?.text,
            ackedAt: ack.acked_at,
            receivedEpochMs: Date.now(),
          },
          leaseState,
        }),
      );

      if (s1GateEnabled()) {
        s1GateLog(
          `[agent-controls] ack id=${ackId} status=${ack.status} branch=${ack.reason ?? "none"} ms=${Date.now()}`,
        );
      }
    });
  }, [agentPubkey]);

  // ── Tick: expire stale entries, fire retries ──────────────────────────────
  React.useEffect(() => {
    const id = setInterval(() => {
      const nowMs = Date.now();
      setState((prev) => {
        let next = prev;

        // Expire
        for (const entry of pendingExpired(next, nowMs)) {
          next = controlReducer(next, {
            type: "control_expired",
            id: entry.id,
            nowEpochMs: nowMs,
          });
        }

        // Retry (single byte-identical re-publish)
        for (const entry of pendingRetry(next, nowMs)) {
          next = controlReducer(next, {
            type: "control_retried",
            id: entry.id,
            sentEpochMs: nowMs,
          });
          // Fire-and-forget retry — re-publish the last-sent payload
          retryControl(entry, agentPubkey);
        }

        return next;
      });
    }, 1000);
    return () => clearInterval(id);
  }, [agentPubkey]);

  // ── Sync lease state to module-level store for cross-component badges ────
  React.useEffect(() => {
    setSharedLeaseState(agentPubkey, state.lease);
  }, [agentPubkey, state.lease]);

  // Clear shared lease state on unmount so the paused badge doesn't go stale
  // when the controls bar is no longer mounted (e.g. switching agents/channels).
  React.useEffect(() => {
    const pubkey = agentPubkey;
    return () => {
      setSharedLeaseState(pubkey, {
        leaseId: null,
        generation: 0,
        leaseExpiresAt: 0,
        queueState: "running",
      });
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Helpers ──────────────────────────────────────────────────────────────

  const target = React.useMemo<ControlTarget>(
    () => ({
      computer_id: computerId,
      agent_pubkey: normAgent,
      channel_id: channelId ?? "00000000-0000-0000-0000-000000000000",
      run_id: runId,
    }),
    [computerId, normAgent, channelId, runId],
  );

  // ── Dispatch helpers ─────────────────────────────────────────────────────

  async function dispatchCancel() {
    if (!hasChannel) return;
    const seq = state.nextSeq;
    const issuedAt = Math.floor(Date.now() / 1000);
    const cmd = buildCancelCommand({
      operatorPubkey,
      target,
      seq,
      issuedAt,
    });
    setState((prev) =>
      controlReducer(prev, {
        type: "control_sent",
        kind: "cancel",
        id: cmd.command_id,
        sentAt: issuedAt,
        expiresAt: cmd.expires_at,
        sentEpochMs: Date.now(),
      }),
    );
    if (s1GateEnabled()) {
      s1GateLog(
        `[agent-controls] sent kind=cancel id=${cmd.command_id} ms=${Date.now()}`,
      );
    }
    const event = await sendAgentObserverControl(agentPubkey, cmd, issuedAt);
    setState((prev) =>
      controlReducer(prev, {
        type: "control_event_stored",
        id: cmd.command_id,
        originalEvent: event,
      }),
    );
  }

  async function dispatchSteer() {
    if (!hasChannel || !steerMessage.trim()) return;
    setSteerSending(true);
    try {
      // 1. Publish the operator message on the channel, mentioning the agent
      // so the sidecar's mention filter admits it (Defect 6 fix).
      const result = await sendChannelMessage(
        channelId!,
        steerMessage.trim(),
        undefined, // parentEventId
        undefined, // mediaTags
        [agentPubkey], // mentionPubkeys — same shape as managedAgentControlActions.ts
      );
      // 2. Build and send the steer command with the published event id
      const seq = state.nextSeq;
      const issuedAt = Math.floor(Date.now() / 1000);
      const cmd = buildSteerCommand({
        operatorPubkey,
        target,
        seq,
        issuedAt,
        steerMessageEventId: result.eventId,
      });
      setState((prev) =>
        controlReducer(prev, {
          type: "control_sent",
          kind: "steer",
          id: cmd.command_id,
          sentAt: issuedAt,
          expiresAt: cmd.expires_at,
          sentEpochMs: Date.now(),
        }),
      );
      if (s1GateEnabled()) {
        s1GateLog(
          `[agent-controls] sent kind=steer id=${cmd.command_id} steerEventId=${result.eventId} ms=${Date.now()}`,
        );
      }
      const event = await sendAgentObserverControl(agentPubkey, cmd, issuedAt);
      setState((prev) =>
        controlReducer(prev, {
          type: "control_event_stored",
          id: cmd.command_id,
          originalEvent: event,
        }),
      );
    } finally {
      setSteerSending(false);
      setSteerOpen(false);
      setSteerMessage("");
    }
  }

  async function dispatchPause() {
    if (!hasChannel) return;
    const seq = state.nextSeq;
    const issuedAt = Math.floor(Date.now() / 1000);
    const t = buildPauseTransition({ operatorPubkey, target, seq, issuedAt });
    setState((prev) =>
      controlReducer(prev, {
        type: "control_sent",
        kind: "pause",
        id: t.transition_id,
        sentAt: issuedAt,
        expiresAt: t.transition_expires_at,
        sentEpochMs: Date.now(),
      }),
    );
    if (s1GateEnabled()) {
      s1GateLog(
        `[agent-controls] sent kind=pause id=${t.transition_id} ms=${Date.now()}`,
      );
    }
    const event = await sendAgentObserverControl(agentPubkey, t, issuedAt);
    setState((prev) =>
      controlReducer(prev, {
        type: "control_event_stored",
        id: t.transition_id,
        originalEvent: event,
      }),
    );
  }

  async function dispatchRenew() {
    if (!hasChannel || !state.lease.leaseId) return;
    const seq = state.nextSeq;
    const issuedAt = Math.floor(Date.now() / 1000);
    const t = buildRenewTransition({
      operatorPubkey,
      target,
      seq,
      issuedAt,
      leaseId: state.lease.leaseId,
      generation: state.lease.generation + 1,
      leaseExpiresAt: issuedAt + 300,
    });
    setState((prev) =>
      controlReducer(prev, {
        type: "control_sent",
        kind: "renew",
        id: t.transition_id,
        sentAt: issuedAt,
        expiresAt: t.transition_expires_at,
        sentEpochMs: Date.now(),
      }),
    );
    if (s1GateEnabled()) {
      s1GateLog(
        `[agent-controls] sent kind=renew id=${t.transition_id} ms=${Date.now()}`,
      );
    }
    const event = await sendAgentObserverControl(agentPubkey, t, issuedAt);
    setState((prev) =>
      controlReducer(prev, {
        type: "control_event_stored",
        id: t.transition_id,
        originalEvent: event,
      }),
    );
  }

  async function dispatchResume() {
    if (!hasChannel || !state.lease.leaseId) return;
    const seq = state.nextSeq;
    const issuedAt = Math.floor(Date.now() / 1000);
    const t = buildResumeTransition({
      operatorPubkey,
      target,
      seq,
      issuedAt,
      leaseId: state.lease.leaseId,
      generation: state.lease.generation + 1,
    });
    setState((prev) =>
      controlReducer(prev, {
        type: "control_sent",
        kind: "resume",
        id: t.transition_id,
        sentAt: issuedAt,
        expiresAt: t.transition_expires_at,
        sentEpochMs: Date.now(),
      }),
    );
    if (s1GateEnabled()) {
      s1GateLog(
        `[agent-controls] sent kind=resume id=${t.transition_id} ms=${Date.now()}`,
      );
    }
    const event = await sendAgentObserverControl(agentPubkey, t, issuedAt);
    setState((prev) =>
      controlReducer(prev, {
        type: "control_event_stored",
        id: t.transition_id,
        originalEvent: event,
      }),
    );
  }

  // ── Pending check ────────────────────────────────────────────────────────

  const cancelPending = !!pendingForKind(state, "cancel");
  const steerPending = !!pendingForKind(state, "steer");
  const pausePending = !!pendingForKind(state, "pause");
  const renewPending = !!pendingForKind(state, "renew");
  const resumePending = !!pendingForKind(state, "resume");

  const isPaused = state.lease.queueState === "paused";

  // Latest control entry for display
  const latestEntry = state.entries.length > 0
    ? state.entries[state.entries.length - 1]
    : null;

  // ── No controls available ─────────────────────────────────────────────────

  if (!hasChannel) {
    return (
      <div className={cn("flex items-center gap-2 text-sm text-muted-foreground", className)}>
        <GripHorizontal className="h-4 w-4" />
        <span>Select a channel to control this agent</span>
      </div>
    );
  }

  if (!computerId) {
    return (
      <div className={cn("flex items-center gap-2 text-sm text-muted-foreground", className)}>
        <GripHorizontal className="h-4 w-4" />
        <span>Controls unavailable for this agent</span>
      </div>
    );
  }

  // ── Render ────────────────────────────────────────────────────────────────

  return (
    <div className={cn("space-y-2", className)}>
      {/* Button row */}
      <div className="flex flex-wrap items-center gap-2" role="toolbar" aria-label="Agent controls">
        {/* Cancel */}
        <Button
          variant="outline"
          size="sm"
          disabled={cancelPending || !hasChannel}
          aria-label="Cancel current turn"
          onClick={() => void dispatchCancel()}
        >
          <X className="mr-1.5 h-3.5 w-3.5" />
          Cancel
        </Button>

        {/* Steer… */}
        <Button
          variant="outline"
          size="sm"
          disabled={steerPending || steerSending || !hasChannel}
          aria-label="Steer agent with a message"
          onClick={() => {
            setSteerOpen((prev) => !prev);
            if (!steerOpen) {
              setTimeout(() => steerTextareaRef.current?.focus(), 50);
            }
          }}
        >
          <GripHorizontal className="mr-1.5 h-3.5 w-3.5" />
          Steer…
        </Button>

        {/* Pause / Resume toggle */}
        {isPaused ? (
          <>
            <Button
              variant="outline"
              size="sm"
              disabled={resumePending || !hasChannel}
              aria-label="Resume agent queue"
              onClick={() => void dispatchResume()}
            >
              <Play className="mr-1.5 h-3.5 w-3.5" />
              Resume
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={renewPending || !hasChannel}
              aria-label="Renew pause lease by 5 minutes"
              onClick={() => void dispatchRenew()}
            >
              <Clock className="mr-1.5 h-3.5 w-3.5" />
              Renew +5 min
            </Button>
          </>
        ) : (
          <Button
            variant="outline"
            size="sm"
            disabled={pausePending || !hasChannel}
            aria-label="Pause agent queue"
            onClick={() => void dispatchPause()}
          >
            <Pause className="mr-1.5 h-3.5 w-3.5" />
            Pause
          </Button>
        )}
      </div>

      {/* Steer textarea (collapsible) */}
      {steerOpen && (
        <div className="space-y-2 rounded border border-border p-2">
          <Textarea
            ref={steerTextareaRef}
            placeholder="Message to steer the agent…"
            value={steerMessage}
            onChange={(e) => setSteerMessage(e.target.value)}
            rows={2}
            className="resize-none text-sm"
            aria-label="Steer message"
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                if (steerMessage.trim() && !steerSending) {
                  void dispatchSteer();
                }
              }
            }}
          />
          <div className="flex items-center gap-2">
            <Button
              size="sm"
              disabled={!steerMessage.trim() || steerSending}
              onClick={() => void dispatchSteer()}
            >
              {steerSending ? (
                <>
                  <Spinner className="mr-1.5 h-3 w-3" /> Sending…
                </>
              ) : (
                "Send Steer"
              )}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setSteerOpen(false);
                setSteerMessage("");
              }}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}

      {/* State chips */}
      <div className="flex flex-wrap items-center gap-1.5">
        {latestEntry && <ControlStateChip entry={latestEntry} />}
        {isPaused && (
          <Badge variant="warning" className="gap-1">
            <Pause className="h-3 w-3" />
            Paused
          </Badge>
        )}
      </div>
    </div>
  );
}

// ── State chip ───────────────────────────────────────────────────────────────

function ControlStateChip({ entry }: { entry: ControlEntry }) {
  const label = controlKindLabel(entry.kind);

  switch (entry.state) {
    case "pending":
      return (
        <Badge variant="secondary" className="gap-1">
          <Spinner className="h-3 w-3" />
          {label} pending
        </Badge>
      );
    case "acked":
      return (
        <Badge variant="default" className="gap-1">
          {label}{" "}
          {entry.ack?.status === "applied"
            ? "applied"
            : entry.ack?.status === "queued"
              ? "queued"
              : entry.ack?.status === "no_active_turn"
                ? "no turn"
                : entry.ack?.status ?? "acked"}
        </Badge>
      );
    case "rejected":
      return (
        <Badge variant="destructive" className="gap-1">
          {label} rejected: {entry.ack?.reason ?? "unknown"}
        </Badge>
      );
    case "expired":
      return (
        <Badge variant="outline" className="gap-1 text-muted-foreground">
          {label} expired
        </Badge>
      );
    default:
      return null;
  }
}

function controlKindLabel(kind: ControlKind): string {
  switch (kind) {
    case "cancel":
      return "Cancel";
    case "steer":
      return "Steer";
    case "pause":
      return "Pause";
    case "renew":
      return "Renew";
    case "resume":
      return "Resume";
  }
}

// ── Retry helper ─────────────────────────────────────────────────────────────

async function retryControl(entry: ControlEntry, _agentPubkey: string) {
  // Byte-identical re-publish (N6): use the cached signed RelayEvent from the
  // initial send, with no new signature / seq / timestamp changes.
  if (s1GateEnabled()) {
    s1GateLog(
      `[agent-controls] retry kind=${entry.kind} id=${entry.id} ms=${Date.now()}`,
    );
  }
  if (!entry.originalEvent) {
    // Should not happen in normal operation — control_event_stored always fires
    // before the retry tick. Log and skip.
    if (s1GateEnabled()) {
      s1GateLog(
        `[agent-controls] retry SKIP kind=${entry.kind} id=${entry.id} — no cached originalEvent`,
      );
    }
    return;
  }
  await retryPublishObserverControl(entry.originalEvent as RelayEvent);
}