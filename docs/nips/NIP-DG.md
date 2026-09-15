# NIP-DG: Operator-approved agent delegation (v2)

Status: Slice-0 inert contract, extended by Slice 4 (`BUZZ_DELEGATION`). Relay ingest and execution are feature-gated behind the relay env `BUZZ_DELEGATION` and the desktop flag of the same name; both default off.

Delegation transfers work, never authority. The source, target, and every agent in the bounded ancestry path must resolve in the current tenant to the operator who signed the approval. The target still runs through its ordinary authority and approval gates.

## Wire records

`DelegationRecord`, `DelegationExecutionContext`, and `DelegationApproval` are defined in `buzz_core::delegation`. All security-bearing JSON structs reject unknown and duplicate fields. Record/context data contains identifiers and constraints only; the task body remains in the encrypted `origin_event_id` event.

The immutable request hash is lowercase SHA-256 over this exact binary preimage:

1. ASCII `buzz-delegation/request/v2` plus one NUL byte.
2. The server-resolved community UUID as its 16 RFC 4122 network-order bytes. It is an external trusted hash input and never comes from the wire request.
3. Delegation UUID as its 16 RFC 4122 network-order bytes.
4. Parent-approval presence as one byte (`0` absent, `1` present), followed when present by the parent event id using the string encoding in the next step.
5. Each string as an unsigned 64-bit big-endian UTF-8 byte length followed by those bytes: origin event id, source agent, and target agent.
6. Agent-path count as one unsigned byte, then each path pubkey using the same 64-bit length-prefixed string encoding.
7. Hop budget as one unsigned byte; max turns as unsigned 32-bit big-endian.
8. Cost cap as a one-byte presence marker (`0` absent, `1` present), followed when present by unsigned 64-bit big-endian micro-USD.
9. Token budget as unsigned 64-bit big-endian (Slice 4, `DelegationRequest::token_budget`; required, zero is rejected before hashing), then idempotency key using the string encoding above, then expiry as unsigned 64-bit big-endian Unix seconds.

Mutable lifecycle state, approval id, answer id, and record timestamps are excluded. `NIP-DG.fixtures.json` contains the required cross-language golden hash vector. A v1-shaped request (no `token_budget`) is rejected by strict parse under v2; no v1 record exists in any store, so no migration is required (`docs/nips/SLICE-0-CONTRACT-EXTENSION-delegation-token-budget.md`).

The ordered `agent_path` makes cycle rejection enforceable only when it is bound to authoritative lineage. Direct delegation is `[source, target]` with no parent approval. A second hop is `[root, source, target]` and must name the current parent approval. At validation, the server supplies an opaque lineage token from the currently executing durable run; it binds community, run/delegation identity, approval id, request hash, signer, expiry, and the already-validated parent path. The child must use the same tenant/signer, remain within the parent expiry, and extend that path by exactly one target. A fresh direct record therefore cannot erase active ancestry to evade a cross-record cycle. Entries must be unique, the final two entries must be source and target, the default hop budget is 1, and the hard maximum is 2.

## Approval envelope

An approval is an operator-signed regular event of kind `43007`, with strict JSON content `DelegationApproval` and exactly these tags:

- `d`: delegation UUID
- `e`: encrypted origin event id
- `p`: target agent pubkey
- `request`: immutable request hash
- `expiration`: Unix-seconds expiry

The validator rechecks the complete event id and Schnorr signature, signer, content, tags, request binding, and deadline. Because Schnorr verification is CPU-bound, async relay handlers must run this validation on their blocking-work executor. `now == expires_at` is expired. Kind `43007` is deliberately distinct from workflow approval kind `46030`, whose token-based contract cannot bind this request. Kind `43007` remains inert until a feature-gated relay handler is added.

## Runtime authority and replay

Owner and parent-lineage facts never come from the wire context. The relay must resolve each path agent's current owner and monotonic ownership revision inside the current tenant, and all owners must equal the approval signer. Every owner or visibility transition must advance that revision, including an A -> B -> A transition. `ResolvedDelegationLineage` has no public field-level constructor: `ResolvedDelegationLineage::unavailable()` yields the fail-closed state; `root_for_run` and `root_from_permit`/`parent_from_permit` mint the root/parent states only from a [`DelegationExecutionPermit`], which is itself sealed (below). `root_for_run` additionally requires a `RootProof`, obtainable only from `DelegationClaimStore::prove_no_open_parent` inside the claim transaction, so root-ness is proven under that transaction's own snapshot, never asserted by a caller. Unavailable lineage fails closed against both the root and parent match arms of `validate_parent_lineage` (there is no wildcard success case). Missing, invisible, and wrong-owner targets share the same external refusal.

Validation is two-stage:

1. `validate_for_claim` returns an opaque statelessly validated context.
2. The durable transaction rechecks its own current time against `DelegationClaim::expires_at`, then atomically claims `(community_id, delegation_id)`, `(community_id, approval_event_id)`, and `(community_id, operator_pubkey, source_agent, idempotency_key)` **and commits a durable work/outbox row**; `claim_and_enqueue` (Slice 4) requires transaction time, freezes duplicate/replay behavior, and mints a `DelegationExecutionPermit` — the caller's proof that the claim actually ran — on exactly one outcome (`ClaimStoreOutcome::AcquiredAndEnqueued`). Every other outcome yields no permit. Claim markers are retained through the signed expiry at minimum.

An exact duplicate is collapsed only when the store proves the same work is durably pending or complete. Reusing any key with different binding data is a replay and is rejected. Claim-store failure fails closed. The target executor must never accept raw deserialized context or a caller-asserted claim outcome — only a `DelegationExecutionPermit` obtained from `claim_and_enqueue`.

