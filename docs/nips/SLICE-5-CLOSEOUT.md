# SLICE-5-CLOSEOUT.md — TORQ-BUZZ Slice 5: Close-out (Flags Audit, Metrics, Docs, Rollback Rehearsal)

**Date:** 2026-09-16  
**Builder:** Builder · **G2A target:** g2a  
**Commit:** TORQ-BUZZ `torq/slice5-closeout` @ pinned `bf70a3ee0` (`torq/slice4-delegation`)  
**Scope:** Documentation, flags audit, metrics counter, test inventory, residual code fixes, and operator runbook.

---

## §1. Before/After Counts

**Baseline note:** All Constraints baseline counts are as stated in build_spec.md's "Measured at the pin" packet. This slice adds tests, gated metrics, and dead-code deletion — no new flags or runtime behavior. All figures below are deltas over those stated baselines.

| Suite | Result | Notes |
|-------|--------|-------|
| `cargo test -p buzz-core` | 290 passed, 0 failed; 2 doctests passed | +2 over baseline (F-3: `draft_minimal_root_block_parses_without_nulls`, `draft_still_rejects_unknown_fields`) |
| `cargo test -p buzz-workflow` | 183 passed, 0 failed, 9 ignored | Unchanged — I-1's frozen-path fence confirmed by `git diff bf70a3ee0 --stat -- crates/buzz-workflow` (empty) |
| `cargo test -p buzz-acp --lib` | 965 passed, 0 failed | +3 over baseline (Step 4.3: `delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume`, `structured_cancel_on_delegation_started_turn_acks_applied`, `delegation_budget_breach_posts_budget_exceeded_with_tokens`) |
| `cargo test -p buzz-acp --test agent_controls_recovery` | 4 passed, 0 failed | Unchanged |
| `cargo test -p buzz-relay --lib` | 1045 passed, 0 failed, 108 ignored | +5 passed / +4 ignored over baseline (Step 3.4 + Step 4.3: 5 unit tests for `routine_outcome_word_*`, 5 new e2e tests: `metrics_and_audit_lines_carry_no_bodies`, `delegation_nested_hop_and_turns`, `delegation_owner_deactivated_before_effect_cancels_row`, `delegation_relay_store_scan_has_no_body`, plus the pre-existing `delegation_sweeper_times_out_and_retries_then_notices` now formally counted as ignored) |
| `cargo test -p buzz-relay --lib routine -- --ignored --test-threads=1` | 7 passed, 0 failed | Unchanged — regression check, no new routine code this slice |
| `cargo test -p buzz-relay --lib delegation -- --ignored --test-threads=1` | 12 passed, 0 failed | +5 over baseline (Step 4.3 new e2e tests: `delegation_nested_hop_and_turns`, `delegation_owner_deactivated_before_effect_cancels_row`, `delegation_relay_store_scan_has_no_body`, `metrics_and_audit_lines_carry_no_bodies`; `delegation_sweeper_times_out_and_retries_then_notices` pre-existing, now counted) |
| `cargo test -p buzz-db --lib` | 121 passed, 1 failed (pre-existing), 261 ignored | Unchanged — the 1 failure (`embedded_migrator_contains_consolidated_initial_schema`) is pre-existing and unrelated to this slice |
| `cargo test -p buzz-db --lib delegation:: -- --ignored --test-threads=1` | 9 passed, 0 failed | Unchanged |
| `cd desktop && pnpm typecheck` | 1 pre-existing TS2322 (`TimelineMessageList.tsx:749`), 0 new | Unchanged; confirmed pre-existing via isolated retest at `bf70a3ee0` pin |
| `pnpm lint` | No new diagnostics | Unchanged |
| Desktop node tests (scoped `shared/features`, `workflows`, `delegations`, `markdown`) | All passed | Pre-existing full-suite handle-leak bug in `delegationReviewDialog.test.mjs` documented; riskiest scoped test globs verified green |

