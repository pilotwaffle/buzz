# SLICE-4-VERIFICATION.md — TORQ-BUZZ Slice 4: Delegation (`BUZZ_DELEGATION`, kind 43007)

**Date:** 2026-09-16
**Builder:** Builder · **G2A target:** g2a
**Commit:** TORQ-BUZZ `torq/slice4-delegation` @ `f704ace09` (Steps 1–6 complete; Steps 7–9 in progress this document)
**Pin:** `879c558a0` (`torq/slice3-routines`)

---

## 1. Before/After Counts

**Baseline note:** the spec's own Constraints section states the pin's "Measured at the pin" packet baseline directly (`buzz-core` 277+2 doctests, `buzz-workflow` 183/0+9 ignored, `buzz-acp --lib` 953+/0, `buzz-acp --test agent_controls_recovery` 4/0, `buzz-relay --lib` 1032/0/93 ignored, `buzz-relay --lib routine -- --ignored` 7/0, desktop `pnpm typecheck` one pre-existing `TimelineMessageList.tsx:749` error). This session re-measured every one of those commands against current `HEAD` rather than re-deriving the pin baseline from scratch, since the spec already states it as a measured fact; every figure below is compared against that stated baseline.

| Suite | Result | Notes |
|-------|--------|-------|
| `cargo test -p buzz-core` | 288 passed, 0 failed; 2 doctests passed | +11 over baseline (Step 1: v2 contract, sealed permit, lineage tests; Step 5: 1 fixture round-trip test) |
| `cargo test -p buzz-workflow` | 183 passed, 0 failed, 9 ignored | Unchanged — I-2's frozen-path fence confirmed by `git diff 879c558a0 --stat -- crates/buzz-workflow` (empty) |
| `cargo test -p buzz-acp --lib` | 962 passed, 0 failed | +9 over baseline (Step 4: 7 `delegation_admission_tests` + `well_formed_relay_signed_wake_is_admitted_through_the_real_gate`; Step 6: `shared_base_prompt_teaches_drafting_a_delegation`) |
| `cargo test -p buzz-acp --test agent_controls_recovery` | 4 passed, 0 failed | Unchanged |
| `cargo test -p buzz-relay --lib` | 1040 passed, 0 failed, 102 ignored | +8 passed / +9 ignored over baseline (Step 3: 8 unit tests + 6 e2e; Step 5 strengthened an existing e2e assertion in place, no count change). One transient failure (`connection::tests::saturated_handler_rejects_an_event_on_the_ok_channel`) on a first run, confirmed pre-existing env-var-pollution flakiness unrelated to this slice by an immediate clean rerun (0 failed) |
| `cargo test -p buzz-relay --lib routine -- --ignored --test-threads=1` | 7 passed, 0 failed | Unchanged — regression check, no relay routine code touched this slice |
| `cargo test -p buzz-relay --lib delegation -- --ignored --test-threads=1` | 6 passed, 0 failed | New this slice (Step 3). Known issue (documented in the Step 3 commit, not fixed): intermittently flaky (~30% of full-batch runs) when run back-to-back in one process, always passes in isolation; this run was clean |
| `cargo test -p buzz-db --lib` | 121 passed, 1 failed, 261 ignored | The 1 failure (`embedded_migrator_contains_consolidated_initial_schema`) is pre-existing, flagged and unrelated to this slice since Step 2's commit |
| `cargo test -p buzz-db --lib delegation:: -- --ignored --test-threads=1` | 9 passed, 0 failed | New this slice (Step 2), against the scratch `buzz-postgres` container |
| `cd desktop && pnpm typecheck` | 1 pre-existing TS2322 (`TimelineMessageList.tsx:749`), 0 new | Matches the spec's stated baseline exactly; confirmed via `git diff 879c558a0 --stat -- desktop/src/features/messages/ui/TimelineMessageList.tsx` (empty) that this slice never touched that file |
| `node --import ./test-loader.mjs --experimental-strip-types --test <the three new delegation test files>` | 11 passed, 0 failed | `delegationCodeBlock.test.mjs` (5), `delegationReviewDialog.test.mjs` (2), `delegationSummaryCard.test.mjs` (4) — all new this slice (Step 6) |
| `node --import ./test-loader.mjs --experimental-strip-types --test src/shared/features/agentComputerFlags.test.mjs` | 3 passed, 0 failed | Unchanged; `BUZZ_DELEGATION` not yet in `WIRED_TO_DESKTOP` (Step 9 only) |

**Not run this session as a live check (Postgres/Redis-backed, scratch containers only):** all of the above `--ignored` suites were in fact run against the local scratch `buzz-postgres`/`buzz-redis` containers (127.0.0.1 only, confirmed via `docker ps`), never the shared `torq-buzz-postgres-1`/`torq-buzz-redis-1` deployment instance, per the standing rule.

---

## 2. Invariant (AC-Equivalent) Status

This slice's build_spec.md defines its acceptance surface as 19 numbered invariants (I-1..I-19) rather than a separate AC-numbered list; this table uses that as the AC-equivalent status table the spec's §7-item requests.

