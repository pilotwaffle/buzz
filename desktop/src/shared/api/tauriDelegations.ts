import { invokeTauri } from "@/shared/api/tauri";
import type { ApproveDelegationResult } from "@/shared/api/delegationTypes";

type RawApproveDelegationResult = {
  event_id: string;
};

/**
 * Build the operator approval for a drafted delegation, publish it, and
 * return the approval event's id. `originEventId` is the id of the message
 * containing the `buzz-delegation` block (the desktop fills this into the
 * request's `origin_event_id` before hashing — D-4).
 *
 * `requestJson` is the exact fenced-block content the agent posted, parsed
 * strictly on the Rust side against `DelegationRequest`.
 */
export async function approveDelegation(
  originEventId: string,
  requestJson: string,
): Promise<ApproveDelegationResult> {
  const raw = await invokeTauri<RawApproveDelegationResult>(
    "approve_delegation",
    { originEventId, requestJson },
  );
  return { eventId: raw.event_id };
}