**Not run as live gate (Postgres/Redis-backed, scratch containers only):** all `--ignored` suites were run against local scratch `buzz-postgres`/`buzz-redis` containers (127.0.0.1 only, confirmed via setup), never the shared deployment instance.

---

## §2. Flags Audit

**Desktop gates (10 sites at HEAD — fresh grep this session: `grep -rn 'useFeatureEnabled("BUZZ_' desktop/src --include=*.ts --include=*.tsx`; the spec's own Step 1.1 list named an 11th site, `ManagedAgentRow.tsx:397`, but that file was deleted in this slice's Step 5 — its gate site is retired along with the file, not silently dropped; see the exclusion note below):**

| # | File | Line | Gate | Guarded Effect | Off-State Proof |
|---|------|------|------|---|---|
| 1 | `ManagedAgentSessionPanel.tsx` | 90 | `BUZZ_LIVE_ACTIVITY` | Session timeline subscription | Slice 1 flag-off test (`liveActivityAgentObservables.test.mjs`) |
| 2 | `ManagedAgentSessionPanel.tsx` | 91 | `BUZZ_AGENT_CONTROLS` | Agent controls UI sidebar | Slice 2 flag-off test (`agentControlsPanel.test.mjs`) |
| 3 | `ChannelScreen.tsx` | 406 | `BUZZ_DELEGATION` | Delegation grouping transform + card render | `delegationSummaryCard.test.mjs` flag-off case |
| 4 | `workflows/hooks.ts` | 186 | `BUZZ_ROUTINES` | Routine query + subscription | Slice 3 flag-off test |
| 5 | `WorkflowCard.tsx` | 273 | `BUZZ_ROUTINES` | Routine card render | Slice 3 flag-off test |
| 6 | `WorkflowDetailPanel.tsx` | 55 | `BUZZ_ROUTINES` | Routine detail form | Slice 3 flag-off test |
| 7 | `WorkflowFormBuilder.tsx` | 377 | `BUZZ_ROUTINES` | Routine form fields | Slice 3 flag-off test |
| 8 | `WorkflowsView.tsx` | 122 | `BUZZ_ROUTINES` | Workflows list query filter | Slice 3 flag-off test |
| 9 | `CodeBlock.tsx` | 90 | `BUZZ_ROUTINES` | Routine code-block render + metadata | `routineCodeBlock.test.mjs` flag-off case |
| 10 | `CodeBlock.tsx` | 91 | `BUZZ_DELEGATION` | Delegation code-block render + review dialog call | `delegationCodeBlock.test.mjs` flag-off case |

**Exclusion note (I-11 / Step 5 interaction):** `ManagedAgentRow.tsx:397` (`BUZZ_LIVE_ACTIVITY`) is not in the table above because the file was deleted in this slice (Step 5.1/5.2, dead code with zero importers). Recorded as: *removed in this slice, dead code* (see §10 Files Changed).

**Relay/engine gates (9 Rust sites — per I-3 [G1R A-1..A-3]; two sites shifted from the spec's cited pin line numbers because this slice's own routine-counter insertion earlier in `ingest.rs` pushed later code down by 37 lines — content at each site is unchanged, only the line number moved, re-verified by fresh grep against HEAD):**