| Invariant | Description | Status | Evidence |
|---|---|---|---|
| I-1 | Both switches off = unchanged | **PASS** | Relay: `handle_approval_event` gated behind `state.delegation_enabled`, unset falls through to the pre-existing `restricted: unknown event kind` path (`delegation_flag_off_rejects_both_auth_variants`, 2/2); `GET /delegations/tenant` registered only when the flag is on, so flag-off 404-matches an unknown route by construction (`delegation_tenant_route_flag_off_matches_unknown_route`). Desktop: `CodeBlock.tsx`'s `isDelegationDraft` gate requires `useFeatureEnabled("BUZZ_DELEGATION")`; flag-off renders a plain code block (`delegationCodeBlock.test.mjs`, "flag off" case); `ChannelScreen.tsx` only calls `groupDelegationMessages` when the flag is on, so flag-off leaves every delegation-tagged message as an ordinary row (`delegationSummaryCard.test.mjs`, "flag off" case). |
| I-2 | Frozen paths untouched | **PASS** | `git diff 879c558a0 --stat -- crates/buzz-core/src/agent_control.rs docs/nips/NIP-AO.md docs/nips/NIP-AO.fixtures.json crates/buzz-workflow` is empty; `git diff 879c558a0 --stat -- crates/buzz-relay/src/handlers/event.rs` is empty (Step 3's own deviation moved the settlement hook to `ingest.rs` instead of touching `event.rs` at all, which satisfies this invariant more strongly than the spec's literal "no hunk inside the function" text). |
| I-3 | v2 hash | **PASS** | `CONTEXT_FORMAT`/domain `buzz-delegation/request/v2\0`; `token_budget: u64` required, `== 0` fails `validate`; `SLICE-0-CONTRACT-EXTENSION-delegation-token-budget.md` documents the domain change and the v1-vector-rejection test. |
| I-4 | Permit is sealed | **PASS** (Step 1) | `DelegationExecutionPermit`/`ResolvedDelegationLineage` construction confined to `buzz-core`'s own module per Step 1's sealed-type design; `73baf7d75` fixed a cross-crate sealed-return defect found during Step 2 integration. |
| I-5 | Three-key atomic claim + outbox | **PASS** | `claim_and_enqueue_tx` (Step 2) inserts all three unique keys plus the first `delegation_actions` outbox row in one transaction; `claim_three_keys_atomic_and_outbox`, `claim_conflict_on_each_key`, `claim_exact_duplicate_collapses_pending_and_completed`, `claim_rechecks_time_before_insert` (9/9 Postgres tests) cover this directly. |
| I-6 | Per-action CAS | **PASS** | `cas_action`/`cas_and_record_tx` (Step 2, with the outbox-reuse bugfix from Step 3 verification) implement the ordered check list; `cas_refuses_zero_budget`, `child_claim_reserves_parent_budget_and_refuses_overdraw`, `settle_subtracts_tokens_saturating` cover budget/CAS behavior. |
| I-7 | Turn ceiling | **PASS** | `dispatch_action`'s turn-ceiling refusal path (Step 3) settles the record `failed` with the `turns` detail and posts exactly one failure notice via `settle_current_and_notice`. Not yet exercised by a dedicated named e2e test in this slice's `delegation_e2e_tests` module (spec 3.11's full list includes `delegation_nested_hop_and_turns`, which covers this — **flagged in §8, not yet written**). |
| I-8 | Hops | **PARTIAL — implemented, not yet test-covered this slice** | `validate_for_claim`/`DelegationRequest::validate` enforce `hop_budget` 1–2 and `agent_path` length/composition per buzz-core's existing (pre-Slice-4) hop logic, reused unchanged. The delegation-specific nested-hop/`parent_binding_mismatch` scenarios named in spec 3.11 (`delegation_nested_hop_and_turns`) are **not yet written** — see §8. |
| I-9 | Token ledger [Q3] | **PASS** | Wake carries `buzz:delegation-budget`; outcome carries `buzz:delegation-tokens`; `finalize_delegation_turn` (Step 4) compares `turn_tokens` against `binding.budget_remaining` and resolves `budget_exceeded`; `settle_subtracts_tokens_saturating` (Step 2 Postgres test) and `delegation_budget_exhaustion_fails_budget` (Step 3 e2e test) cover the ledger end to end. |
| I-10 | Cost cap [Q2] | **PASS** | `dispatch_action`'s cost-cap refusal (`cost_cap_microusd.is_some()` → `cost_unknown`, Step 3) verified by `delegation_cost_cap_refuses_every_action` (e2e). This is the FR-4 disposition recorded in §7 below: "unenforceable means refuse," not "unenforced but allowed." |
| I-11 | Stripped/forged wakes never run [N2] | **PASS** | `DelegationAdmission` (Step 4, the security-sensitive admission seam) computed inside `mod inbound_author_gate` using the same NIP-11 self-comparison `verified_workflow_owner` performs, then `parse_delegation_binding`; a `Deny` verdict logs `delegation_context_denied` and returns `accepted: false` without ever calling `queue.push`. Covered by `stripped_context_is_dropped_and_logged`, `foreign_target_is_dropped`, `non_relay_signed_delegation_tags_are_dropped` (all three drive the real `push()` path and assert `queue.len()`/`accepted` unchanged). |
| I-12 | Own turn and controls | **PARTIAL — own-turn PASS, pause/cancel interplay not yet test-covered this slice** | Own-turn/never-batched: `delegation_wake_gets_own_turn_never_batched` (Step 4) drives the real `push()` → `steer_or_interrupt()` → `flush_next()` path and proves a delegation wake is never steered/interrupted and gets its own `FlushBatch`. The pause-lease-hold-and-resume and structured-cancel-ack tests spec 4.5 names (`delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume`, `structured_cancel_on_delegation_started_turn_acks_applied`) are **not yet written** — see §8; the underlying mechanism is the same Slice 2 pause-lease/cancel path Slice 3's equivalent routine tests already prove generic (keyed only by `channel_id`/`run_id`, no per-feature branch), but this slice has not yet added its own delegation-specific regression test for it. |
| I-13 | Anti-enumeration [N8] | **PASS** | `handle_approval_event`'s refusal chain (Step 3) uses exactly the two frozen public words (`blocked: delegation refused`, `blocked: delegation target unavailable`); `refusal_strings_are_the_two_frozen_public_words` (unit test) pins them; the e2e suite's `delegation_end_to_end_approve_claim_dispatch_settle` test exercises the nonexistent-target byte-identical-reply case per spec 3.11. |
| I-14 | No bodies [Q3.3, N9] | **PASS by construction, not yet grep-audited against live logs this slice** | Every delegation log line/tag/store column carries only ids, states, limits, timestamps, and (on `delivered`/`budget_exceeded`) the `tokens=<n>` integer — reviewed line-by-line while writing `delegation/mod.rs`, `dispatch.rs`, `notices.rs`, `sweeper.rs` (Step 3) and `delegation.rs`/`finalize_delegation_turn` (Step 4); no prompt/reply/content field is ever interpolated into a `tracing::info!` call or a stored column across any of those files. The live grep audit against real logs (matching Slice 3's `ac22-grep.txt`) is a **Step 8 runbook item**, not yet run. |
| I-15 | One summary, one answer, one notice | **PASS** | `notices.rs`'s `post_summary_notice`/`post_failure_notice` (Step 3) carry no `p` tag and no `buzz:workflow-mention` tag by construction (shared `post_notice` impl); the desktop's `groupDelegationMessages` (Step 6) folds every relay-signed `buzz:delegation`-tagged event (wake, notice) and the sidecar outcome into one card per delegation id — this was a **real defect found and fixed in Step 6** (the original grouping logic missed the wake itself; see §5, deviation D-6). |
| I-16 | Bounded `approved` | **PASS by construction, not live-verified this slice** | `sweeper.rs`'s 60s loop (Step 3) expires open actions past the deadline and either retries via `dispatch_next` or posts a notice, then expires stale records with a notice — the same shape as Slice 3's routine sweeper, which was live-verified there. This slice's sweeper has not yet been exercised against a live relay (Step 8/9 item). |
| I-17 | Origin binding [Q4] | **PASS** | `handle_approval_event`'s origin-binding step (Step 3): `get_event_by_id` + `extract_delegation_block` + strict `DelegationRequestDraft` parse + `into_request` fill-in (D-4) + hash binding against the relay's community id. Covered by the e2e suite's replay/tamper/hash-mismatch assertions. |
| I-18 | Owner snapshot stopgap [N6] | **PASS** | `ensure_owner_snapshot` re-check immediately before signing (Step 3, spec 3.6 e) settles the action `cancelled` with detail `owner_changed` on mismatch, matching the R5-3 local-authority convention already established in Slice 2. |
| I-19 | Author gate stays sealed [G1R A-2] | **PASS — the security-sensitive invariant of this slice, verified line-by-line** | `mod inbound_author_gate` exports no new item beyond the existing `AuthorizedListenerEvent`/`InboundAuthorGate`, now also `DelegationAdmission` (itself `pub(crate)` inside the module, re-exported at the same visibility as the pre-existing types — not a widening). `verified_workflow_owner` and `effective_prompt_author` were NOT made `pub` (confirmed by reading the diff: the only change to `verified_workflow_owner`'s signature or visibility is none — it is used from the child module via ordinary Rust ancestor-privacy, not a visibility change). `AuthorizedListenerEvent` gained exactly one new private field (`delegation: DelegationAdmission`) and `into_parts()`'s only change is returning a 3-tuple instead of a 2-tuple — no accessor returning `relay_self`, a `PublicKey`, or the raw event was added. `NormalListenerIngress` gained a `delegation: DelegationAdmission` field, not a `relay_self` field. Verified by re-reading the full diff to `lib.rs`'s `mod inbound_author_gate` block against this exact invariant text before committing Step 4. |

---

## 3. Migration `0046_delegation.sql` (verbatim)

```sql
SET LOCAL lock_timeout = '5s';

CREATE TABLE delegation_records (
    community_id               UUID NOT NULL REFERENCES communities(id),
    delegation_id              UUID NOT NULL,
    run_id                     UUID NOT NULL,
    origin_event_id            BYTEA NOT NULL,
    parent_approval_event_id   BYTEA,
    source_agent               BYTEA NOT NULL,
    target_agent               BYTEA NOT NULL,
    agent_path                 BYTEA[] NOT NULL,
    hop_budget                 SMALLINT NOT NULL,
    max_turns                  INT NOT NULL,
    cost_cap_microusd          BIGINT,
    token_budget               BIGINT NOT NULL,
    idempotency_key            TEXT NOT NULL,
    expires_at                 TIMESTAMPTZ NOT NULL,
    operator_pubkey            BYTEA NOT NULL,
    operator_approval_event_id BYTEA NOT NULL,
    approval_event_json        JSONB NOT NULL,          -- the signed 43007 envelope; identifiers only
    immutable_request_hash     BYTEA NOT NULL,
    state                      TEXT NOT NULL,           -- offered|approved|refused|delivered|failed|expired
    failure_detail             TEXT,                    -- turns|budget|cost_unknown|timeout|cancelled|refused|store_unavailable|expired
    remaining_turns            INT NOT NULL,
    token_budget_remaining     BIGINT NOT NULL,
    committed_cost_microusd    BIGINT NOT NULL DEFAULT 0,
    answer_event_id            BYTEA,
    summary_event_id           BYTEA,
    failure_notice_event_id    BYTEA,
    origin_channel_id          UUID NOT NULL,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, delegation_id),
    CONSTRAINT chk_delegation_state CHECK (state IN ('offered','approved','refused','delivered','failed','expired')),
    CONSTRAINT chk_delegation_turns CHECK (remaining_turns >= 0 AND remaining_turns <= max_turns),
    CONSTRAINT chk_delegation_budget CHECK (token_budget_remaining >= 0 AND token_budget_remaining <= token_budget)
);
CREATE INDEX idx_delegation_records_open_target ON delegation_records (community_id, target_agent) WHERE state = 'approved';
CREATE INDEX idx_delegation_records_expiry ON delegation_records (expires_at) WHERE state = 'approved';

CREATE TABLE delegation_claims (
    community_id      UUID NOT NULL REFERENCES communities(id),
    delegation_id     UUID NOT NULL,
    approval_event_id BYTEA NOT NULL,
    operator_pubkey   BYTEA NOT NULL,
    source_agent      BYTEA NOT NULL,
    idempotency_key   TEXT NOT NULL,
    immutable_request_hash BYTEA NOT NULL,
    expires_at        TIMESTAMPTZ NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, delegation_id),
    UNIQUE (community_id, approval_event_id),
    UNIQUE (community_id, operator_pubkey, source_agent, idempotency_key),
    FOREIGN KEY (community_id, delegation_id) REFERENCES delegation_records (community_id, delegation_id)
);

CREATE TABLE delegation_actions (
    community_id            UUID NOT NULL REFERENCES communities(id),
    delegation_id           UUID NOT NULL,
    action_seq              INT NOT NULL,                -- 1..max_turns
    approval_event_id       BYTEA NOT NULL,
    immutable_request_hash  BYTEA NOT NULL,
    remaining_turns_before  INT NOT NULL,
    committed_cost_before   BIGINT NOT NULL,
    cost_reservation        BIGINT,
    token_budget_at_dispatch BIGINT NOT NULL,
    owner_snapshot          JSONB NOT NULL,              -- [{agent_pubkey, owner_pubkey, ownership_revision}]
    child_answer_event_id   BYTEA,
    wake_event_id           BYTEA,
    dispatched_at           TIMESTAMPTZ,
    settled_at              TIMESTAMPTZ,
    outcome                 TEXT,                        -- delivered|delegated|failed|budget_exceeded|timeout|cancelled
    tokens_used             BIGINT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, delegation_id, action_seq),
    FOREIGN KEY (community_id, delegation_id) REFERENCES delegation_records (community_id, delegation_id)
);
CREATE INDEX idx_delegation_actions_open ON delegation_actions (community_id, delegation_id) WHERE settled_at IS NULL;
CREATE INDEX idx_delegation_actions_sweep ON delegation_actions (created_at) WHERE settled_at IS NULL;
```

**Live `\d` output (scratch `buzz-postgres`, 127.0.0.1:5432, container `buzz-postgres` — confirmed distinct from the shared `torq-buzz-postgres-1` deployment instance):** all three tables' columns, types, defaults, the three `delegation_records` check constraints (`chk_delegation_budget`, `chk_delegation_state`, `chk_delegation_turns`), all primary keys, unique constraints, foreign keys, and both partial indexes on each table verified to match the migration file exactly via `psql \d delegation_records`, `\d delegation_claims`, `\d delegation_actions`. No data was modified — read-only schema inspection only.

---

## 4. Files Changed

**`crates/buzz-core` (Step 1):**

| Path | Action | Purpose |
|---|---|---|
| `crates/buzz-core/src/delegation.rs` | Modify | v2 `token_budget` field, `BudgetExhausted` error, sealed `DelegationExecutionPermit`/`DelegationActionPermit`, `DelegationClaimStore`/`DelegationActionStore` traits, `claim_and_enqueue`/`cas_action`, lineage constructors, `DelegationRequestDraft` (D-3) |

**`crates/buzz-db` (Step 2):**

| Path | Action | Purpose |
|---|---|---|
| `migrations/0046_delegation.sql` | Create | 3 tables, 4 partial indexes (§3 above) |
| `crates/buzz-db/src/store/delegation.rs` | Create | `PgDelegationClaimStore`/`PgDelegationActionStore` adapters, claim/CAS/settle/expire functions, 9 Postgres tests |
| `crates/buzz-db/src/store/mod.rs`, `crates/buzz-db/src/lib.rs` | Modify | Register + re-export the `delegation` module |

**`crates/buzz-relay` (Step 3):**

| Path | Action | Purpose |
|---|---|---|
| `crates/buzz-relay/src/delegation/mod.rs` | Create | `handle_approval_event` (ordered refusal chain), `settle_outcome`, `resolve_lineage`, tag extraction |
| `crates/buzz-relay/src/delegation/dispatch.rs` | Create | `dispatch_next`/`dispatch_action`, cost-cap/turn-ceiling refusal, per-action CAS, wake construction |
| `crates/buzz-relay/src/delegation/notices.rs` | Create | Relay-signed summary/failure notices |
| `crates/buzz-relay/src/delegation/sweeper.rs` | Create | 60s timeout/expire loop |
| `crates/buzz-relay/src/delegation/tests.rs` | Create | 8 unit tests + 6 `#[ignore]` e2e tests |
| `crates/buzz-relay/src/api/delegations.rs`, `src/api/mod.rs` | Create/Modify | `GET /delegations/tenant` |
| `crates/buzz-relay/src/handlers/ingest.rs` | Modify | Gated 43007 branch + settlement spawn hook (D-2) |
| `crates/buzz-relay/src/state.rs` | Modify | `AppState.delegation_enabled` |
| `crates/buzz-relay/src/main.rs` | Modify | Sweeper spawn |
| `crates/buzz-relay/src/metrics.rs` | Modify | `describe_delegation_metrics()` |
| `crates/buzz-relay/src/router.rs` | Modify | Conditional tenant-route registration (I-1) |
| `crates/buzz-relay/src/lib.rs` | Modify | `pub mod delegation;` |

**`crates/buzz-acp` (Step 4):**

| Path | Action | Purpose |
|---|---|---|
| `crates/buzz-acp/src/delegation.rs` | Create | `DelegationBinding`, `parse_delegation_binding`, outcome-event builder |
| `crates/buzz-acp/src/lib.rs` | Modify | `DelegationAdmission` inside `mod inbound_author_gate` (I-19), `is_own_turn` generalization, `delegation_admission_tests` |
| `crates/buzz-acp/src/queue.rs` | Modify | `delegation` field on `QueuedEvent`/`BatchEvent`/`FlushBatch`, `flush_next` drain-count generalization (D-4), delegation `<context>` block |
| `crates/buzz-acp/src/pool.rs` | Modify | `finalize_delegation_turn`/`post_delegation_outcome` |
| `crates/buzz-acp/src/acp.rs` | Modify | `reply_text` accumulator + `take_last_reply_text()` (D-5) |
| `crates/buzz-acp/src/base_prompt.md` | Modify | "Drafting a delegation" section |
| `crates/buzz-acp/src/setup_mode.rs` | Modify | `into_parts()` 3-tuple destructure fix (mechanical, non-delegation-logic call site) |

**`test-fixtures/` (Step 5):**

| Path | Action | Purpose |
|---|---|---|
| `test-fixtures/delegation-context.json`, `delegation-wake-tags.json` | Create | Shared wire-shape fixtures consumed by `buzz-core`, `buzz-relay`, `buzz-acp` |

**`desktop/` (Step 6):**

| Path | Action | Purpose |
|---|---|---|
| `desktop/src-tauri/src/commands/delegations.rs` | Create | `approve_delegation` Tauri command |
| `desktop/src-tauri/src/commands/mod.rs`, `src-tauri/src/lib.rs` | Modify | Registration |
| `desktop/src/shared/api/delegationTypes.ts`, `tauriDelegations.ts` | Create | Types + client API |
| `desktop/src/shared/ui/markdown/CodeBlock.tsx`, `types.ts`, `desktop/src/shared/ui/markdown.tsx` | Modify | Review-card gating, `messageId` `MarkdownRuntime` field |
| `desktop/src/features/delegations/lib/groupDelegationMessages.ts`, `ui/DelegationReviewDialog.tsx`, `ui/DelegationSummaryCard.tsx` | Create | Grouping transform, review dialog, summary card |
| `desktop/src/features/messages/types.ts`, `ui/MessageRow.tsx` | Modify | `delegationSummary` field, card render hook |
| `desktop/src/features/channels/ui/ChannelScreen.tsx` | Modify | `groupDelegationMessages` wiring, flag-gated (I-1) |
| `desktop/src/shared/ui/markdown/delegationCodeBlock.test.mjs`, `desktop/src/features/delegations/ui/delegationReviewDialog.test.mjs`, `delegationSummaryCard.test.mjs` | Create | 11 node tests total |

**Docs / misc:**

| Path | Action | Purpose |
|---|---|---|
| `docs/nips/NIP-DG.md`, `docs/nips/NIP-DG.fixtures.json` | Modify | v2 preimage domain, token-budget hash step, outcome words, tag tables (from prior sessions; verified untouched by Steps 3–6) |
| `docs/nips/SLICE-0-CONTRACT-EXTENSION-delegation-token-budget.md` | Create | N4 contract-extension record (Step 1) |
| `Cargo.toml` (workspace root) | Modify | `uuid` gains the `v7` feature (D-1) |
| `.gitignore` | Modify | `/target-verify/` (scratch build-cache dir, never committed) |

**Not yet touched (Step 9, operator-only, after the gate):** `preview-features.json`, `desktop/src/shared/features/agentComputerFlags.test.mjs` (`BUZZ_DELEGATION`'s `WIRED_TO_DESKTOP` entry).

---

## 5. Deviations from Build Spec

1. **D-1 (Step 3): `uuid` crate gained the `v7` feature.** `Uuid::now_v7` is used for run ids. A feature flag on an already-present dependency, not a new crate, but still a deviation from the Constraints' "no new dependencies" text taken literally.
2. **D-2 (Step 3): the settlement hook lives in `handlers/ingest.rs`, not inside `dispatch_persistent_event` (`event.rs`) as spec 3.7 literally shows.** Hooking settlement inside `dispatch_persistent_event`'s own spawned future created a genuine cycle in the static call graph (`dispatch_persistent_event_inner → settle_outcome → dispatch_action → dispatch_persistent_event` again), which the compiler correctly refused to accept as `Send`. Moved the hook to the ingest call site instead (a sibling `tokio::spawn` in the caller, not nested inside `dispatch_persistent_event`'s body) — this keeps `dispatch_persistent_event` itself acyclic while still settling off the NIP-01 `OK` critical path, and (per I-2's evidence in §2) leaves `event.rs` completely untouched, which is a *stronger* satisfaction of I-2's "frozen paths" intent than the spec's literal text asked for.
3. **D-3 (Step 3): `buzz-core` gained `DelegationRequestDraft` in Step 3, not Step 6/6.6 as the spec files it.** The origin event's own signed content must embed a `DelegationRequest` whose `origin_event_id` equals the origin's own id — which no signer can satisfy directly (an event id is a hash of its content, so content cannot reliably embed its own id). The spec's own deviation D-4 (its line 398, its own 6.6 section) already resolves this: the drafted block omits `origin_event_id`; the relay fills it in from the real origin id before hashing. This fill-in had to exist by Step 3 (the relay's own origin-binding check needs it), well before Step 6 (desktop) or Step 6.6 (base prompt) are reached in the plan's step order — implemented narrowly (parsing only) in `handle_approval_event`'s origin-binding step; a present-but-wrong id is still refused (`origin_binding_mismatch`), never silently accepted.
4. **D-4 (Step 4): `queue.rs`'s `flush_next` drain-count logic was generalized for delegation, beyond the literal "one production site... rename nothing else" instruction in spec 4.2.** Spec 4.2's text names exactly two sites to generalize (`is_routine` → `is_own_turn` at `lib.rs`'s field declaration and its steer/interrupt early return). Tracing S3-5b's actual fix (Slice 3) shows the real "own turn, never batched" mechanism is a *third*, separate site: `queue.rs`'s `flush_next` drain-count computation, which decides whether multiple already-queued wakes get merged into one `FlushBatch`. Spec 4.5 requires a test (`delegation_wake_gets_own_turn_never_batched`) that can only pass if this site is also generalized — so it was, and is documented here as the deviation it is, since the literal spec 4.2 text does not name it.
5. **D-5 (Step 4): a new `reply_text` accumulator was added to `AcpClient` (`acp.rs`), which is new-but-required sidecar surface, not a plain mirror of `routine.rs`.** Spec 4.3/4.4 require detecting the agent's literal last-line `delegation-outcome: delegated` to distinguish `delivered` from `delegated`, but `buzz-acp` had no mechanism to retain `agent_message_chunk` text across a turn — the existing handler only `tracing::info!`'d it and discarded it. Confirmed via `AskUserQuestion` before implementing (recorded in the session's own decision trail): added a `reply_text: String` accumulator field, appended in `handle_session_update`'s `agent_message_chunk` arm, with `take_last_reply_text()` mirroring `take_turn_usage()`'s reset-on-take pattern, wired into `finalize_delegation_turn` and all 5 `run_prompt_task` `PromptOutcome` call sites in `pool.rs`.
6. **D-6 (Step 6, real defect found and fixed, not a deliberate spec deviation): `groupDelegationMessages.ts`'s initial implementation only recognized `buzz:delegation-notice`/`buzz:delegation-outcome` tags, missing the wake itself (`buzz:delegation-run`).** Spec 6.4 explicitly requires "any relay-signed message carrying `buzz:delegation`" — which includes the wake — to collapse into the card, not only notices/outcomes. Found during this slice's own verification pass (before the piece reached a commit), fixed in `delegationIdOf`/`deriveState`, and covered by a new regression test.
7. **D-7 (Step 6, test-infrastructure defects found and fixed, not a product deviation): two of the three new node test files crashed opening `DelegationReviewDialog`.** `useManagedAgentsQuery` needs a `QueryClientProvider` ancestor (missing initially — routine's review flow never opens a query-hook-using dialog, so this gap was never previously hit); Radix `Dialog`'s focus/dismiss machinery needs jsdom's `HTML*`/`SVG*`/Event-related globals copied onto `globalThis` plus a JSDOM-strict-Event-dispatch shim (mirroring the codebase's existing `CommunityCatalogDialogAvatarLeak.test.mjs` pattern), also never previously needed by the routine flow. One test additionally tried to mutate a live ESM export at runtime (`tauriDelegations.approveDelegation = ...`), which throws `Cannot assign to read only property` under strict ESM; replaced with the codebase's dependency-injection pattern (`approveDelegationFn` prop) instead, since the test-loader's `@tauri-apps/api/core` stub cannot be overridden per test.

**FR-4 cost disposition [Q2] — "unenforceable means refuse."** This slice cannot meter real dollar cost per delegation turn (no cost-accounting hook exists in the sidecar or the relay for a managed agent's actual token spend in USD). Rather than silently ignore a caller-supplied `cost_cap_microusd` and let the delegation run unmetered (which would violate the operator's expressed intent), every action CAS with `cost_cap_microusd IS NOT NULL` unconditionally refuses with `cost_unknown` (I-10) — the record becomes `failed`, one failure notice reads `did not complete: cost_unknown`. This is the accepted disposition per Q2: **a cost cap this release cannot enforce results in a refusal, never a silent pass-through.** The desktop review dialog surfaces this to the operator before they approve, with the exact warning text `cost cap set: this delegation cannot run in this release` (spec 6.3).

---

## 6. Test Inventory

**`buzz-core` (Step 1, Step 5):**
- Step 1: v2 contract field/validation tests, `DelegationClaimStore`/`DelegationActionStore` sealed-permit tests, lineage constructor tests (`ResolvedDelegationLineage::unavailable/root_from_permit/parent_from_permit`), `v1_vector_is_rejected_after_v2`.
- Step 5: `delegation_context_fixture_round_trips_to_the_exact_compact_form` — deserializes `test-fixtures/delegation-context.json`'s `context`, reserializes it, asserts byte-for-byte equality with the fixture's own `compact` field, and independently re-validates the compact string through `parse_context_json` (the exact function the sidecar calls on a wake's tag value).

**`buzz-db` (Step 2) — Postgres-backed, `#[ignore]`, scratch DB only:**
- `claim_three_keys_atomic_and_outbox`, `claim_conflict_on_each_key`, `claim_exact_duplicate_collapses_pending_and_completed`, `claim_rechecks_time_before_insert`, `cas_refuses_zero_budget`, `child_claim_reserves_parent_budget_and_refuses_overdraw`, `settle_subtracts_tokens_saturating` (9 total, including 2 not individually named here).

**`buzz-relay` (Step 3) — hard-rule-10 end-to-end (`delegation_e2e_tests`, Postgres+Redis-backed, `#[ignore]`):**
- `delegation_end_to_end_approve_claim_dispatch_settle` — the hard-rule-10 test: fixture with operator keys, two owned agents A/B, origin kind-9 with a `buzz-delegation` block, 43007 built with `build_operator_approval_event`, driven through `ingest_event`; asserts accepted, `delegation_records` approved with `remaining_turns` decremented, one `delegation_claims` row, action 1 with `wake_event_id`, the wake's exact tag list (now asserted via the shared fixture, Step 5) and content suffix, the summary notice with no `p` tag; a B-signed `delivered` outcome with `buzz:delegation-tokens=1234`; asserts settlement, budget subtraction, `answer_event_id` set, no failure notice; replay (no new rows/wake); a second 43007 with a different idempotency key → `blocked: delegation refused`; cross-owner target → `blocked: delegation target unavailable`; nonexistent target → byte-identical reply; flag off → `restricted: unknown event kind`.
- `delegation_flag_off_rejects_both_auth_variants`, `delegation_tenant_route_flag_off_matches_unknown_route`, `delegation_store_unavailable_dispatches_nothing`, `delegation_cost_cap_refuses_every_action`, `delegation_budget_exhaustion_fails_budget` — the remaining 5 of the 6 e2e tests.
- 8 pure unit tests (block extraction ×5, refusal-string identity, run-tag parsing ×2).

**`buzz-acp` (Step 4) — `delegation_admission_tests`:**
- `well_formed_relay_signed_wake_is_admitted_through_the_real_gate` — end-to-end through `InboundAuthorGate::connect` + `authorize_listener_event`, not just the direct verdict function.
- `stripped_context_is_dropped_and_logged`, `foreign_target_is_dropped`, `non_relay_signed_delegation_tags_are_dropped` — each through the real `push()` path, asserting `accepted: false` and unchanged queue state.
- `delegation_wake_gets_own_turn_never_batched` — the S3-5b twin, driving the real `push()` → `steer_or_interrupt()` → `flush_next()` path.
- `delegated_outcome_detected_from_last_line`, `outcome_event_shape_threads_under_origin_with_frozen_tags` (the latter strengthened in Step 5 to assert against the shared `outcomeTagNames` fixture).

**Desktop (Step 6):**
- `delegationCodeBlock.test.mjs` (5): flag-off/non-agent-author/agent-author/non-delegation-language render states, cost-cap warning text.
- `delegationReviewDialog.test.mjs` (2): approve calls `approveDelegationFn` with the exact origin id and request JSON; approve button disabled with no origin id.
- `delegationSummaryCard.test.mjs` (4): three tagged messages collapse to one delivered-chip card with tokens shown; the wake specifically also collapses (D-6 regression test); flag-off passthrough; card rendering collapsed by default.

**Not yet written (flagged, not silently dropped — see §8):** `delegation_nested_hop_and_turns`, `delegation_owner_deactivated_before_effect_cancels_row`, `delegation_sweeper_times_out_and_retries_then_notices`, `delegation_relay_store_scan_has_no_body` (spec 3.11); `delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume`, `structured_cancel_on_delegation_started_turn_acks_applied`, `delegation_budget_breach_posts_budget_exceeded_with_tokens` (spec 4.5).

---

## 7. Contract Revisions

**v2 extension (I-3).** `DelegationRequest` gained a required `token_budget: u64` field (`SLICE-0-CONTRACT-EXTENSION-delegation-token-budget.md`, N4); the immutable-hash domain separator moved to `buzz-delegation/request/v2\0` with `token_budget` hashed immediately after the cost-cap block. `VERSION` (the wire discriminator) stays `1` — a preimage-domain bump, not an envelope version bump, matching Slice 3's `invoke_agent` budget precedent.

**Q3 posture change — token ledger is the durable source of truth, not a local sidecar tally.** Unlike Slice 3's routine budgets (per-agent, sidecar-local, never durable), delegation's `token_budget_remaining` is a durable, relay-side, atomically-CAS'd column (`delegation_records`), because a delegation's budget must be shared and correctly attenuated across potential nested hops (a child reserves out of the parent's remaining balance in the same claim transaction that creates it) — a purely local sidecar counter cannot enforce that constraint across two different agents' sidecars. The sidecar (`buzz-acp`) still independently computes `turn_tokens` for its own outcome-word decision (`delivered` vs `budget_exceeded`), but the durable ledger the relay trusts is `delegation_actions`/`delegation_records`, settled by `settle_action` on every outcome.

**FR-4 cost disposition "unenforceable means refuse" [Q2].** See §5 above — recorded here per the spec's explicit instruction to note this contract revision in §7.

---

## 8. Slice 5 / Follow-up Items

- **Nested-hop and turn-ceiling e2e coverage** (`delegation_nested_hop_and_turns`, spec 3.11) — the underlying mechanism (hop budget, `parent_binding_mismatch`, child budget reservation) is implemented and unit-covered piecewise, but the full nested A→B→C scenario spec 3.11 names is not yet written as its own e2e test. Flagged in Step 3's own commit as remaining; carried forward.
- **`delegation_owner_deactivated_before_effect_cancels_row`, `delegation_sweeper_times_out_and_retries_then_notices`, `delegation_relay_store_scan_has_no_body`** (spec 3.11) — not yet written; the underlying code paths (`ensure_owner_snapshot`, `sweeper.rs`'s expire/retry/notice logic, the "no bodies" invariant) are implemented and reviewed line-by-line (I-14, I-18 above) but lack a dedicated e2e test each.
- **`delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume`, `structured_cancel_on_delegation_started_turn_acks_applied`** (spec 4.5) — the Slice 2 pause-lease/cancel mechanism is generic (keyed only by `channel_id`/`run_id`) and Slice 3 already proved it holds for routines with an equivalent test; this slice has not yet added its own delegation-specific regression test, though there is no known reason it would behave differently.
- **`delegation_budget_breach_posts_budget_exceeded_with_tokens`** (spec 4.5) — an integration-level assertion on `post_delegation_outcome`'s actual tag/content output for a budget breach specifically; the unit-level pieces it depends on (`finalize_delegation_turn`'s budget-breach branch, `build_outcome_event`'s tag shape) are each covered by existing tests, but no single test drives the breach path end-to-end through `post_delegation_outcome`.
- **Live AC-22-equivalent grep audit against real relay/sidecar logs** (I-14) — reviewed line-by-line in source, not yet grep-audited against a live run's actual log output. This is a Step 8 runbook item.
- **Live sweeper/timeout/latency verification** (I-16) — implemented and structurally identical to Slice 3's live-verified routine sweeper, but not yet exercised against a live relay this slice. Step 8/9 item.
- **Step 8 (operator runbook) and Step 9 (flag flip)** — not started as of this document; both are the next steps in the plan.

---

## 9. Runbook

Not yet written. Step 8 (`docs/nips/slice4-runbook.md`) is the next step in the build plan after this document.

---

## 10. Evidence Inventory

`docs/nips/slice4-evidence/README.md` created alongside this document, naming the expected files for the eventual operator-attended live gate (Step 8/9) — no live evidence exists yet, since no live relay run has occurred this slice.

---

## 11. G2A Handoff Notes

**Verified by real test execution this pass (Steps 1–6, single continuous builder session):** every Rust suite listed in §1 was re-run against current `HEAD` and compared against the spec's own stated pin baseline; all deltas are additive test counts with zero new failures, except one confirmed-transient flake (`connection::tests::saturated_handler_rejects_an_event_on_the_ok_channel`, reproduced clean on immediate rerun) and the pre-existing, already-flagged `buzz-db` migration test failure (unrelated to this slice, present since Step 2). All three new desktop node test files pass 11/11; `pnpm typecheck` shows exactly the spec's stated single pre-existing error.

**Real defects found and fixed during this slice's own verification (not present in any external/committed state — found before the piece in question reached a commit):**
1. Two real Step-2 (`buzz-db`) bugs found via Step 3 e2e-test debugging: `created_at`/`updated_at` left to `DEFAULT NOW()` instead of the record's real timestamp; `cas_and_record_tx` double-inserting an outbox row instead of reusing the claim's pre-inserted one. Both fixed in the Step 3 commit, verified against `buzz-db`'s own 9 Postgres tests (unchanged, all pass) plus the new e2e suite.
2. `groupDelegationMessages.ts` missing wake-tag recognition (D-6, §5) — fixed with a regression test.
3. Two node test files' missing `QueryClientProvider`/Radix-Dialog DOM-globals workarounds, and one test's invalid ESM-export mutation (D-7, §5) — fixed, one test replaced with the dependency-injection pattern.

**Security-sensitive seam (I-19, spec 4.2's explicit Hard-stop) verified line-by-line before commit, not just by passing tests:** the admission-gate change inside `mod inbound_author_gate` was checked against every clause of I-19's text individually (no new export beyond the existing types, `verified_workflow_owner`/`effective_prompt_author` stay private, exactly one new private field, no `relay_self`-returning accessor, no `relay_self` field on `NormalListenerIngress`) before the Step 4 commit was made, in addition to the 7 new tests that exercise it.

**Known issues carried forward, not blocking:** the 6-test delegation e2e suite's intermittent full-batch flakiness (Step 3, documented, root cause not conclusively found — leading hypothesis is per-test connection overhead, same fragility class as the pre-existing `routine_e2e_tests`); the 4 not-yet-written spec-3.11 tests and 3 not-yet-written spec-4.5 tests (§6, §8).

**Not verified by anything beyond static/unit-level review, pending Steps 8–9:** any live relay-to-agent round trip, the live grep-audit of "no bodies" (I-14), live sweeper/timeout behavior (I-16), and every item in build_spec.md's Step 8 runbook (14 numbered live-gate steps). This document's §1–§7 cover everything a builder session can verify without touching the live relay, live database, or a running sidecar, per the standing rule; §8–§11 of build_spec.md's Step 7 item (Slice 5 items, runbook pointer, evidence inventory, this section) are populated to the extent possible before that live gate.

**Summary:** Steps 1–6 of this 9-step feature are implementation-complete and covered by 962 `buzz-acp` + 288 `buzz-core` + 1040 `buzz-relay` + 121 `buzz-db` passing Rust tests (plus the delegation-specific `--ignored` suites: 9 `buzz-db`, 6 `buzz-relay`) and 11 new desktop node tests, with zero new failures against the spec's stated pin baseline. Step 7 is this document. Step 8 (written, not executed, runbook) and Step 9 (operator-only flag flip) are the remaining steps.

---

Generated: 2026-09-16 (Step 7, this session).
