# NIP-DG: Operator-approved agent delegation (v1)

Status: Slice-0 inert contract. Relay ingest and execution remain feature-gated and are not implemented here.

Delegation transfers work, never authority. The source, target, and every agent in the bounded ancestry path must resolve in the current tenant to the operator who signed the approval. The target still runs through its ordinary authority and approval gates.

## Wire records

`DelegationRecord`, `DelegationExecutionContext`, and `DelegationApproval` are defined in `buzz_core::delegation`. All security-bearing JSON structs reject unknown and duplicate fields. Record/context data contains identifiers and constraints only; the task body remains in the encrypted `origin_event_id` event.

The immutable request hash is lowercase SHA-256 over this exact binary preimage:

1. ASCII `buzz-delegation/request/v1` plus one NUL byte.
2. The server-resolved community UUID as its 16 RFC 4122 network-order bytes. It is an external trusted hash input and never comes from the wire request.
3. Delegation UUID as its 16 RFC 4122 network-order bytes.
4. Parent-approval presence as one byte (`0` absent, `1` present), followed when present by the parent event id using the string encoding in the next step.
5. Each string as an unsigned 64-bit big-endian UTF-8 byte length followed by those bytes: origin event id, source agent, and target agent.
6. Agent-path count as one unsigned byte, then each path pubkey using the same 64-bit length-prefixed string encoding.
7. Hop budget as one unsigned byte; max turns as unsigned 32-bit big-endian.
8. Cost cap as a one-byte presence marker (`0` absent, `1` present), followed when present by unsigned 64-bit big-endian micro-USD.
9. Idempotency key using the string encoding above; expiry as unsigned 64-bit big-endian Unix seconds.

Mutable lifecycle state, approval id, answer id, and record timestamps are excluded. `NIP-DG.fixtures.json` contains the required cross-language golden hash vector.

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

Owner and parent-lineage facts never come from the wire context. The relay must resolve each path agent's current owner and monotonic ownership revision inside the current tenant, and all owners must equal the approval signer. Every owner or visibility transition must advance that revision, including an A -> B -> A transition. `ResolvedDelegationLineage` deliberately has no public constructor in Slice 0: Slice 4 may mint it only from its private live execution permit after durable claim/enqueue, never from stateless validation. That runtime must positively distinguish a root run from unavailable lineage; unavailable state fails closed. Missing, invisible, and wrong-owner targets share the same external refusal.

Validation is two-stage:

1. `validate_for_claim` returns an opaque statelessly validated context.
2. The durable transaction rechecks its own current time against `DelegationClaim::expires_at`, then atomically claims `(community_id, delegation_id)`, `(community_id, approval_event_id)`, and `(community_id, operator_pubkey, source_agent, idempotency_key)` **and commits a durable work/outbox row**; `classify_claim_outcome` also requires transaction time and freezes duplicate/replay behavior but intentionally does not manufacture an execution permit. Claim markers are retained through the signed expiry at minimum.

An exact duplicate is collapsed only when the store proves the same work is durably pending or complete. Reusing any key with different binding data is a replay and is rejected. Claim-store failure fails closed. Slice 4 must create a private execution permit inside the durable transaction seam; the target executor must never accept raw deserialized context or a caller-asserted claim outcome.

The Slice-0 claim outcomes, dispositions, and classifier are crate-private
reference models used by the executable conformance matrix. They are not a
durable adapter and cannot prove that a transaction ran. Slice 4 MUST co-locate
the sealed claim-store adapter and the private execution permit with the actual
three-key claim and work/outbox rows.

Before every target action, `validate_next_action` rechecks the server-resolved community, re-resolves every path owner and ownership revision, and checks current expiry, the durable remaining-turn counter, and cumulative committed cost (settled cost plus outstanding reservations). The context's `remaining_turns` is an immutable ceiling: durable state must be nonzero and no greater than that ceiling, while the opaque action token carries the exact durable pre-CAS counter. This permits sequential actions at N, N-1, and so on without permitting a counter increase above the approved context.

The opaque action token also carries the complete ordered owner/revision snapshot, community, delegation id, approval id, request hash, signed expiry, expected turn/cost state, and reservation. For a capped delegation, the dispatcher must know a conservative reservation before starting the action; unknown cost fails closed. Inside the transaction it must lock and re-resolve the path ownership rows, require their owner/revision snapshot to equal the token, recheck current time against the carried expiry, then compare-and-swap the exact claim identity and expected turn/cost state, decrement one turn, add the reservation, and record both the action and authority snapshot in the outbox before dispatch. The transaction reports a crate-private reference `DelegationActionStoreOutcome`; `classify_action_outcome` accepts `AppliedAndRecorded` only to exercise the Slice-0 contract. Neither value is an execution permit. Slice 4 MUST instead co-locate a sealed action-store adapter and private permit with the actual compare-and-swap and outbox rows. The outbox dispatcher must re-resolve the same owner/revision snapshot immediately before any external effect; a change cancels the row rather than dispatching it. A higher revision invalidates the snapshot even when the owner pubkey is again identical after an A -> B -> A transition. Missing/mismatched claims, changed authority, and stale turn/cost state collapse to `action_conflict`, while an unavailable store is `authority_unavailable`. Afterward the runtime settles actual cost by releasing unused reservation; the provider/tool boundary must prevent actual cost from exceeding the reservation. If that bound cannot be enforced, the action cannot claim a hard cost cap and must be refused. This prevents stale action tokens or concurrent actions from crossing expiry, surviving an ownership transfer, mutating another claim, or overspending a cap.

The shared 40-case malicious-case manifest is `NIP-DG.fixtures.json`.