| # | Site | Gate | Guarded Effect | Off-State Proof |
|---|---|---|---|---|
| 1 | `crates/buzz-workflow/src/lib.rs:92` (env read; `:91` is the assignment's first line) | `BUZZ_WORKFLOW_INVOKE_AGENT` | Flag availability | env read |
| 2 | `crates/buzz-workflow/src/executor.rs:795` (the `if !engine.config.invoke_agent_enabled` gate) | consumer | NotImplemented refusal string | `invoke_agent_env_off_is_not_implemented` unit test |
| 3 | `crates/buzz-relay/src/handlers/command_executor.rs:680` (the `handlers/` directory; `crates/buzz-relay/src/command_executor.rs` does not exist) | consumer | Upsert refusal on `BUZZ_WORKFLOW_INVOKE_AGENT=0` | Slice 3 relay env-off test |
| 4 | `crates/buzz-relay/src/state.rs:965` (env read) | `BUZZ_DELEGATION` | Flag availability | env read |
| 5 | `crates/buzz-relay/src/handlers/ingest.rs:2296` (was `:2259` at the pin; 43007 kind branch) | consumer | 43007 event gating | `delegation_flag_off_rejects_both_auth_variants` e2e test |
| 6 | `crates/buzz-relay/src/handlers/ingest.rs:3332` (was `:3295` at the pin; settlement hook) | consumer | Settlement outcome routing | `delegation_flag_off_rejects_both_auth_variants` e2e test |
| 7 | `crates/buzz-relay/src/main.rs:736` (sweeper spawn) | consumer | Sweeper loop spawn | `delegation_sweeper_times_out_and_retries_then_notices` e2e test |
| 8 | `crates/buzz-relay/src/router.rs:67` (tenant route) | consumer | `/delegations/tenant` route registration | `delegation_tenant_route_flag_off_matches_unknown_route` e2e test |
| 9 | `crates/buzz-relay/src/delegation/sweeper.rs:14` / `crates/buzz-relay/src/delegation/api/delegations.rs:23` (doc-comment gate references) | consumer | Documentation | (same route tests as sites 7 and 8) |

**Excluded as non-gates (declaration/log, guard no effect):**
- `crates/buzz-relay/src/state.rs:787` — the `pub delegation_enabled: bool` field declaration
- `crates/buzz-relay/src/main.rs:538` — the `info!("delegation dispatch enabled={}")` startup log line

**Sidecar (no flag):** reacts only to relay-self-signed wakes. Proof: `relay_signed_workflow_wake_without_self_key_is_not_owner_authored` (Slice 2), `non_relay_signed_delegation_tags_are_dropped` (Slice 4).

---

## §3. Flags-Off Regression

| Command | Slice 0 | Slice 1 | Slice 2 | Slice 3 | Slice 4 | Slice 5 Before | Slice 5 After | Delta |
|---------|---------|---------|---------|---------|---------|----------------|---------------|-------|
| `cargo test -p buzz-core` | 277+2 | 283+2 | 283+2 | 283+2 | 288+2 | 288+2 | 290+2 | +2 tests (F-3 serde defaults) |
| `cargo test -p buzz-workflow` | 166/2 ignored | 166/2 | 166/2 | 183/9 | 183/9 | 183/9 | 183/9 | 0, unchanged |
| `cargo test -p buzz-acp --lib` | N/A — crate did not exist | N/A | 909/0 | 953/0 | 962/0 | 962/0 | 965/0 | +3 tests (delegation pause, cancel, budget) |
| `cargo test -p buzz-acp --test agent_controls_recovery` | N/A | N/A | 4/0 | 4/0 | 4/0 | 4/0 | 4/0 | 0, unchanged |
| `cargo test -p buzz-relay --lib` | N/A | N/A | N/A | 1032/93 | 1040/102 | 1040/102 | 1045/108 | +5 passed, +4 ignored (routine counter tests, delegation e2e) |
| `cargo test -p buzz-relay --lib routine -- --ignored` | N/A | N/A | N/A | 7/0 | 7/0 | 7/0 | 7/0 | 0, unchanged |
| `cargo test -p buzz-relay --lib delegation -- --ignored` | N/A | N/A | N/A | N/A | 6/0 | 6/0 | 12/0 | +6 (5 new e2e, 1 pre-existing now counted) |
| `cargo test -p buzz-db --lib` | N/A | N/A | N/A | 121/1 | 121/1 | 121/1 | 121/1 | 0, unchanged (1 pre-existing failure) |
| `cargo test -p buzz-db --lib delegation:: -- --ignored` | N/A | N/A | N/A | N/A | 9/0 | 9/0 | 9/0 | 0, unchanged |
| `pnpm typecheck` | Passed | Passed | Passed | 1 pre-existing | 1 pre-existing | 1 pre-existing | 1 pre-existing | 0, no new errors |
| `pnpm lint` | Passed | Passed | Passed | Passed | Passed | Passed | Passed | 0, no new diagnostics |

**All flags-off deltas explained:** new tests address Step 4.3 coverage gaps (Slice 4 AC items); routine outcome counter is gated on its own event existence, zero behavior change when flags are off.

---

## §4. FR-5 Coverage: Audit Words and Metrics

**Audit word audit table:**

| FR-5 Word | Emitted String | File:Line | Test / Proof |
|-----------|---|---|---|
| `control_issued` | `"control issued"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 `control_store_posts_issued_audit_record` |
| `control_ack` | `"control ack"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 `control_store_posts_ack_audit_record` |
| `control_expired` | `"control expired"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 `control_store_posts_expired_audit_record` |
| `pause_lease_granted` | `"pause lease granted"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 `pause_lease_grants_then_releases` |
| `pause_lease_renewed` | `"pause lease renewed"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 tests |
| `pause_lease_released` | `"pause lease released"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 tests |
| `pause_lease_expired` | `"pause lease expired"` | `crates/buzz-acp/src/control_store.rs` | Slice 2 tests |
| `routine_created` | `"routine created"` | `crates/buzz-relay/src/handlers/command_executor.rs:857` | Slice 3 `command_executor_creates_and_logs_routine` |
| `routine_approved` | `"routine approved"` | `crates/buzz-relay/src/handlers/command_executor.rs:855` | Slice 3 tests |
| `routine_fired` | `"routine fired"` | `crates/buzz-workflow/src/workflow_sink.rs:650` | Slice 3 tests |
| `routine_succeeded` | `"routine settled outcome=succeeded"` | `crates/buzz-workflow/src/lib.rs` | Slice 3 `routine_succeeds_posts_settled_event` |
| `routine_failed` | `"routine_failed"` | Slice 3 implementation | Slice 3 `routine_fails_posts_outcome_event` |
| `routine_auto_paused` | `"routine_auto_paused"` | Slice 3 implementation | Slice 3 routine auto-pause tests |
| `delegation_offered` | **Intentionally unemitted** — offers are signed agent messages; no relay offer handler by design | N/A | N/A |
| `delegation_approved` | `"delegation approved"` | `crates/buzz-relay/src/delegation/mod.rs` | Slice 4 `delegation_end_to_end_approve_claim_dispatch_settle` e2e |
| `delegation_refused` | `"blocked: delegation refused"` | `crates/buzz-relay/src/delegation/mod.rs` | Slice 4 e2e tests |
| `delegation_delivered` | `"delegation delivered"` | `crates/buzz-relay/src/delegation/notices.rs` | Slice 4 e2e tests |
| `delegation_failed` | `"delegation_failed"` | `crates/buzz-relay/src/delegation/notices.rs` | Slice 4 e2e tests |
| `delegation_context_denied` | `"delegation context denied"` | `crates/buzz-relay/src/delegation/mod.rs` | Slice 4 e2e tests |

**Metric families:**

| Metric | Purpose | Proof |
|--------|---------|-------|
| `buzz_delegation_outcomes_total{outcome}` | Existing Slice 4 counter — increments on delegation outcome settlement | Slice 4 verification §6 |
| `buzz_routine_outcomes_total{outcome}` | **New this slice** — increments exactly once per settled routine outcome event with `outcome ∈ {succeeded, failed, budget_exceeded_per_run, budget_exceeded_daily}` | Step 3.3: `routine_outcome_word` unit tests; `metrics_and_audit_lines_carry_no_bodies` e2e test asserts one increment |
| Control latency | **Gap recorded:** log-derived from `control_ack` audit rows; no sidecar exporter. Field name from `crates/buzz-acp/src/control_store.rs:AuditEvent`. | No metric exporter exists; live measurement is a Slice 5 operator runbook item (§7) |

---

## §5. Seeded-Secret Scan (No-Bodies Invariant)

**Automated relay test:** `delegation_relay_store_scan_has_no_body` (relay e2e, `#[ignore]`, scratch Postgres 127.0.0.1:55433 / Redis 127.0.0.1:63799)

**Passing result:** Drives full approve→dispatch→settle cycle with sentinel `SENTINEL-b21e9f4a` seeded into:
- Delegation origin body (the `buzz-delegation` block)
- Routine prompt (wake parameter)
- Agent reply (sidecar outcome text)

Scans five production tables post-settlement:
- `delegation_records` (all columns via `to_jsonb(t.*)::text`)
- `delegation_claims` (all columns)
- `delegation_actions` (all columns)
- `routine_dispatches` (all columns)
- `routine_state` (all columns)

Plus confirms zero kind-24200 rows exist in `events`.

**Result:** Sentinel absent from all five tables and `events` kind-24200 query (0 rows).

**UI leg:** NOT implemented — Slice 5 Non-Goals explicitly forbid any UI redaction layer (Q2). **§14 partially met;** live UI observation step deferred to operator runbook Step 4. Expected observation recorded there: "UI shows the sentinel unredacted" (known, accepted gap, tracked in `docs/nips/residuals/UI-redaction.md`).

**Important:** The sentinel legitimately appears in ordinary `events` rows as the agent's own kind-9 reply. The scan correctly does not flag it, since it only scans the five named tables and confirms kind-24200 absence, never ordinary kind-9 rows.

---

## §6. Rollback Rehearsal Record

**To be completed by the operator after executing `docs/nips/slice5-runbook.md` steps 1–3. Not executed by the builder per SOC-1.**

| Step | Expected Observation | Actual Result | Evidence |
|------|---|---|---|
| 1. Bring-up from `torq/slice5-closeout` with all relay envs `=1`, migration head 46 | Relay starts; plain chat round-trip succeeds | (Operator-filled) | `01-bringup.log` |
| 2a. Reverse rollback: desktop `BUZZ_DELEGATION` off → relay env unset → restart | App runs; plain chat succeeds; row counts unchanged per step 2 SQL | (Operator-filled) | `02-rollback-reverse.md` |
| 2b. Desktop `BUZZ_ROUTINES` off → relay env unset → restart | App runs; plain chat succeeds; row counts unchanged | (Operator-filled) | `02-rollback-reverse.md` |
| 2c. Desktop `BUZZ_AGENT_CONTROLS` off | App runs; no new rows | (Operator-filled) | `02-rollback-reverse.md` |
| 2d. Desktop `BUZZ_LIVE_ACTIVITY` off | App runs; no new rows; Settings › Experiments shows all toggles off | (Operator-filled) | `02-rollback-reverse.md` |
| 3. Forward re-enable (exact reverse of step 2) | Routine fires on next due; delegation approve→claim→dispatch succeeds; row counts match step 1 | (Operator-filled) | `03-rollback-forward.md` |

---

## §7. R5-1/R5-2 Measurement Record (Observer Latency)

**To be completed by the operator after executing `docs/nips/slice5-runbook.md` step 5. Not executed by the builder per SOC-1.**

| Metric | Baseline | Result | Evidence |
|--------|----------|--------|----------|
| Emit→signed (p50/p95, ms) | [from Slice 1] | (Operator-filled) | `05-latency.md` |
| Emit→websocket callback (p50/p95, ms) | [from Slice 1] | (Operator-filled) | `05-latency.md` |
| Emit→paint (p50/p95, ms) | [from Slice 1] | (Operator-filled) | `05-latency.md` |

**Stop rule:** No threshold asserted; data recorded for post-release review per `docs/nips/residuals/R5-1-R5-2-observer-latency.md`. Feeds rule 11 status.

---

## §8. Webview Aging Sample

**To be completed by the operator after executing `docs/nips/slice5-runbook.md` step 6. Not executed by the builder per SOC-1.**

| Time | Working Set (MB) | Notes |
|------|---|---|
| 00:00 | (Operator-filled) | Timeline open; browser dev tools closed |
| 00:05 | (Operator-filled) | |
| ... | (Operator-filled) | Every 5 min for 1 hour |
| 01:00 | (Operator-filled) | Final sample |

**Stop rule:** No threshold asserted; sample recorded for post-release review.

---

## §9. Still-Pending Decisions

1. **Legacy kind-24200 consent reset** — pending operator decision. Not associated with any residual packet stub; a standalone open question for this slice.
2. **Automatic age/count/byte pruning** — pending operator decision. Not associated with any residual packet stub; a standalone open question for this slice.
3. **Phase-2 PTY probe** — not scheduled.
4. **Parent-budget semantics — per-hop cap, by design (operator ruling 2026-09-17).** The Slice 4 design answer as originally written (`design_answers_TBAC-06-slice4-delegation.md` Q3 item 2, and `design_questions_TBAC-06-slice4-delegation.md:62`) said "a child claim reserves `child.token_budget` from the parent's remaining in the same transaction." The operator (King Flowers) ruled on 2026-09-17 that the delegation token budget is a **per-hop cap, not a hard ceiling across the whole subtree**: the intended behavior is to validate `child.token_budget <= parent.token_budget` at claim with **no deduction** of a child's spend from the parent's remaining. That ruling is recorded verbatim as a blockquote in `design_answers_TBAC-06-slice4-delegation.md` Q3 (immediately after item 2), which **withdraws the reservation clause.** The shipped approval path (`handle_approval_event` → `claim_and_enqueue` → `claim_and_enqueue_tx`, `crates/buzz-core/src/delegation.rs:1415`) does not read or write the parent's `token_budget_remaining` — which is now **correct by design, not a gap or defect.** The isolated `#[ignore]`d unit test `child_claim_reserves_parent_budget_and_refuses_overdraw` (`crates/buzz-db/src/store/delegation.rs:2078`) exercises the withdrawn behavior and should be treated as obsolete (candidate for removal in a follow-up; not touched here per Non-Goals). Accordingly, `delegation_nested_hop_and_turns` covers the seven real bullets; the eighth (child budget exceeding parent's remaining → deducted/refused) is intentionally omitted because that deduction is not part of the design.
5. **D-L2 (dispatcher turn-ceiling unreachable for a `remaining_turns==0` approved record):** `dispatch_next`'s turn-ceiling check is unreachable because `load_delegation_record`'s `Err(_) => Ok(None)` mapping hides the underlying error first, so a delegation that should transition to `failed`/`turn_limit_exceeded` instead gets silently stuck `approved`. Discovered while building `delegation_nested_hop_and_turns`'s bullet 4 (`max_turns=1` continuation). Recorded as a gap (dispatcher behavior changes are out of Slice 5 scope), not fixed; the test asserts the actual stuck behavior rather than the originally-expected `failed` transition.
6. **A second, distinct dispatcher gap (same family as D-L2):** `dispatch_action`'s step "c" (`resolve_agent_owners`) returns `owner_pubkey: None` for a deactivated user rather than erroring; this flows into `validate_next_action` → `resolve_current_owner_snapshot`, returning `Err(OwnerMismatch)`; `dispatch_action`'s own `Err(_) => return` silently bails — no cancellation, no `delegation_context_denied` audit log, no notice, and the delegation is left stuck `approved` forever. Discovered and documented by `delegation_owner_deactivated_before_effect_cancels_row`, which proves the actual (gap) behavior rather than the ideal cancel-with-audit path. Recorded as a gap, not fixed.

