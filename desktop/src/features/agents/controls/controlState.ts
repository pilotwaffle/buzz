/**
 * Per-agent structured-control reducer (Slice 2, §6.2).
 *
 * Manages the lifecycle of:
 * - One-shot control commands (cancel, steer): pending → acked/rejected/expired
 * - Pause-lease transitions (pause, renew, resume): lease state mirror
 *
 * Each agent gets its own ControlState. The caller is responsible for
 * dispatching retries (single byte-identical re-publish after 3 s without ack)
 * and expiry (transition to `expired` at `expires_at + 5 s` skew).
 *
 * Module-level lease sharing (§6.5 / N5): lease state is also published to a
 * module-level Map so the paused badge on ManagedAgentRow stays in sync without
 * lifting state through the component tree.
 */

// ── Module-level lease state (cross-component sharing) ─────────────────────

type LeaseListener = (lease: LeaseState) => void;

const sharedLeases = new Map<string, LeaseState>();
const leaseListeners = new Map<string, Set<LeaseListener>>();

/** Publish a lease update so other components (ManagedAgentRow) can read it. */
export function setSharedLeaseState(
  agentPubkey: string,
  lease: LeaseState,
): void {
  const norm = agentPubkey.trim().toLowerCase();
  sharedLeases.set(norm, lease);
  const listeners = leaseListeners.get(norm);
  if (listeners) {
    for (const fn of listeners) {
      try { fn(lease); } catch { /* swallow listener errors */ }
    }
  }
}

/** Read the last-known lease state for an agent. */
export function getSharedLeaseState(
  agentPubkey: string,
): LeaseState | undefined {
  return sharedLeases.get(agentPubkey.trim().toLowerCase());
}

/**
 * Subscribe to lease state changes for an agent.
 * Returns an unsubscribe function.
 */
export function subscribeSharedLeaseState(
  agentPubkey: string,
  listener: LeaseListener,
): () => void {
  const norm = agentPubkey.trim().toLowerCase();
  let set = leaseListeners.get(norm);
  if (!set) {
    set = new Set();
    leaseListeners.set(norm, set);
  }
  set.add(listener);
  return () => {
    set?.delete(listener);
    if (set && set.size === 0) {
      leaseListeners.delete(norm);
      sharedLeases.delete(norm);
    }
  };
}

// ── Types ────────────────────────────────────────────────────────────────────

export type ControlKind = "cancel" | "steer" | "pause" | "renew" | "resume";

export type ControlEntryState = "pending" | "acked" | "expired" | "rejected";

export interface AckData {
  /** ack_id from the agent-signed acknowledgement */
  ackId: string;
  /** status from the ack: applied, no_active_turn, queued, rejected, already_applied */
  status: string;
  /** reason from the ack (binding_mismatch, internal_error, unsupported), if present */
  reason?: string;
  /** detail text from the ack, if present */
  detailText?: string;
  /** acked_at Unix seconds from the ack payload */
  ackedAt: number;
  /** epoch-millis when the desktop received the ack */
  receivedEpochMs: number;
}

export interface ControlEntry {
  kind: ControlKind;
  /** command_id or transition_id */
  id: string;
  /** Unix seconds when the control was sent (issued_at) */
  sentAt: number;
  /** Unix seconds when the control expires (expires_at or transition_expires_at) */
  expiresAt: number;
  /** Current lifecycle state */
  state: ControlEntryState;
  /** Ack data when state is 'acked' or 'rejected' */
  ack?: AckData;
  /** Remaining retry count (starts at 1 for the single byte-identical retry) */
  retries: number;
  /** epoch-millis of last send / retry */
  lastSentEpochMs: number;
  /**
   * The signed RelayEvent from the initial send, cached so retryControl can
   * re-publish byte-identically (N6). Undefined until the async send completes.
   */
  originalEvent?: unknown;
}

export interface LeaseState {
  leaseId: string | null;
  generation: number;
  leaseExpiresAt: number;
  /** 'paused' while an active lease holds the queue, 'running' otherwise */
  queueState: "paused" | "running";
}

export interface AgentControlState {
  entries: ControlEntry[];
  lease: LeaseState;
  /** Monotonic seq counter for the next outgoing control */
  nextSeq: number;
}

// ── Initial state ────────────────────────────────────────────────────────────

export function createControlState(): AgentControlState {
  return {
    entries: [],
    lease: {
      leaseId: null,
      generation: 0,
      leaseExpiresAt: 0,
      queueState: "running",
    },
    nextSeq: 1,
  };
}

// ── Actions ──────────────────────────────────────────────────────────────────

