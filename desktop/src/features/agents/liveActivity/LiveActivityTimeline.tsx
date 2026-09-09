/**
 * Live Activity Timeline — flag-gated, mounted from ManagedAgentSessionPanel.
 *
 * States: live, stale (10 s no frame), disconnected, archive-disabled,
 * "activity may be incomplete (N missed)" per agent stream.
 */

import * as React from "react";
import {
  Activity,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Clock,
  Wifi,
  WifiOff,
} from "lucide-react";

import type { ConnectionState, ObserverEvent } from "@/features/agents/ui/agentSessionTypes";
import { cn } from "@/shared/lib/cn";
import { Badge } from "@/shared/ui/badge";
import { Skeleton } from "@/shared/ui/skeleton";
import {
  mapObserverEvents,
  type TimelineEntry,
  type TimelineResult,
} from "./mapObserverEvents";
import { LIVE_ACTIVITY_STALE_THRESHOLD_MS } from "./liveActivityConstants";

// ── Props ─────────────────────────────────────────────────────────────────

export type LiveActivityTimelineProps = {
  /** Raw observer events to render (live + archive merged). */
  events: readonly ObserverEvent[];
  /** Connection state from useObserverEvents. */
  connectionState: ConnectionState;
  /** Error message from useObserverEvents. */
  errorMessage: string | null;
  /** Whether the agent is reported running (controls stale detection). */
  agentRunning: boolean;
  /** Agent pubkey for stream keying. */
  agentPubkey: string;
  /** Whether archive history is available. */
  archiveEnabled: boolean;
  /** Called to load the next page of archived events. */
  fetchOlderArchived?: () => Promise<void>;
  /** Whether there are older archived events available to load. */
  hasOlderArchived?: boolean;
  /** Optional className. */
  className?: string;
};

// ── Latency instrumentation (dev-only) ────────────────────────────────────

export type LiveActivityPaintSample = {
  performanceMs: number;
  epochMs: number;
  count: number;
};

/** Exported for the gate-run script — logs paint samples to dev console. */
export function useLiveActivityPaintLog(
  events: readonly ObserverEvent[],
): LiveActivityPaintSample | null {
  const [paint, setPaint] = React.useState<LiveActivityPaintSample | null>(null);

  React.useEffect(() => {
    const sample: LiveActivityPaintSample = {
      performanceMs: performance.now(),
      epochMs: Date.now(),
      count: events.length,
    };
    setPaint(sample);
    if (import.meta.env.DEV) {
      console.debug(
        `[live-activity] paint perf=${sample.performanceMs.toFixed(2)}ms epoch=${sample.epochMs} n=${sample.count}`,
      );
    }
  }, [events]);

  return paint;
}

// ── Component ─────────────────────────────────────────────────────────────