`DelegationExecutionPermit`, `DelegationActionPermit`, `ResolvedDelegationLineage`, and `RootProof` are sealed: every field is private, none derives `Default`/`Clone`/`Copy`/`serde::Deserialize`, and the only constructors are `claim_and_enqueue`, `cas_action`, `ResolvedDelegationLineage::{unavailable, root_for_run, root_from_permit, parent_from_permit}`, and the `DelegationClaimStore`/`DelegationActionStore` trait methods a caller implements against its own durable transaction. A caller implements `DelegationClaimStore` (`claim_and_enqueue`, `prove_no_open_parent`, `reopen_live_permit`) and `DelegationActionStore` (`cas_and_record`) against its actual three-key claim, work/outbox rows, and per-action compare-and-swap; this crate never performs the I/O itself.

Before every target action, `validate_next_action` rechecks the server-resolved community, re-resolves every path owner and ownership revision, and checks current expiry, the durable remaining-turn counter, and cumulative committed cost (settled cost plus outstanding reservations). The context's `remaining_turns` is an immutable ceiling: durable state must be nonzero and no greater than that ceiling, while the opaque action token carries the exact durable pre-CAS counter. This permits sequential actions at N, N-1, and so on without permitting a counter increase above the approved context.

The opaque action token also carries the complete ordered owner/revision snapshot, community, delegation id, approval id, request hash, signed expiry, expected turn/cost state, and reservation. For a capped delegation, the dispatcher must know a conservative reservation before starting the action; unknown cost fails closed. Inside the transaction it must lock and re-resolve the path ownership rows, require their owner/revision snapshot to equal the token, recheck current time against the carried expiry, then compare-and-swap the exact claim identity and expected turn/cost state, require `token_budget_remaining > 0`, decrement one turn, add the reservation, and record both the action and authority snapshot in the outbox before dispatch. `cas_action` mints a `DelegationActionPermit` only on `ActionStoreOutcome::AppliedAndRecorded`; every other outcome (`ClaimConflict`/`StateConflict`/`AuthorityConflict` -> `action_conflict`, `BudgetExhausted` -> `budget_exhausted`, `StoreUnavailable` -> `authority_unavailable`) yields none. The outbox dispatcher must re-resolve the same owner/revision snapshot immediately before any external effect; a change cancels the row rather than dispatching it. A higher revision invalidates the snapshot even when the owner pubkey is again identical after an A -> B -> A transition. Afterward the runtime settles actual cost by releasing unused reservation; the provider/tool boundary must prevent actual cost from exceeding the reservation. If that bound cannot be enforced, the action cannot claim a hard cost cap and must be refused (Slice 4: every action on a capped delegation is refused `cost_unknown`, since no cost oracle exists yet). This prevents stale action tokens or concurrent actions from crossing expiry, surviving an ownership transfer, mutating another claim, overspending a cap, or overspending the token budget.

## Token budget (Slice 4)

`DelegationRequest::token_budget` is a required `u64` total budget for the delegation's lifetime, hashed into the immutable request as domain step 9 above. The wake to the target agent carries `["buzz:delegation-budget", <token_budget_remaining>]`; the agent-signed outcome carries `["buzz:delegation-tokens", <turn_total>]`. Settlement subtracts the turn's token usage from the remaining budget with saturating arithmetic (never below zero). A nested child's claim reserves `child.token_budget` out of the parent's `token_budget_remaining` inside the same claim transaction; `child.token_budget` exceeding the parent's remaining budget is refused (public message "delegation refused") and the parent's remaining budget is left unchanged. `remaining == 0` at the per-action compare-and-swap fails that action `budget_exhausted`; a turn outcome of `budget_exceeded` settles the delegation record `failed` with detail `budget`.

## Outcome words

Every target-executed action settles to one of: `delivered` (the agent completed the task and posted its ordinary reply), `delegated` (the agent posted a further `buzz-delegation` block and ends its turn without a final answer; the parent's continuation wake carries `buzz:delegation-child-answer`), `failed` (unrecoverable error, cancellation, or a turn-ceiling/parent-binding refusal — see the fixed failure-detail words below), or `budget_exceeded` (the turn's token usage exceeded the remaining budget). A delegation record's `failure_detail` is one of: `turns`, `budget`, `cost_unknown`, `timeout`, `cancelled`, `refused`, `store_unavailable`, `expired`.

## Wake and outcome tags (Slice 4, frozen for Slice 5)

| Tag | Where | Value |
|---|---|---|
| `buzz:delegation-run` | wake, outcome | run UUID |
| `buzz:delegation` | wake, notices | delegation UUID |
| `buzz:delegation-context` | wake | compact `DelegationExecutionContext` JSON |
| `buzz:delegation-budget` | wake | remaining tokens, decimal |
| `buzz:delegation-child-answer` | continuation wake | child outcome event id hex |
| `buzz:delegation-outcome` | outcome | `delivered\|delegated\|failed\|budget_exceeded` |
| `buzz:delegation-tokens` | outcome | turn total tokens, decimal |
| `buzz:delegation-notice` | relay notices | `approved\|failed` |

Content strings: wake `…\n\ndelegation-run: <run_id>` (+ `\nchild-answer: <id>` on a continuation); outcome `delegation run <run_id> <delivered|delegated|failed: <detail>|exceeded its token budget>`; failure notice `Delegation <id8> to <target8> did not complete: <detail>`. No tag or content string ever carries the origin task body, a prompt, or reply text — only identifiers, states, limits, timestamps, and the `tokens=<n>` integer.

The shared 44-case malicious-case manifest is `NIP-DG.fixtures.json`.
