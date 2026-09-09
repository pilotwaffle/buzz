/**
 * Pure mapping module: decoded observer events → structured timeline entries.
 *
 * - Input: decoded `ObserverEvent[]` (the desktop-side shape from `useObserverEvents`).
 * - Output: `TimelineEntry[]` with human-readable titles, byte-capped excerpts,
 *   and per-stream seq-gap detection (Q2).
 * - Raw observer JSON never escapes this module — verify with a test assertion.
 */

import type { ObserverEvent } from "@/features/agents/ui/agentSessionTypes";
import { EXCERPT_BYTE_CAP } from "./liveActivityConstants";

// ── Output types ──────────────────────────────────────────────────────────

export type TimelineEntry = {
  /** Stable id: `${seq}:${timestamp}` */
  id: string;
  kind: string;
  /** Human-readable title — NO protocol terms (finding 23). */
  title: string;
  /** Collapsible detail text (tool result, read body, thought text, etc.). */
  detail: string | null;
  /** First N bytes of detail for inline preview; null if detail fits. */
  excerpt: string | null;
  /** Whether the detail exceeds EXCERPT_BYTE_CAP and needs "show more." */
  hasMore: boolean;
  timestamp: string;
  seq: number;
  /** Publisher stream key (agent pubkey). */
  streamKey: string;
  /** Turn id if the event carries one. */
  turnId: string | null;
  /** Session id if the event carries one. */
  sessionId: string | null;
  /** Channel id if the event carries one. */
  channelId: string | null;
};

export type StreamGapState = {
  incomplete: boolean;
  gapCount: number;
};

export type TimelineResult = {
  entries: TimelineEntry[];
  /** Per-stream gap state, keyed by agent pubkey. */
  streams: Record<string, StreamGapState>;
};

// ── Human-readable titles per event kind ──────────────────────────────────

const KIND_TITLES: Record<string, string> = {
  turn_started: "Turn started",
  turn_completed: "Turn completed",
  turn_failed: "Turn failed",
  acp_read: "Reading",
  acp_write: "Writing",
  acp_edit: "Editing file",
  acp_shell: "Running command",
  acp_todo: "Planning",
  acp_thought: "Thinking",
  acp_permission: "Permission request",
  acp_error: "Error",
  prompt_delivered: "Prompt delivered",
  session_started: "Session started",
  session_config_captured: "Configuration captured",
  lifecycle: "Status change",
  control_result: "Control result",
  managed_agent_runtime_lifecycle: "Runtime event",
};

function humanTitle(kind: string): string {
  return KIND_TITLES[kind] ?? kind.replace(/_/g, " ");
}

// ── Detail extraction ─────────────────────────────────────────────────────

function extractDetail(
  kind: string,
  payload: unknown,
): string | null {
  if (payload == null || typeof payload !== "object") return null;
  const p = payload as Record<string, unknown>;

  // Common observer payload shapes, ordered by specificity.
  if (typeof p.body === "string" && p.body.length > 0) return p.body;
  if (typeof p.text === "string" && p.text.length > 0) return p.text;
  if (typeof p.message === "string" && p.message.length > 0) return p.message;
  if (typeof p.error === "string" && p.error.length > 0) return `Error: ${p.error}`;
  if (typeof p.command === "string" && p.command.length > 0) return `$ ${p.command}`;

  // Tool result: prefer structured fields.
  if (kind === "acp_read" || kind === "acp_shell" || kind === "acp_edit") {
    if (typeof p.result === "string" && p.result.length > 0) return p.result;
    if (typeof p.output === "string" && p.output.length > 0) return p.output;
  }

  // Permission: describe what was asked.
  if (kind === "acp_permission") {
    if (typeof p.tool_name === "string") {
      return `Requested: ${p.tool_name}`;
    }
    return "Requested tool access";
  }

  // Tool use.
  if (typeof p.tool_name === "string") {
    const toolName = p.tool_name as string;
    if (typeof p.args === "object" && p.args != null) {
      return `${toolName}(${JSON.stringify(p.args).slice(0, 200)})`;
    }
    return toolName;
  }

  // Fallback: serialize non-empty payload.
  try {
    const keys = Object.keys(p);
    if (keys.length > 0) {
      return JSON.stringify(p).slice(0, EXCERPT_BYTE_CAP * 2);
    }
  } catch {
    // Ignore serialization failures.
  }

  return null;
}

