import type { RelayEvent } from "@/shared/api/types";
import { invokeTauri } from "./tauri";

export async function decryptObserverEvent(
  event: RelayEvent,
): Promise<unknown> {
  return invokeTauri<unknown>("decrypt_observer_event", {
    eventJson: JSON.stringify(event),
  });
}

export async function buildObserverControlEvent(input: {
  agentPubkey: string;
  payload: unknown;
  /** Optional custom created_at (Unix seconds). When set, the signed event uses this timestamp. */
  createdAt?: number;
}): Promise<RelayEvent> {
  const eventJson = await invokeTauri<string>("build_observer_control_event", {
    agentPubkey: input.agentPubkey,
    payload: input.payload,
    createdAt: input.createdAt ?? null,
  });
  return JSON.parse(eventJson) as RelayEvent;
}