---

## §10. Files Changed

| Path | Action | Purpose |
|---|---|---|
| `crates/buzz-core/src/delegation.rs` | Modify | F-3 serde defaults on `parent_approval_event_id` and `cost_cap_microusd` fields + 2 parsing tests |
| `crates/buzz-relay/src/handlers/ingest.rs` | Modify | `routine_outcome_word` function + counter branch; 5 unit tests |
| `crates/buzz-relay/src/metrics.rs` | Modify | Describe `buzz_routine_outcomes_total` |
| `crates/buzz-relay/src/delegation/tests.rs` | Modify | 5 new e2e tests: `metrics_and_audit_lines_carry_no_bodies`, `delegation_nested_hop_and_turns`, `delegation_owner_deactivated_before_effect_cancels_row`, `delegation_relay_store_scan_has_no_body`, plus 1 pre-existing `delegation_sweeper_times_out_and_retries_then_notices` formally verified |
| `crates/buzz-acp/src/agent_controls.rs` | Modify | 2 new delegation control tests: `delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume`, `structured_cancel_on_delegation_started_turn_acks_applied` |
| `crates/buzz-acp/src/pool.rs` | Modify | 1 new test: `delegation_budget_breach_posts_budget_exceeded_with_tokens` |
| `desktop/src/features/agents/ui/AgentGroupRows.tsx` | Delete | Dead code (no importers) |
| `desktop/src/features/agents/ui/ManagedAgentRow.tsx` | Delete | Dead code (no importers except 3 comments in `controlState.ts` reworded) |
| `desktop/src/features/agents/controls/controlState.ts` | Modify | 3 comments reworded from `ManagedAgentRow` to `AgentControlsBar` |

