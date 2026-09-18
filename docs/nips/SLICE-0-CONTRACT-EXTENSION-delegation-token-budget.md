# Slice 0 Contract Extension — Delegation Token Budget

**Slice:** 4 (Delegation)
**Date:** 2026-09-15
**Contract:** `DelegationRequest` (`buzz_core::delegation`), NIP-DG

## Fields added

| Field | Type | Required | Validation |
|---|---|---|---|
| `token_budget` | `u64` | yes | Must be > 0 (`DelegationError::InvalidField("token_budget")` when zero) |

## Wire contract (`DelegationRequest`)

`token_budget` sits immediately after `cost_cap_microusd` and is required with `deny_unknown_fields`; missing it, or supplying `0`, is a parse/validate failure.

```json
{
  "delegation_id": "…",
  "origin_event_id": "…",
  "parent_approval_event_id": null,
  "source_agent": "…",
  "target_agent": "…",
  "agent_path": ["…", "…"],
  "hop_budget": 1,
  "max_turns": 8,
  "cost_cap_microusd": null,
  "token_budget": 50000,
  "idempotency_key": "…",
  "expires_at": 1800000300
}
```

## Hash domain change

`immutable_request_hash`'s domain separator changed from `buzz-delegation/request/v1\0` to `buzz-delegation/request/v2\0`, and `token_budget` (u64 big-endian) is hashed immediately after the cost-cap block, before the idempotency key. `DelegationRequest::VERSION` (the wire discriminator on `DelegationRecord`/`DelegationExecutionContext`/`DelegationApproval`) stays `1` — this is a preimage-domain bump, not a wire-format version bump, matching Slice 3's `invoke_agent` budget precedent of extending a field set without bumping the outer envelope version.

## Reason

A delegation with no bounded token budget is an unbounded cost: the target agent could run indefinitely under the operator's approval with no per-delegation ceiling. Slice 4 requires every approved delegation to carry a caller-chosen total token budget, enforced at the sidecar turn boundary and settled durably on the relay, with nested children reserving their budget out of the parent's remaining balance so a hop-2 delegation cannot exceed what its root approved.

## Compatibility

v1-shaped requests (no `token_budget`) are rejected by strict parse under v2 (`serde(deny_unknown_fields)` plus the new required field); the v1 golden hash vector no longer verifies (`v1_vector_is_rejected_after_v2`). No v1 record exists in any store — Slice 0 was inert (relay ingest and execution were feature-gated and unimplemented) and Slice 4 is the first slice to actually claim and dispatch a delegation — so no migration of existing rows is required.