// ── Excerpt with byte cap ─────────────────────────────────────────────────

function byteLength(s: string): number {
  return new TextEncoder().encode(s).length;
}

function makeExcerpt(detail: string | null): {
  excerpt: string | null;
  hasMore: boolean;
} {
  if (detail == null) return { excerpt: null, hasMore: false };
  if (byteLength(detail) <= EXCERPT_BYTE_CAP) {
    return { excerpt: null, hasMore: false };
  }
  // Truncate to EXCERPT_BYTE_CAP bytes at a UTF-8 boundary.
  let truncated = "";
  let bytes = 0;
  for (const char of detail) {
    const charBytes = new TextEncoder().encode(char).length;
    if (bytes + charBytes > EXCERPT_BYTE_CAP) break;
    truncated += char;
    bytes += charBytes;
  }
  return { excerpt: truncated, hasMore: true };
}

// ── Seq-gap detection (Q2) ────────────────────────────────────────────────

/**
 * Per-stream seq tracker. Rising non-contiguous seq = gap (accumulates).
 * Decreasing seq = stream reset (harness restart; seq restarts at 1) — NOT a gap.
 */
class SeqTracker {
  private lastSeq: number | null = null;
  private initialized = false;
  gapCount = 0;
  incomplete = false;

  ingest(seq: number): void {
    if (!this.initialized) {
      this.lastSeq = seq;
      this.initialized = true;
      return;
    }
    if (seq <= (this.lastSeq ?? 0)) {
      // Decreasing or equal seq → stream reset (or dedup).
      // Reset the tracker: seq restarts at 1 on harness restart.
      this.lastSeq = seq;
      return;
    }
    // Rising seq: check for gaps.
    const expected = (this.lastSeq ?? 0) + 1;
    if (seq > expected) {
      this.gapCount += seq - expected;
      this.incomplete = true;
    }
    this.lastSeq = seq;
  }
}

// ── Main mapping function ─────────────────────────────────────────────────

export function mapObserverEvents(
  events: readonly ObserverEvent[],
  streamKey: string,
  existingStreams?: Record<string, StreamGapState>,
): TimelineResult {
  const tracker = new SeqTracker();
  // Carry forward existing gap state if provided (e.g. across archive/live merges).
  if (existingStreams?.[streamKey]) {
    tracker.gapCount = existingStreams[streamKey].gapCount;
    tracker.incomplete = existingStreams[streamKey].incomplete;
  }

  const entries: TimelineEntry[] = [];

  for (const event of events) {
    tracker.ingest(event.seq);

    const detail = extractDetail(event.kind, event.payload);
    const { excerpt, hasMore } = makeExcerpt(detail);

    entries.push({
      id: `${event.seq}:${event.timestamp}`,
      kind: event.kind,
      title: humanTitle(event.kind),
      detail,
      excerpt,
      hasMore,
      timestamp: event.timestamp,
      seq: event.seq,
      streamKey,
      turnId: event.turnId ?? null,
      sessionId: event.sessionId ?? null,
      channelId: event.channelId ?? null,
    });
  }

  const streamState: StreamGapState = {
    incomplete: tracker.incomplete,
    gapCount: tracker.gapCount,
  };

  return {
    entries,
    streams: { [streamKey]: streamState },
  };
}

/**
 * Merge results from multiple streams (e.g. multiple agents in a shared view).
 * Streams are kept separate in the result; entries are sorted by (timestamp, seq).
 */
export function mergeTimelineResults(results: TimelineResult[]): TimelineResult {
  const allEntries: TimelineEntry[] = [];
  const allStreams: Record<string, StreamGapState> = {};

  for (const result of results) {
    allEntries.push(...result.entries);
    Object.assign(allStreams, result.streams);
  }

  allEntries.sort((a, b) => {
    const aTime = Date.parse(a.timestamp);
    const bTime = Date.parse(b.timestamp);
    if (Number.isFinite(aTime) && Number.isFinite(bTime)) {
      const diff = aTime - bTime;
      if (diff !== 0) return diff;
    }
    return a.seq - b.seq;
  });

  return { entries: allEntries, streams: allStreams };
}