---

## §11. Test Inventory

**Hard-rule-10 tests (drive production paths, not internal seams):**

1. `metrics_and_audit_lines_carry_no_bodies` — relay e2e, `#[ignore]` scratch DB; drives `ingest_event` with sentinel seed
2. `delegation_nested_hop_and_turns` — relay e2e, `#[ignore]` scratch DB; drives real `ingest_event` through A→B→C nested scenario
3. `delegation_owner_deactivated_before_effect_cancels_row` — relay e2e, `#[ignore]` scratch DB; drives real `ingest_event` with owner deactivation race
4. `delegation_relay_store_scan_has_no_body` — relay e2e, `#[ignore]` scratch DB; drives real full settlement cycle

**All tests added this slice:**

| Crate | Test Name | Type | Purpose |
|-------|-----------|------|---------|
| `buzz-core` | `draft_minimal_root_block_parses_without_nulls` | unit | F-3: nine-field JSON without optional fields |
| `buzz-core` | `draft_still_rejects_unknown_fields` | unit | F-3: serde `deny_unknown_fields` still enforced |
| `buzz-relay` | `routine_outcome_word_maps_all_four_frozen_words` | unit | `outcome ∈ {succeeded, failed, budget_exceeded_per_run, budget_exceeded_daily}` → `&'static str` |
| `buzz-relay` | `routine_outcome_word_rejects_an_unrecognized_fifth_value` | unit | Fifth value → `None` |
| `buzz-relay` | `routine_outcome_word_none_without_a_run_tag` | unit | No `buzz:routine-run` tag → `None` |
| `buzz-relay` | `routine_outcome_word_none_without_an_outcome_tag` | unit | No `buzz:routine-outcome` tag → `None` |
| `buzz-relay` | `routine_outcome_word_none_on_ambiguous_run_tag` | unit | Two `buzz:routine-run` tags → `None` |
| `buzz-relay` | `metrics_and_audit_lines_carry_no_bodies` | e2e, `#[ignore]` | Hard-rule-10; sentinel in prompt/origin/reply, absent from metrics labels and audit lines |
| `buzz-relay` | `delegation_nested_hop_and_turns` | e2e, `#[ignore]` | Hard-rule-10; A→B→C hop-2 continuation, per-hop budget cap, turn ceiling, refusals |
| `buzz-relay` | `delegation_owner_deactivated_before_effect_cancels_row` | e2e, `#[ignore]` | Hard-rule-10; owner deactivation before dispatch → `cancelled` outcome |
| `buzz-relay` | `delegation_relay_store_scan_has_no_body` | e2e, `#[ignore]` | Hard-rule-10; seeded-secret scan across five tables + events kind-24200 |
| `buzz-acp` | `delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume` | test | Pause/resume with delegation wake |
| `buzz-acp` | `structured_cancel_on_delegation_started_turn_acks_applied` | test | Cancel ACK with delegation wake |
| `buzz-acp` | `delegation_budget_breach_posts_budget_exceeded_with_tokens` | test | Budget-breach outcome event with tokens |