export type ControlAction =
  | {
      type: "control_sent";
      kind: ControlKind;
      id: string;
      sentAt: number;
      expiresAt: number;
      /** epoch-millis when published */
      sentEpochMs: number;
      /** The signed RelayEvent for byte-identical retry (N6) */
      originalEvent?: unknown;
    }
  | {
      type: "control_acked";
      id: string;
      ack: AckData;
      /** For pause/renew/resume: new lease state from the ack */
      leaseState?: LeaseState;
    }
  | {
      type: "control_expired";
      id: string;
      nowEpochMs: number;
    }
  | { type: "control_retried"; id: string; sentEpochMs: number }
  | {
      type: "control_event_stored";
      id: string;
      /** The signed RelayEvent for byte-identical retry (N6) */
      originalEvent: unknown;
    }
  | { type: "lease_updated"; lease: LeaseState };

// ── Reducer ──────────────────────────────────────────────────────────────────

export function controlReducer(
  state: AgentControlState,
  action: ControlAction,
): AgentControlState {
  switch (action.type) {
    case "control_sent": {
      const entry: ControlEntry = {
        kind: action.kind,
        id: action.id,
        sentAt: action.sentAt,
        expiresAt: action.expiresAt,
        state: "pending",
        retries: 1, // one byte-identical retry allowed
        lastSentEpochMs: action.sentEpochMs,
        originalEvent: action.originalEvent,
      };
      return {
        ...state,
        entries: [...state.entries, entry],
        nextSeq: state.nextSeq + 1,
      };
    }

    case "control_acked": {
      const entries = state.entries.map((e) => {
        if (e.id !== action.id) return e;
        return {
          ...e,
          state: (action.ack.status === "rejected" ? "rejected" : "acked") as ControlEntryState,
          ack: action.ack,
          retries: 0,
        };
      });
      let lease = state.lease;
      if (action.leaseState) {
        lease = action.leaseState;
      }
      return { ...state, entries, lease };
    }

    case "control_event_stored": {
      const entries = state.entries.map((e) => {
        if (e.id !== action.id) return e;
        return { ...e, originalEvent: action.originalEvent };
      });
      return { ...state, entries };
    }

    case "control_retried": {
      const entries = state.entries.map((e) => {
        if (e.id !== action.id) return e;
        return {
          ...e,
          retries: e.retries - 1,
          lastSentEpochMs: action.sentEpochMs,
        };
      });
      return { ...state, entries };
    }

    case "control_expired": {
      const entries = state.entries.map((e) => {
        if (e.id !== action.id || e.state !== "pending") return e;
        return { ...e, state: "expired" as const, retries: 0 };
      });
      // If the expired control was a pause, clear the lease if it matches
      let lease = state.lease;
      if (lease.queueState === "paused") {
        // Check if any non-expired pause still holds
        const hasActivePause = entries.some(
          (e) =>
            e.kind === "pause" &&
            e.state === "pending" &&
            e.id !== action.id,
        );
        if (!hasActivePause) {
          lease = { ...lease, queueState: "running" as const };
        }
      }
      return { ...state, entries, lease };
    }

    case "lease_updated": {
      return { ...state, lease: action.lease };
    }

    default:
      return state;
  }
}

// ── Selectors ────────────────────────────────────────────────────────────────

/** Returns the most recent entry for a given kind, if still pending. */
export function pendingForKind(
  state: AgentControlState,
  kind: ControlKind,
): ControlEntry | undefined {
  // Search in reverse (most recent first)
  for (let i = state.entries.length - 1; i >= 0; i--) {
    const e = state.entries[i];
    if (e.kind === kind && e.state === "pending") return e;
  }
  return undefined;
}

/** Returns entries that are past their expiry and still pending. */
export function pendingExpired(
  state: AgentControlState,
  nowEpochMs: number,
): ControlEntry[] {
  const nowSecs = Math.floor(nowEpochMs / 1000);
  const skewSecs = 5; // +5 s skew per spec
  return state.entries.filter(
    (e) =>
      e.state === "pending" &&
      nowSecs >= e.expiresAt + skewSecs,
  );
}

/** Returns entries that are pending, past 3 s since last send, and still have retries. */
export function pendingRetry(
  state: AgentControlState,
  nowEpochMs: number,
): ControlEntry[] {
  const retryWindowMs = 3000; // 3 s per spec
  return state.entries.filter(
    (e) =>
      e.state === "pending" &&
      e.retries > 0 &&
      nowEpochMs - e.lastSentEpochMs >= retryWindowMs,
  );
}