export function LiveActivityTimeline({
  events,
  connectionState,
  errorMessage,
  agentRunning,
  agentPubkey,
  archiveEnabled,
  fetchOlderArchived,
  hasOlderArchived,
  className,
}: LiveActivityTimelineProps) {
  const paint = useLiveActivityPaintLog(events);

  // Archive paging state for "Show more".
  const [loadingMore, setLoadingMore] = React.useState(false);
  const handleShowMore = React.useCallback(async () => {
    if (!fetchOlderArchived || loadingMore) return;
    setLoadingMore(true);
    try {
      await fetchOlderArchived();
    } finally {
      setLoadingMore(false);
    }
  }, [fetchOlderArchived, loadingMore]);

  // Map events to timeline entries with seq-gap detection.
  const timeline = React.useMemo<TimelineResult>(
    () => mapObserverEvents(events, agentPubkey),
    [events, agentPubkey],
  );

  // Stale detection: no events for > STALE_THRESHOLD while agent is running.
  const [stale, setStale] = React.useState(false);
  React.useEffect(() => {
    if (!agentRunning || connectionState !== "open") {
      setStale(false);
      return;
    }
    if (timeline.entries.length === 0) return;
    const latest = timeline.entries[timeline.entries.length - 1];
    const latestMs = Date.parse(latest.timestamp);
    if (!Number.isFinite(latestMs)) return;

    const check = () => {
      const age = Date.now() - latestMs;
      setStale(age > LIVE_ACTIVITY_STALE_THRESHOLD_MS);
    };
    check();
    const interval = setInterval(check, 1000);
    return () => clearInterval(interval);
  }, [agentRunning, connectionState, timeline.entries]);

  // Disconnected state.
  if (connectionState === "error" || connectionState === "closed") {
    return (
      <div className={cn("py-3", className)}>
        <DisconnectedBanner
          connectionState={connectionState}
          errorMessage={errorMessage}
        />
      </div>
    );
  }

  // Connecting skeleton.
  if (connectionState === "connecting" && timeline.entries.length === 0) {
    return (
      <div className={cn("space-y-3 py-3", className)}>
        <Skeleton className="h-4 w-32" />
        <Skeleton className="h-4 w-48" />
        <Skeleton className="h-4 w-40" />
      </div>
    );
  }

  // Empty: no events yet.
  if (timeline.entries.length === 0) {
    return (
      <div className={cn("py-3", className)}>
        <EmptyState agentRunning={agentRunning} />
      </div>
    );
  }

  const streamState = timeline.streams[agentPubkey];
  const showIncomplete =
    streamState?.incomplete && streamState.gapCount > 0;

  return (
    <div className={cn("flex flex-col gap-2", className)}>
      {/* Status bar */}
      <StatusBar
        stale={stale}
        connectionState={connectionState}
        paint={paint}
        eventCount={timeline.entries.length}
      />

      {/* Per-stream incomplete banner */}
      {showIncomplete ? (
        <IncompleteBanner gapCount={streamState.gapCount} />
      ) : null}

      {/* Archive-disabled notice */}
      {!archiveEnabled ? (
        <p className="text-xs text-muted-foreground italic">
          History off — older activity is not being saved.
        </p>
      ) : null}

      {/* Timeline entries */}
      <ol
        className="space-y-1"
        role="list"
        aria-label="Agent activity timeline"
      >
        {timeline.entries.map((entry) => (
          <TimelineRow key={entry.id} entry={entry} />
        ))}
      </ol>

      {/* Show more: only when archive is enabled and older pages are available. */}
      {archiveEnabled && hasOlderArchived ? (
        <div className="flex justify-center pt-1">
          <button
            type="button"
            className="rounded-md px-3 py-1 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-50"
            onClick={handleShowMore}
            disabled={loadingMore}
          >
            {loadingMore ? "Loading…" : "Show more"}
          </button>
        </div>
      ) : null}
    </div>
  );
}

// ── Sub-components ────────────────────────────────────────────────────────

function StatusBar({
  stale,
  connectionState,
  paint,
  eventCount,
}: {
  stale: boolean;
  connectionState: ConnectionState;
  paint: LiveActivityPaintSample | null;
  eventCount: number;
}) {
  const statusIcon =
    connectionState === "open" ? (
      <Wifi className="h-3 w-3 text-green-500" aria-label="Connected" />
    ) : (
      <WifiOff className="h-3 w-3 text-muted-foreground" aria-label="Disconnected" />
    );

  return (
    <div className="flex items-center gap-2 text-xs text-muted-foreground">
      {statusIcon}
      {stale ? (
        <Badge variant="secondary" className="gap-1">
          <Clock className="h-3 w-3" />
          Stale
        </Badge>
      ) : (
        <Badge variant="secondary" className="gap-1">
          <Activity className="h-3 w-3" />
          Live
        </Badge>
      )}
      <span className="font-mono">{eventCount} event{eventCount === 1 ? "" : "s"}</span>
      {import.meta.env.DEV && paint ? (
        <span className="font-mono text-[10px] opacity-50">
          paint={paint.performanceMs.toFixed(0)}ms
        </span>
      ) : null}
    </div>
  );
}