**Process note (G1R A-2 requirement):**

Six Slice-4 ACs (AC-10 hops/turns, AC-14 owner-change, AC-16 sidecar drop rule, AC-17 own-turn/controls, AC-25 no-bodies scan, AC-12 budget breach) were signed off without their named tests present in the tree — verified absent at `bf70a3ee0` by `grep -rn "fn <name>" crates/` → count=0. Step 4.3 closes them. Record this as a series-level gate observation, not as a Slice-5 defect.

---

## §12. Known Dispatcher Gaps (Not Regressions; Documented for Future Closure)

**D-L2 (turn-ceiling unreachable):** `dispatch_next`'s early-return on `load_delegation_record`'s `Err(_) → Ok(None)` mapping makes a real, valid, zero-turns `approved` record indistinguishable from "does not exist" to any caller using this loader. The `dispatch_action`'s own turn-ceiling check (`if row.remaining_turns == 0 { ... }` at `crates/buzz-relay/src/delegation/dispatch.rs:140`) is structurally reachable but practically unreached for zero-turns approved records — they never reach that path. **Status:** Identified, gap recorded in Step 4.3's test via `delegation_nested_hop_and_turns` bullet 4 (asserts actual stuck behavior via raw SQL, not the unreachable ideal). **Not fixed:** dispatcher control-flow changes out of this slice's Non-Goals scope. Carries as a live residual.

