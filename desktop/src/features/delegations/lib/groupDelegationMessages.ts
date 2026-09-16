import type { TimelineMessage } from "@/features/messages/types";
import type { DelegationDisplayState } from "@/shared/api/delegationTypes";

// Tag names this transform recognises. Kept local (not a shared constants
// file) since nothing else in the desktop client currently reads them.
const TAG_DELEGATION = "buzz:delegation";
const TAG_DELEGATION_RUN = "buzz:delegation-run";
const TAG_DELEGATION_NOTICE = "buzz:delegation-notice";
const TAG_DELEGATION_OUTCOME = "buzz:delegation-outcome";
const TAG_DELEGATION_TOKENS = "buzz:delegation-tokens";

export type DelegationSummaryData = {
  delegationId: string;
  state: DelegationDisplayState;
  tokensUsed: number | null;
  /** The raw messages this summary consolidates, oldest first, for the
   * "expand to raw notices" affordance. */
  rawMessages: TimelineMessage[];
};

function tagValue(tags: string[][] | undefined, name: string): string | undefined {
  return tags?.find((tag) => tag[0] === name)?.[1];
}

/** Whether `message` is one this transform consolidates (build_spec.md 6.4):
 * any relay-signed message carrying `buzz:delegation` — the wake
 * (`buzz:delegation-run`) or a notice (`buzz:delegation-notice`) — or any
 * agent-signed outcome carrying `buzz:delegation-outcome`. All three always
 * also carry `buzz:delegation` (the delegation id) per the frozen wake/
 * notice/outcome tag shapes. */
function delegationIdOf(message: TimelineMessage): string | null {
  const hasDelegationTag = tagValue(message.tags, TAG_DELEGATION);
  const isWake = tagValue(message.tags, TAG_DELEGATION_RUN) != null;
  const isOutcome = tagValue(message.tags, TAG_DELEGATION_OUTCOME) != null;
  const isNotice = tagValue(message.tags, TAG_DELEGATION_NOTICE) != null;
  if (!hasDelegationTag || (!isWake && !isOutcome && !isNotice)) return null;
  return hasDelegationTag;
}

/** Latest-wins state derivation: notice `approved` → running until a
 * terminal signal arrives; an outcome event or a terminal notice wins over
 * `running`, chosen by whichever consolidated message has the greatest
 * `createdAt`. */
function deriveState(messages: TimelineMessage[]): {
  state: DelegationDisplayState;
  tokensUsed: number | null;
} {
  const sorted = [...messages].sort((a, b) => a.createdAt - b.createdAt);
  let state: DelegationDisplayState = "approved";
  let tokensUsed: number | null = null;

  for (const message of sorted) {
    const outcome = tagValue(message.tags, TAG_DELEGATION_OUTCOME);
    const notice = tagValue(message.tags, TAG_DELEGATION_NOTICE);
    const isWake = tagValue(message.tags, TAG_DELEGATION_RUN) != null;
    if (outcome) {
      const tokens = tagValue(message.tags, TAG_DELEGATION_TOKENS);
      tokensUsed = tokens != null ? Number(tokens) : null;
      state =
        outcome === "delivered"
          ? "delivered"
          : outcome === "delegated"
            ? "running"
            : outcome === "failed"
              ? "failed"
              : outcome === "budget_exceeded"
                ? "failed"
                : state;
    } else if (notice === "approved") {
      state = "running";
    } else if (notice === "failed") {
      state = "failed";
    } else if (notice === "expired") {
      state = "expired";
    } else if (isWake) {
      // The wake carries neither a notice nor an outcome tag — it dispatched
      // the task, so the delegation is running unless a later message in
      // this same sorted pass says otherwise.
      state = "running";
    }
  }

  return { state, tokensUsed };
}

/**
 * Consolidate every message sharing a `delegation_id` into one synthetic
 * summary `TimelineMessage` (Slice 4 spec 6.4). Non-delegation messages —
 * including the target's own ordinary reply, which never carries these tags
 * — pass through untouched. The synthetic entry replaces the position of the
 * EARLIEST consolidated message so it renders where the delegation first
 * appeared in the timeline, not where it last updated.
 */
export function groupDelegationMessages(
  messages: TimelineMessage[],
): TimelineMessage[] {
  const byDelegationId = new Map<string, TimelineMessage[]>();
  for (const message of messages) {
    const id = delegationIdOf(message);
    if (!id) continue;
    const group = byDelegationId.get(id) ?? [];
    group.push(message);
    byDelegationId.set(id, group);
  }
  if (byDelegationId.size === 0) return messages;

  const consumedIds = new Set<string>();
  const summaryByEarliestId = new Map<string, DelegationSummaryData>();
  for (const [delegationId, group] of byDelegationId) {
    const { state, tokensUsed } = deriveState(group);
    const earliest = [...group].sort(
      (a, b) => a.createdAt - b.createdAt,
    )[0];
    for (const message of group) consumedIds.add(message.id);
    summaryByEarliestId.set(earliest.id, {
      delegationId,
      state,
      tokensUsed,
      rawMessages: [...group].sort((a, b) => a.createdAt - b.createdAt),
    });
  }

  const result: TimelineMessage[] = [];
  for (const message of messages) {
    const summary = summaryByEarliestId.get(message.id);
    if (summary) {
      result.push({ ...message, delegationSummary: summary });
      continue;
    }
    if (consumedIds.has(message.id)) continue;
    result.push(message);
  }
  return result;
}