function IncompleteBanner({ gapCount }: { gapCount: number }) {
  return (
    <div
      className="flex items-center gap-2 rounded border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-600 dark:text-amber-400"
      role="alert"
    >
      <AlertTriangle className="h-3.5 w-3.5 flex-shrink-0" />
      <span>Activity may be incomplete ({gapCount} missed)</span>
    </div>
  );
}

function DisconnectedBanner({
  connectionState,
  errorMessage,
}: {
  connectionState: ConnectionState;
  errorMessage: string | null;
}) {
  return (
    <div className="flex flex-col items-center gap-1 py-4 text-center">
      <WifiOff className="h-5 w-5 text-muted-foreground" />
      <p className="text-sm font-medium">
        {connectionState === "error" ? "Live activity unavailable" : "Connection closed"}
      </p>
      {errorMessage ? (
        <p className="text-xs text-muted-foreground">{errorMessage}</p>
      ) : null}
    </div>
  );
}

function EmptyState({ agentRunning }: { agentRunning: boolean }) {
  return (
    <div className="flex flex-col items-center gap-1 py-4 text-center">
      <Activity className="h-5 w-5 text-muted-foreground" />
      <p className="text-sm font-medium">
        {agentRunning
          ? "Waiting for the agent's next update."
          : "Restart this agent to reconnect live activity."}
      </p>
    </div>
  );
}

// ── Timeline row ──────────────────────────────────────────────────────────

function TimelineRow({ entry }: { entry: TimelineEntry }) {
  const [expanded, setExpanded] = React.useState(false);
  const hasDetail = entry.detail != null && entry.detail.length > 0;

  return (
    <li className="group">
      <button
        type="button"
        className={cn(
          "flex w-full items-start gap-2 rounded px-2 py-1 text-left text-xs transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          hasDetail && "cursor-pointer",
        )}
        onClick={() => hasDetail && setExpanded(!expanded)}
        onKeyDown={(e) => {
          if (hasDetail && (e.key === "Enter" || e.key === " ")) {
            e.preventDefault();
            setExpanded(!expanded);
          }
        }}
        aria-expanded={hasDetail ? expanded : undefined}
        tabIndex={0}
      >
        {/* Expand/collapse icon */}
        {hasDetail ? (
          expanded ? (
            <ChevronDown className="mt-0.5 h-3 w-3 flex-shrink-0 text-muted-foreground" />
          ) : (
            <ChevronRight className="mt-0.5 h-3 w-3 flex-shrink-0 text-muted-foreground" />
          )
        ) : (
          <span className="mt-0.5 h-3 w-3 flex-shrink-0" />
        )}

        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="font-medium truncate">{entry.title}</span>
            <span className="flex-shrink-0 font-mono text-[10px] text-muted-foreground/60">
              {entry.timestamp.slice(11, 19)}
            </span>
          </div>

          {/* Inline excerpt when collapsed */}
          {!expanded && entry.excerpt ? (
            <p className="mt-0.5 truncate text-muted-foreground/80">
              {entry.excerpt}
              {entry.hasMore ? (
                <span className="ml-1 text-muted-foreground/50">…show more</span>
              ) : null}
            </p>
          ) : null}

          {/* Collapsed short detail (no truncation needed) */}
          {!expanded && !entry.excerpt && entry.detail ? (
            <p className="mt-0.5 line-clamp-2 text-muted-foreground/80">
              {entry.detail}
            </p>
          ) : null}
        </div>
      </button>

      {/* Expanded detail */}
      {expanded && hasDetail ? (
        <div className="ml-7 mt-0.5 rounded border border-border/50 bg-muted/20 px-2 py-1.5 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-all">
          {entry.detail}
        </div>
      ) : null}
    </li>
  );
}