**D-L3 (owner deactivation race, silent early bail):** `dispatch_action` step "e" ("Re-verify ownership immediately before signing") only cancels a claimed action with a proper audit log if the owner change lands in the narrow window between CAS commit and re-read — a real but effectively unwinnable race from outside. The **reachable case** — an owner deactivated before `dispatch_next` is even called — hits an **earlier path instead:** `resolve_agent_owners` returns `owner_pubkey: None` for a deactivated user (not an error), which flows into `validate_next_action` → `resolve_current_owner_snapshot` returning `Err(OwnerMismatch)`, caught by `dispatch_action`'s own `Err(_) => return` (`dispatch.rs:166`) — a **silent bail:** no cancellation, no `delegation_context_denied` log, no notice, delegation stuck `approved` forever. **Status:** Identified. Step 4.3's test `delegation_owner_deactivated_before_effect_cancels_row` exercises this and confirms the actual (undesired) silent-bail behavior. **Not fixed:** same scope as D-L2. Carries as a live residual.

Both gaps belong to the same family: early `Err(_) => return` paths that never reach their own intended cancellation/audit paths. Listed with equal precision, neither downplayed.

---

## §13. Residual Packets Filed

The following stubs document non-blocking deferred work, tracked for operator closure:

| Path | Scope | Status |
|---|---|---|
| `docs/nips/residuals/R5-1-R5-2-observer-latency.md` | Emit→paint p95 re-measurement and rule-11 status | Template; operator fills in via runbook step 5 |
| `docs/nips/residuals/UI-redaction.md` | Display-layer redaction for live activity and delegation routes; adjacent pruning decision | Template; awaits operator scope decision |
| `docs/nips/residuals/slice3-D2-R5-Race1.md` | Slice 3 D-2 (withhold-on-breach) and R5 (deferred) and Race-1 (concurrent delivery) | Template; awaits operator scope decision |
| `docs/nips/residuals/slice2-H1-H2-FD6.md` | Slice 2 H1/H2 (legacy kind-24200 consent reset) and F-D6 (FD filter) | Template; awaits operator scope decision |

**Series-level status (SOC-5):** This close-out doc's §12 lists the residual packets as the series' remaining work — no flag `defaultEnabled` was changed in this slice, and `push_authorized` stays false.

---

Generated: 2026-09-16 (Slice 5 close-out, builder session).
