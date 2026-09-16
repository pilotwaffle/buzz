// Types for the desktop delegation review/approval flow (Slice 4, Step 6.1).
// Mirrors the wire shape `buzz_core_pkg::delegation::DelegationRequest`
// (crates/buzz-core/src/delegation.rs) — a v2 request always carries
// `token_budget` (required, non-zero) and no legacy v1 fields.

/** The `buzz-delegation` fenced-block request an agent drafts, v2. */
export type DelegationRequestV2 = {
  delegation_id: string;
  /**
   * Omitted by the drafting agent (D-4): it cannot know its own message's
   * event id before signing. The desktop fills this in from the containing
   * message's id when building the operator approval.
   */
  origin_event_id?: string;
  parent_approval_event_id: string | null;
  source_agent: string;
  target_agent: string;
  agent_path: string[];
  hop_budget: number;
  max_turns: number;
  cost_cap_microusd: number | null;
  token_budget: number;
  idempotency_key: string;
  expires_at: number;
};

/** The four frozen delegation outcome words (build_spec.md 4.4). */
export type DelegationOutcome =
  | "delivered"
  | "delegated"
  | "failed"
  | "budget_exceeded";

/** Parsed `buzz:delegation-outcome` / `buzz:delegation-tokens` tags from an
 * agent-signed outcome event. */
export type DelegationOutcomeEvent = {
  eventId: string;
  runId: string;
  outcome: DelegationOutcome;
  tokensUsed: number | null;
  createdAt: number;
};

/** The relay-signed notice word carried by a `buzz:delegation-notice` tag. */
export type DelegationNoticeWord =
  | "approved"
  | "failed"
  | "expired";

/** A relay-signed summary/failure/expiry notice for one delegation. */
export type DelegationNoticeEvent = {
  eventId: string;
  delegationId: string;
  notice: DelegationNoticeWord;
  content: string;
  createdAt: number;
};

/** Derived display state for a `DelegationSummaryCard` chip. */
export type DelegationDisplayState =
  | "approved"
  | "running"
  | "delivered"
  | "failed"
  | "expired";

export type ApproveDelegationResult = {
  eventId: string;
};
