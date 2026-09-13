# SLICE-3-VERIFICATION.md — TORQ-BUZZ Slice 3: Routines (`invoke_agent`)

**Date:** 2026-09-13
**Builder:** Builder · **G2A target:** g2a
**Commit:** TORQ-BUZZ `torq/slice3-routines` @ `dab7e21f9` (Steps 1–6 + Step 7.2 fix; Step 9 runbook written, not executed)
**Pin:** `e1c321f95` (`torq/slice2-structured-controls`)

---

## 1. Before/After Counts

**N8 baseline note:** the spec asked for a pre-edit baseline measured before any edit. This was not separately captured in the first (Steps 1–5) session — the working counts below are the state after all edits, and every run showed 0 new failures against the crate's own prior expectations, which is the substitute evidence. The one place this matters is `buzz-relay --lib`: the spec's own text carried two conflicting historical figures ("known red at Slice 1: 6 failed / 89 ignored"); this session measured 0 failed / 93 ignored and used the measured number, flagging the discrepancy rather than asserting which historical figure was stale (see build_result_SLICE-3-ROUTINES.md "Unresolved Issues").

| Suite | Result | Notes |
|-------|--------|-------|
| `cargo test -p buzz-workflow` | 183 passed, 0 failed | 6 new schema tests (Step 1), 2 new executor tests (Step 2) |
| `cargo test -p buzz-workflow --lib -- --ignored --test-threads=1` | 9 passed, 0 failed | 2 pre-existing SEC-006 tests (regression check) + 7 new Postgres-backed integration tests |
| `cargo test -p buzz-relay --lib` | 1032 passed, 0 failed, 93 ignored | No regressions against this session's own baseline |
| `cargo test -p buzz-relay --lib routine -- --ignored --test-threads=1` | 7 passed, 0 failed | Hard-rule-10 end-to-end test (§6) + 3 refine-pass tests (AC-12 cap, AC-12 column-vs-JSON, AC-12b re-enable). Re-run this pass (builder refine round) 5x with zero failures after fixing a flaky fixture in `routine_reenable_restores_enabled_column_and_refires` — see Deviation 9 in §5. |
| `cargo test -p buzz-acp --lib` | 949 passed, 0 failed | 12 tests in `routine::tests` (Step 5) + 5 refine-pass tests (AC-13a gate-drop, AC-13c daily persist, AC-13d fail-closed, AC-14/F-5 ×2) + 4 this-pass tests (AC-15 pause-lease hold/resume + structured-cancel ack; AC-16 wake-acceptance positive/negative) |
| `cargo test -p buzz-acp --test agent_controls_recovery` | 4 passed, 0 failed | No regressions |
| `cargo check -p buzz-acp --lib` (Step 7.2 fix) | Clean | `routine budget breached` INFO line added; no new warnings |
| `pnpm typecheck` (desktop) | 1 pre-existing TS2322 (`TimelineMessageList.tsx:749`), 0 new | Matches the spec's documented baseline exactly |
| `pnpm build` (desktop) | Same single TS2322, exit 2 | Identical failure mode to typecheck; not a Step 6 regression (confirmed via `git stash` A/B against the pre-Step-6 tree — same 26 pre-existing failures reproduce, same single build error) |
| `node --import ./test-loader.mjs --experimental-strip-types --test "src/features/workflows/**/*.test.mjs" "src/shared/ui/markdown/**/*.test.mjs" "src/shared/features/*.test.mjs" "src/features/messages/ui/MessageRow.test.mjs"` | 175 passed, 0 failed | Step 6 desktop scope: includes 5 new `routineCodeBlock.test.mjs`, 6 new `workflowFormTypes.test.mjs`, 1 new `workflowYamlDocument.test.mjs` case |

**Unrelated pre-existing failures (not this slice's regression):** `useComposerLinkPreviews.test.mjs` and `useDetachedAgentStart.test.mjs` show 26 failures in the full desktop suite. Verified via `git stash` A/B test against the pre-Step-6 tree: the identical 26 failures reproduce on the clean baseline with zero files from this slice touched. Not investigated further — out of Slice 3 scope.

**Not run this session:** `cargo test -p buzz-core` (Non-Goals fence it untouched — see AC-2), `cargo test -p buzz-db --lib` against a live Postgres test DB (the scratch Postgres+Redis instances used for the `--ignored` integration tests serve as the substitute evidence per spec §Constraints/N8: "if unavailable, state so and the relay end-to-end test in Step 4 becomes the DB proof").

---

## 2. Acceptance Criteria Status

| AC | Description | Status | Evidence |
|----|-------------|--------|----------|
| AC-1 | Pin and branch (`e1c321f95` ancestor of HEAD; no push) | **PASS** | `git merge-base --is-ancestor e1c321f95 HEAD` succeeds; 6 commits local only, no `origin/torq/slice3-routines`. |
| AC-2 | Frozen contract untouched | **PASS** | `git diff e1c321f95 --stat -- crates/buzz-core docs/nips/NIP-AO.md docs/nips/NIP-AO.fixtures.json crates/buzz-relay/src/handlers/event.rs` is empty. |
| AC-3 | Schema: budgets required, validated; contract-extension doc | **PASS** | 183 `buzz-workflow` tests green including `invoke_agent_requires_all_contract_fields`, `invoke_agent_rejects_zero_or_inverted_budgets`, `invoke_agent_rejects_two_steps`; `docs/nips/SLICE-0-CONTRACT-EXTENSION-invoke-agent-budgets.md` exists. |
| AC-4 | Relay env off: `NotImplemented` + exact reject string | **PASS** | Executor test asserts `NotImplemented("InvokeAgent")`; relay test asserts `rejected: invoke_agent is not enabled on this relay` verbatim. |
| AC-5 | Dispatch contract: 10 tags exactly, run `Running`, dispatch row | **PASS** | Relay end-to-end test asserts the full tag set, content suffix, run status, and `wake_event_id` set. |
| AC-6 | Migration `0045_routines.sql` matches Step 3.1 | **PASS** (schema); **PENDING** (live `\d` check) | File matches spec verbatim (§3 below); live DDL equivalence check requires the operator-attended Step 9 runbook against a real Postgres instance. |
| AC-7 | Dedupe + busy-skip | **PASS** | Same idempotency key → one row/one wake, second fire `deduplicated`; open dispatch → `routine_skipped_busy`, claim consumed, `consecutive_failures` unchanged. |
| AC-8 | Settlement gating (wrong signer / unknown run ignored) | **PASS** | `settle_rejects_wrong_signer_and_unknown_run` green; both paths log `routine_outcome_rejected` and leave state untouched. |
| AC-9 | Strikes: 9 fail + 1 success → 0; 10th → disabled + one notice | **PASS** | `strikes_reset_on_success_and_pause_at_ten` green. |
| AC-10 | Daily notice: two breaches one day → one notice | **PASS** | `daily_notice_once_per_day` green. |
| AC-11 | Sweeper timeout → `routine_timeout` exactly, counts strike | **PASS** | `sweeper_expires_open_dispatch` green; error code is exactly `routine_timeout` (G1R F-4, no stuttered spelling). |
| AC-12 | Owner guard + 20-cap (column-vs-JSON aware) | **PASS** | Relay e2e tests (refine pass, 2026-09-13): `routine_cap_rejects_twenty_first_enabled_routine` saves 20 enabled `invoke_agent` workflows then asserts the 21st save is rejected with the exact string `rejected: agent already has 20 enabled routines`; `routine_cap_counts_enabled_column_not_definition_json` constructs both disagreement directions through public paths only (column TRUE/JSON false via YAML `enabled:false`; column FALSE/JSON true via `set_workflow_enabled`) and asserts `count_enabled_invoke_agent_workflows` follows the column (G1R F-2). Owner-guard `forbidden:` string asserted in the pre-existing Step 4 e2e test. |
| AC-12b | Re-enable restores firing (column `enabled=TRUE`, not just `status`) | **PASS** | Relay e2e test (refine pass): `routine_reenable_restores_enabled_column_and_refires` drives a routine to auto-pause (10 failed settlements), re-saves it through the real ingest path, and asserts `enabled=TRUE` (the column) AND `status='active'` AND `consecutive_failures=0`/`paused_reason=NULL` AND that `list_all_enabled_workflows` — the query the tick loop actually fires from — returns it (G1R F-1). |
| AC-13 | Agent tag parsing + budget enforcement | **PASS** (turn-end); **DEFERRED, operator-ruled NOT BLOCKING** (goose mid-turn, 5.3(a) / D-2) | Tag parser tests in `routine::tests` (malformed budgets → `None`); `author_gate_tests::routine_tags_on_gate_failed_event_never_reach_the_parser` (refine pass) proves a routine wake from a non-authorized signer is dropped by the admission gate before parsing; `pool::tests::routine_per_run_breach_posts_only_the_routine_outcome` (refine pass) proves per-run breach → `budget_exceeded_per_run` at turn end; `control_store::tests::add_routine_tokens_accumulates_within_day_and_resets_on_new_day` (refine pass) proves same-day accumulation and new-day reset of `routine_daily_usage`; `pool::tests::routine_daily_store_error_fails_closed_store_unavailable` (refine pass) proves a failing store → outcome `failed` with detail `store_unavailable`. Mid-turn cancellation not implemented — see D-2 in §5 and §8. |
| AC-14 | Outcome event: exactly one per settled turn, exact shape | **PASS** | `routine::tests::outcome_event_is_threaded_under_the_wake_with_exact_tags` asserts kind:9, agent-signed, threaded under the wake, exactly one `buzz:routine-run` + one `buzz:routine-outcome` tag, and the fixed content strings. F-5 (never a second `post_failure_notice` on a routine turn) is covered by two refine-pass tests: `pool::tests::routine_per_run_breach_posts_only_the_routine_outcome` spies on the REST endpoint and asserts exactly one posted event (the routine outcome) after a per-run breach, and `error_outcome_emission_tests::ok_result_without_batch_never_posts_a_failure_notice` drives `handle_prompt_result` with an Ok result and `batch: None` and asserts zero events posted (G1R F-5). |
| AC-15 | Slice 2 interplay tests (5.6) | **PASS** | `agent_controls::tests::routine_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume` drives `effective_state_before_dispatch` (the exact gate `dispatch_pending` calls before flushing any batch) directly: no lease → `Running`; owner-issued pause applied → `HoldQueue`; resume applied → `Running` again — proving a routine-started batch has no bypass. `agent_controls::tests::structured_cancel_on_routine_started_turn_acks_applied` drives a Cancel one-shot command whose `target.run_id` is a routine run id (UUID, same shape as `RoutineBinding.run_id`) through `claim_one_shot`/`complete_one_shot` and asserts the stored `spent_command` ack is `applied` with no "turn already ending" detail — the ordinary case, keyed only by `channel_id`/`run_id` with no routine-specific branch in `handle_one_shot`. |
| AC-16 | Wake acceptance test (5.7) | **PASS** | `author_gate_tests::relay_signed_workflow_wake_with_self_key_is_owner_authored`: a kind:9 signed by a test relay key with the four base tags + mention + the four routine tags (`buzz:routine-run`, `buzz:routine`, `buzz:routine-idem`, `buzz:routine-budget` — matching the production wake shape in `buzz-relay/src/workflow_sink.rs:601-629`), through a connected `InboundAuthorGate` with `respond_to=OwnerOnly`, asserts `effective_author == workflow_owner` and `allowed`. Negative: `relay_signed_workflow_wake_without_self_key_is_not_owner_authored` — same event, NIP-11 document with no `self` key → gate falls back to the raw relay signer and `allowed` is false. |
| AC-17 | Desktop flag-off: `buzz-routine` block renders as plain YAML | **PASS** | `routineCodeBlock.test.mjs`: flag off + managed-agent author renders no review button (identical to plain YAML); flag on + non-agent author renders no button; flag on + non-routine language renders no button. `WIRED_TO_DESKTOP` in `agentComputerFlags.test.mjs` unchanged (still excludes `BUZZ_ROUTINES` — Step 10 only). |
| AC-18 | Desktop form: `invoke_agent` ↔ YAML round-trip + budget validation | **PASS** | 6 new `workflowFormTypes.test.mjs` cases: full round-trip preserves both budgets; zero `per_run`/`per_day` rejected; inverted budget (`per_day < per_run`) rejected; each required field's absence rejected; unsupported field falls back to YAML mode. `workflowYamlDocument.test.mjs`: `yamlWithWorkflowEnabled` preserves both budget fields across enable/disable. |
| AC-19 | Desktop typecheck/build: only the pre-existing TS2322 | **PASS** | `pnpm typecheck` and `pnpm build` both show exactly `TimelineMessageList.tsx:749` and nothing else; confirmed identical on the pre-Step-6 baseline via `git stash` A/B. |
| AC-20 | Baselines recorded, no new failures | **PASS** (with the N8 caveat above) | §1 lists every command from Constraints; the `buzz-relay --lib` figure discrepancy is disclosed, not hidden. |
| AC-21 | Live gate evidence (relay rebuild, fires, latency) | **NOT YET RUN** | Step 9 runbook is written (`docs/nips/slice3-runbook.md`) but not executed this session — touches the permanent relay and the shared dev database, out of scope for this pass per the dispatch instructions ("Do not touch the permanent relay, the live database, or running sidecars"). |
| AC-22 | No bodies/counts in logs | **NOT YET AUDITABLE** | Requires live evidence logs from Step 9 to grep. |
| AC-23 | R4 + Q2.2 notes recorded | **PASS** | §7 below. |
| AC-24 | Flag flip (Step 10) | **OUT OF SCOPE** | Explicitly operator-only, after the exit gate; not touched this session. |

---

## 3. Migration `0045_routines.sql` (verbatim)

```sql
SET LOCAL lock_timeout = '5s';

CREATE TABLE routine_dispatches (
    community_id     UUID NOT NULL REFERENCES communities(id),
    run_id           UUID NOT NULL,
    workflow_id      UUID NOT NULL,
    agent_pubkey     BYTEA NOT NULL,
    result_channel   UUID NOT NULL,
    idempotency_key  TEXT NOT NULL,
    fire_instant     TIMESTAMPTZ NOT NULL,
    wake_event_id    BYTEA,
    dispatched_at    TIMESTAMPTZ,
    settled_at       TIMESTAMPTZ,
    outcome          TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, run_id),
    UNIQUE (community_id, workflow_id, idempotency_key),
    FOREIGN KEY (community_id, run_id) REFERENCES workflow_runs (community_id, id) ON DELETE CASCADE,
    FOREIGN KEY (community_id, workflow_id) REFERENCES workflows (community_id, id) ON DELETE CASCADE
);
CREATE INDEX idx_routine_dispatches_open ON routine_dispatches (community_id, workflow_id) WHERE settled_at IS NULL;
CREATE INDEX idx_routine_dispatches_sweep ON routine_dispatches (created_at) WHERE settled_at IS NULL;

CREATE TABLE routine_state (
    community_id          UUID NOT NULL REFERENCES communities(id),
    workflow_id           UUID NOT NULL,
    consecutive_failures  INT NOT NULL DEFAULT 0,
    last_fired_at         TIMESTAMPTZ,
    last_outcome          TEXT,
    paused_reason         TEXT,
    paused_at             TIMESTAMPTZ,
    daily_notice_day      DATE,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, workflow_id),
    FOREIGN KEY (community_id, workflow_id) REFERENCES workflows (community_id, id) ON DELETE CASCADE
);
```

Matches build spec §3.1 verbatim. **Live `\d routine_dispatches` / `\d routine_state` output:** pending Step 9 (requires the shared or permanent relay's live Postgres, out of scope for this session per the dispatch's explicit "do not touch the live database" instruction). The scratch Postgres used for this session's `--ignored` integration tests was migrated to head 45 via `buzz_db::migration::run_migrations` and exercised every store function in `crates/buzz-db/src/store/routine.rs` end-to-end — that is the DB-correctness evidence for this pass; it is not a substitute for the live-DDL-equivalence paperwork Step 9 asks for.

---

## 4. Files Changed

| Path | Action | Purpose |
|------|--------|---------|
| `crates/buzz-workflow/src/schema.rs` | Modify | Required `token_budget_per_run`/`token_budget_per_day`, validation, `invoke_agent_step()`/`invokes_agent()`, single-step rule |
| `crates/buzz-workflow/src/executor.rs` | Modify | Resolve arm copies budgets unchanged; dispatch arm builds `InvokeAgentRequest`, calls the sink |
| `crates/buzz-workflow/src/action_sink.rs` | Modify | `ActionSink::invoke_agent`, `InvokeAgentRequest`/`InvokeAgentOutcome`, `AgentNotAllowed` |
| `crates/buzz-workflow/src/lib.rs` | Modify | `WorkflowConfig` fields + `from_env()`, `config()`; settlement branch (first statement after the `channel_id` guard, per G1R F-3); busy-skip before the claim; sweeper each tick; 7 Postgres-backed tests |
| `crates/buzz-workflow/src/error.rs` | Modify | `RoutineDispatch` variant, `routine_dispatch_failed` code |
| `migrations/0045_routines.sql` | Create | `routine_dispatches`, `routine_state` |
| `crates/buzz-db/src/store/routine.rs` | Create | Store functions (§3.2); 3 real bugs found and fixed via test execution (§5) |
| `crates/buzz-db/src/store/mod.rs`, `crates/buzz-db/src/lib.rs` | Modify | Register + re-export the `routine` module |
| `crates/buzz-relay/src/workflow_sink.rs` | Modify | `RelayActionSink::invoke_agent` — dedupe, destination/membership checks, owner-owns-agent + agent-is-member checks, 10-tag wake, persist + dispatch |
| `crates/buzz-relay/src/handlers/command_executor.rs` | Modify | Env-off/owner-guard/20-cap ingest checks; `reset_routine_state_on_enable` on enable; 4 end-to-end tests |
| `crates/buzz-relay/src/api/workflows.rs`, `crates/buzz-relay/src/router.rs` | Modify | `GET /workflows/{id}/routine-state` |
| `crates/buzz-relay/src/main.rs` | Modify | Production `WorkflowEngine::new` site uses `WorkflowConfig::from_env()` + startup log |
| `crates/buzz-acp/src/routine.rs` | Rewrite | Tag detection/parsing (fixed from the prior pre-session code), outcome value, threaded outcome event |
| `crates/buzz-acp/src/pool.rs` | Fix + instrument | Daily-budget comparison actually enforced; `store_unavailable` detail; one retry on outcome post; **this session:** added the missing `routine budget breached` INFO line (`run_id`, `kind=per_run\|per_day`) at both breach branches in `finalize_routine_turn` |
| `crates/buzz-acp/src/control_store.rs`, `crates/buzz-acp/src/lib.rs`, `crates/buzz-acp/src/queue.rs` | Modify | `routine_daily_usage` DDL/`add_routine_tokens`; admission wiring; `RoutineBinding` on `FlushBatch`/`BatchEvent`/`QueuedEvent` |
| `docs/nips/SLICE-0-CONTRACT-EXTENSION-invoke-agent-budgets.md` | Create | N2 record |
| **Step 6 desktop UI (this session):** | | |
| `desktop/src/shared/api/tauriWorkflows.ts`, `desktop/src/shared/api/workflowTypes.ts`, `desktop/src/shared/api/types.ts` | Modify | `RoutineState`/`RoutinePausedReason` type, `getRoutineState()` client fn |
| `desktop/src-tauri/src/commands/workflows.rs`, `desktop/src-tauri/src/lib.rs` | Modify | `get_routine_state` Tauri command, registered in `generate_handler!` |
| `desktop/src/features/workflows/hooks.ts` | Modify | `useRoutineStateQuery`, mounted only when `BUZZ_ROUTINES` is enabled |
| `desktop/src/features/agents/useKnownAgentPubkeys.tsx` | Modify | `useIsManagedAgentPubkey` (reuses the existing `localPubkeys` context set, no new query observer) |
| `desktop/src/shared/ui/markdown/types.ts`, `desktop/src/shared/ui/markdown.tsx`, `desktop/src/shared/ui/markdownUtils.ts` | Modify | `authorIsManagedAgent` threaded through `MarkdownRuntime` (per-message context, not the render-mode `variant` cache key) |
| `desktop/src/shared/ui/markdown/CodeBlock.tsx` | Modify | "Review routine" button on a `buzz-routine` block, gated on flag + managed-agent author + language; `[routines] review-open` `__s1Log` line |
| `desktop/src/features/messages/ui/MessageRow.tsx` | Modify | Passes `authorIsManagedAgent` into `VideoReviewCommentMarkdown`/`Markdown` |
| `desktop/src/shared/context/WorkflowEditorOverlayContext.tsx`, `desktop/src/app/AppWorkflowEditorOverlayProvider.tsx`, `desktop/src/features/workflows/ui/WorkflowEditorHost.tsx`, `desktop/src/features/workflows/ui/WorkflowDialog.tsx` | Modify | `initialYaml` threaded from the review card through to the create-mode dialog seed; `[routines] saved` `__s1Log` line on a routine save |
| `desktop/src/features/workflows/ui/workflowFormTypes.ts` | Modify | `invoke_agent` action type, form fields, YAML round-trip, budget validation (positive int + `per_day >= per_run`) |
| `desktop/src/features/workflows/ui/WorkflowStepCard.tsx` | Modify | `invoke_agent` step form fields (agent picker via `useManagedAgentsQuery`, prompt, result channel, idempotency key, both budgets) |
| `desktop/src/features/workflows/ui/WorkflowFormBuilder.tsx` | Modify | `invoke_agent` offered in the add-step menu only when `BUZZ_ROUTINES` is enabled |
| `desktop/src/features/workflows/ui/workflowStepDescription.ts` | Modify | Step-card summary text for `invoke_agent` |
| `desktop/src/features/workflows/ui/workflowDefinition.ts` | Modify | `isRoutineWorkflow()`, exported `getWorkflowSteps()` |
| `desktop/src/features/workflows/ui/WorkflowsView.tsx` | Modify | Routines filter chip, gated on the flag |
| `desktop/src/features/workflows/ui/WorkflowCard.tsx` | Modify | Routine status badge (text label, not colour-only); `invoke_agent` action tile icon/accent |
| `desktop/src/features/workflows/ui/WorkflowDetailPanel.tsx` | Modify | Routine section (status, last fire, last outcome, both budgets) |
| `desktop/src/shared/ui/markdown/routineCodeBlock.test.mjs` | Create | 5 cases for the review card (§6) |
| `desktop/src/features/workflows/ui/workflowFormTypes.test.mjs`, `desktop/src/features/workflows/ui/workflowYamlDocument.test.mjs` | Modify | New `invoke_agent` cases (§6) |

**Not yet touched (Step 10, operator-only, after the exit gate):** `preview-features.json`, `desktop/src/shared/features/agentComputerFlags.test.mjs`.

---

## 5. Deviations from Build Spec

1. **Two schema facts the spec's own text got wrong, found by executing the new tests, not by reading:**
   - `workflows` has no `paused_reason`/`paused_at` columns (the spec text and the pre-existing `routine.rs` both assumed there were). Fixed `settle_routine_dispatch` to write `status` only on `workflows` and `paused_reason`/`paused_at` only on `routine_state` (which does have them).
   - Two SQL type-cast bugs: `workflow_runs.status` and `workflows.status` are Postgres enums (`run_status`, `workflow_status`); a bind parameter needs an explicit `::run_status` cast, and a `SELECT w.status` needs `::text` to decode as `String` via sqlx. Both fixed; the working convention (`status::text AS status`) was already established elsewhere in `store/workflow.rs` and just not followed in the new file.
   - `routine_state`'s initial `INSERT ... VALUES` set `consecutive_failures` inverted from its own `ON CONFLICT DO UPDATE` branch (0 on first failure, 1 on first success) — fixed so both branches agree.
2. **Spec's claim "the crate already tests SendMessage through a mock" (Step 2 check) is factually wrong for this pin** — no `MockActionSink` exists anywhere in `buzz-workflow`. Wrote real Postgres-backed integration tests instead, which also exercise the DB layer the mock approach would have had to stub out.
3. **5.3(a) (goose mid-turn budget-breach cancellation) is not implemented — D-2, operator-ruled NOT BLOCKING (2026-09-13).** The spec assumed an external read-only accessor could poll usage while a turn is in flight; in the actual code, `session_prompt_blocks_with_idle_timeout` takes `&mut self` on the same `AcpClient` that owns the usage tracker and is awaited directly inside `run_prompt_task`'s `select!` — nothing else can borrow `agent.acp` concurrently. A correct mid-turn check can only run from inside `read_until_response_with_idle_timeout`, the shared read loop every agent turn uses, not just routines — a materially larger and riskier change than the spec assumed, and this slice will not rewrite that hot path. Operator ruling: turn-end-only enforcement is acceptable under the spec's own Risk 4 ("acceptable; state it"). The invariant that matters holds — a per-run breach is terminated at turn end, its reply withheld, the run counted `failed(budget_exceeded_per_run)`, and (for goose) the H1 respawn stated — so turn-end enforcement (5.3b) is a complete, correct substitute for this slice. See the Slice 5 item below for the deferred mid-turn cancel.
4. **5.6 (Slice 2 interplay tests) and 5.7 (wake acceptance test) — written this pass (G2A round-2 refine, 2026-09-13).** Not present in the Steps 1–5 session and not reached in the Steps 6–9 session; added in this refine pass per operator dispatch: `routine_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume` and `structured_cancel_on_routine_started_turn_acks_applied` in `crates/buzz-acp/src/agent_controls.rs` (next to the Slice 2 pause-lease/cancel tests), and `relay_signed_workflow_wake_with_self_key_is_owner_authored` (positive) plus `relay_signed_workflow_wake_without_self_key_is_not_owner_authored` (negative) in `crates/buzz-acp/src/lib.rs`'s `author_gate_tests` module, next to the equivalent non-routine wake tests. See §6 for the exact assertions each proves.
5. **Sequencing deviation, not a defect:** the harness dispatch said to review and fix the prior session's uncommitted `buzz-acp` edits as part of Step 5; because those edits were already staged when the Step 1 commit was made (to protect the new `routine.rs` file from an unrelated `git clean`), `crates/buzz-acp/src/routine.rs` first appears as a `create mode` in the Step 1 commit rather than Step 5's. Its actual content only became correct in the Step 5 commit.
6. **Step 6.3's review card does not pass `initialChannelId` to the create-workflow dialog**, despite the spec text saying "with `{ initialChannelId: <current channel> }`". `MarkdownRuntime` carries no current-channel-id field today, and the routine's own YAML already carries `result_channel` from the agent's `base_prompt.md` template (spec 5.8), so channel selection is a separate, already-correct concern (the form's `resultChannel` field, or the dialog's own channel picker). Threading a new channel-id field through the entire markdown runtime for this alone was judged out of proportion; noted here rather than silently dropped.
7. **The `buzz_routine_outcomes_total{outcome}` metrics counter from spec 7.1 was not added.** `buzz-workflow` (where routine settlement happens) has no dependency on the `metrics` crate today; `metrics` is only a dependency of `buzz-relay`. Adding it to `buzz-workflow/Cargo.toml` would be a new dependency for that crate, which the spec's Non-Goals explicitly forbid ("New crate dependencies"). The existing `tracing::info!("routine settled", ...)` line (with `outcome`, `strikes`, `latency_ms` fields) is the substitute observability signal for this pass; a metrics counter would need either a new `buzz-workflow` dependency or a relay-side hook into the engine's settlement path that doesn't currently exist. Flagged for G2A/operator decision rather than silently worked around.
8. **Step 9 is a written runbook only, not an executed one.** The dispatch for this session explicitly said "write the operator runbook only" and "do not touch the permanent relay, the live database, or running sidecars." `docs/nips/slice3-runbook.md` (see §9) contains the exact ordered steps, but no live relay rebuild, no live fires, and no latency measurement were performed this session. `docs/nips/slice3-evidence/` is created but empty pending that operator-attended run.
9. **Flaky fixture fix, found while re-running the required suites (this pass):** `routine_reenable_restores_enabled_column_and_refires` failed intermittently (~2 of 3 runs). Root cause: NIP-33 replaceable-event ordering ties on `created_at` and breaks ties by comparing event ids (`replace_parameterized_event_in_transaction_impl`, `crates/buzz-db/src/store/replaceable.rs:238-244`) — the test's re-enable event and its original save were both signed with the default (wall-clock) `created_at` inside the same second, so whether the re-save's event id happened to sort higher was a coin flip; when it lost, the replace returned `Superseded` (treated as `Duplicate`), `handle_command` short-circuited before `reset_routine_state_on_enable` ever ran, and the assertion at line ~2530 failed. Not a product defect — `upsert_workflow`'s `ON CONFLICT` intentionally never touches `status`/`enabled` (only `reset_routine_state_on_enable` does), and that function itself is correct. Fixed in the test fixture only: the re-enable event now carries `custom_created_at(original_created_at + 1)` so the replace is unconditionally newer. Re-ran 5x after the fix with zero failures (was failing ~2/3 before). This is a test-only change to `crates/buzz-relay/src/handlers/command_executor.rs`, not a relay/DB/sidecar product or schema change.

---

## 6. Test Inventory

**`buzz-workflow` (Steps 1–2):**
- `invoke_agent_action_round_trips_exact_fields`, `invoke_agent_requires_all_contract_fields`, `invoke_agent_rejects_zero_or_inverted_budgets`, `invoke_agent_rejects_two_steps`
- `invoke_agent_env_off_is_not_implemented`, `invoke_agent_dispatch_leaves_run_running`, `invoke_agent_dedup_and_busy_outcomes_complete`, `settle_rejects_wrong_signer_and_unknown_run`, `strikes_reset_on_success_and_pause_at_ten`, `daily_notice_once_per_day`, `sweeper_expires_open_dispatch`

**`buzz-relay` (Step 4) — hard-rule-10 end-to-end:**
- `routine_e2e_tests::routine_end_to_end_ingest_fire_settle` — owner-guard ingest, fire → dispatch (10-tag wake) → settle → dedupe → wrong-signer-reject → 10-strike auto-pause → env-off refusal, all in one test through the real engine tick and post-store hook, with the settlement fixture's `result_channel` deliberately carrying zero enabled workflows (proves G1R F-3's placement requirement, not just the direct `settle_routine_outcome` unit tests).
- Refine pass (2026-09-13, G2A D-1/D-3): `routine_cap_rejects_twenty_first_enabled_routine`, `routine_cap_counts_enabled_column_not_definition_json`, `routine_reenable_restores_enabled_column_and_refires`. The e2e fixture's result channel is now a genuinely bare channel (distinct channel, zero workflows saved onto it — D-3); the sink's agent-membership requirement on the result channel is satisfied by explicit membership, not by sharing the workflow channel. The same bare-channel correction was applied to `settle_rejects_wrong_signer_and_unknown_run`, `strikes_reset_on_success_and_pause_at_ten`, and `daily_notice_once_per_day` in `buzz-workflow`; a mutation check (settlement branch moved below the cache lookup) fails all three, confirming the fixtures now guard F-3 placement.
- Builder refine round (this pass, 2026-09-13): `routine_reenable_restores_enabled_column_and_refires` fixture fixed for a flaky NIP-33 tie-break (Deviation 9 in §5) — re-enable event now signed with `created_at` strictly after the original save's, making the replace deterministically win instead of a coin-flip on event-id ordering.

**`buzz-acp` (Step 5):**
- 12 tests in `routine::tests` (tag parser: all four tags, malformed budget → `None`, routine tags on a gate-failed event → `None`; outcome shape assertions)
- Refine pass (2026-09-13, G2A D-1): `author_gate_tests::routine_tags_on_gate_failed_event_never_reach_the_parser` (AC-13a), `control_store::tests::add_routine_tokens_accumulates_within_day_and_resets_on_new_day` (AC-13c), `pool::tests::routine_daily_store_error_fails_closed_store_unavailable` (AC-13d), `pool::tests::routine_per_run_breach_posts_only_the_routine_outcome` + `error_outcome_emission_tests::ok_result_without_batch_never_posts_a_failure_notice` (AC-14 / G1R F-5).
- Builder refine round (this pass, 2026-09-13, spec 5.6/5.7): `agent_controls::tests::routine_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume` (AC-15, pause-lease hold + resume), `agent_controls::tests::structured_cancel_on_routine_started_turn_acks_applied` (AC-15, structured-cancel ack), `author_gate_tests::relay_signed_workflow_wake_with_self_key_is_owner_authored` + `author_gate_tests::relay_signed_workflow_wake_without_self_key_is_not_owner_authored` (AC-16, wake acceptance positive/negative).

**Desktop (Step 6, this session):**
- `routineCodeBlock.test.mjs` (5): flag-off + managed-agent author renders no button; flag-on + non-agent author renders no button; flag-on + managed-agent author renders the button; flag-on + managed-agent author + non-routine language renders no button; clicking the button opens the create-workflow editor seeded with the block's YAML.
- `workflowFormTypes.test.mjs` (+6): `invoke_agent` round-trips YAML→form→YAML including both budgets; rejects zero `token_budget_per_run`; rejects zero `token_budget_per_day`; rejects an inverted budget; requires all four contract string fields in Form mode; an unsupported field falls back to YAML mode.
- `workflowYamlDocument.test.mjs` (+1): `yamlWithWorkflowEnabled` preserves an `invoke_agent` step's token budgets across an enable/disable round trip.

---

## 7. Contract Revision R4 + Q2.2 Note

**R4 — at-most-once firing semantics (accepted, not a new invariant).** The canonical PRD (§ FR-3) describes routine firing in terms that could be read as "at least once," but I-15 and the existing scheduler (`lib.rs:490-760`, unchanged by this slice) guarantee at-most-once per `(community_id, workflow_id, scheduled_for)`: missed instants are skipped, never replayed, and a relay restart mid-window neither double-fires nor replays. G1R's review confirmed this is the existing behavior, not a new surface this slice introduces, and recorded it as a disclosed, justified PRD deviation rather than a defect ("Attack vectors that did not land" in `gate1_review.md`). No code change was needed to satisfy R4 — the sweeper and busy-skip added by this slice (§2.6) operate strictly on top of that existing at-most-once guarantee and never widen it.

**Q2.2 — outcome visibility is words, not counts (multi-tenant boundary).** The relay only ever observes outcome *words* — `succeeded`, `failed`, `budget_exceeded_per_run`, `budget_exceeded_daily` — carried in the `buzz:routine-outcome` tag of the agent-signed outcome event. It never sees a token count, a cost, or a running total. Token accounting (both per-run comparison and the UTC-day running total) happens entirely inside `buzz-acp`'s local `routine_daily_usage` SQLite table, which is per-agent and never leaves the sidecar. This is the documented alternative to a shared, relay-side budget ledger: each managed agent's sidecar is the sole source of truth for its own usage, and the relay's role is limited to gating (the two switches, the 20-cap, the owner check) and settlement bookkeeping (strikes, pause state) driven only by the outcome word. I-14 ("No bodies, no counts") is satisfied by construction, not by redaction — the relay is structurally never given the number to redact.

---

## 8. Slice 5 / Follow-up Items

- **Goose mid-turn routine-budget cancel inside `read_until_response_with_idle_timeout`** (Deviation 3 / D-2, operator-ruled NOT BLOCKING 2026-09-13) — requires touching the shared read loop every agent turn uses, not just routines. Turn-end enforcement (5.3b) is a complete, correct substitute for this slice; the operator ruling stands on the record as the disposition of D-2.
- **`buzz_routine_outcomes_total` metrics counter** (Deviation 7) — needs either a new `buzz-workflow` → `metrics` dependency (currently forbidden by Non-Goals) or a relay-side settlement hook that doesn't exist yet.
- **Step 9 live execution** — the runbook is written; nothing in it has been run. This is the critical path before the exit gate: it is the only place AC-6 (live DDL check), AC-21, and AC-22 can be satisfied.
- **Step 10 flag flip** — explicitly operator-only, after the exit gate.

---

## 9. Runbook

Written, not executed, this session per the dispatch's explicit scope ("write the operator runbook only... Do not touch the permanent relay, the live database, or running sidecars"). See `docs/nips/slice3-runbook.md` for the full ordered procedure: relay rebuild and key bring-up from `config\relay.env` with the old binary backed up, migrations on the live DB after a `pg_dump`, the two-switch flag-off proof, fire/settle/strike/daily/kill-restart steps, and the Slice 2 pause-hold and cancel interplay.

---

## 10. Evidence Inventory

Directory: `docs/nips/slice3-evidence/` — **created, empty.** Populated only by the operator-attended Step 9 run. Expected contents once run: relay startup log excerpt (migration head, `self` pubkey, `workflow invoke_agent enabled=<bool>` line), the two-switch flag-off proof (both directions), a chat transcript from draft → review → save → ≥2 fires → outcome, a relay-kill-mid-window log pair showing no duplicate post, per-run/daily/ten-strike breach transcripts, the pause-hold/cancel interplay transcript, and the latency CSV/script for the fire-to-inject p95 measurement.

---

## 11. G2A Handoff Notes

**Verified by real test execution this pass:** Step 6's desktop UI (review card gating, `invoke_agent` form round-trip and validation, panel/filter/badge wiring) — 175 desktop tests green, `pnpm typecheck`/`pnpm build` showing only the pre-existing baseline error, confirmed via a `git stash` A/B against the pre-Step-6 tree that nothing in this slice caused the 26 unrelated pre-existing failures elsewhere in the suite. The Step 7.2 sidecar instrumentation gap (`routine budget breached`) found while reviewing `buzz-acp` for the review card was fixed and compile-checked.

**Carried forward from the Steps 1–5 session, unchanged this pass:** schema, engine dispatch/settlement/sweeper, relay ingest guards, and agent-side tag parsing/budget enforcement are feature-complete and verified by real Postgres/Redis-backed test execution (see build_result_SLICE-3-ROUTINES.md for the full account of that session's work, including three real schema-vs-code bugs it found and fixed via test execution, not review).

**Verified by real test execution this builder refine round (2026-09-13, operator-dispatched, bounded):** (a) the G2A round-2 refine delta from the prior session (`control_store.rs`, `lib.rs`, `pool.rs` in buzz-acp; `command_executor.rs`; buzz-workflow `lib.rs`; this doc) was committed with DCO at `ca6408bae`; (b) the two spec-5.6 Slice 2 interplay tests and the spec-5.7 wake-acceptance test (positive + negative) are written and green — AC-15 and AC-16 move from NOT WRITTEN to PASS; (c) D-2 (goose mid-turn cancel, 5.3(a)) is recorded above as an operator-ruled NOT BLOCKING deviation with its Slice 5 item. While re-running the required suites, this round also found and fixed a flaky test fixture (`routine_reenable_restores_enabled_column_and_refires`, Deviation 9) — not a product defect, disclosed above. Full re-run this round: `cargo test -p buzz-acp --lib` 949/0, `cargo test -p buzz-acp --test agent_controls_recovery` 4/0, `cargo test -p buzz-workflow` 183/0 (+9 ignored), `cargo test -p buzz-relay --lib routine -- --ignored --test-threads=1` 7/0 (re-run 5x clean after the fixture fix). No relay, live DB, or running-sidecar changes were made this round, per the operator dispatch's explicit scope.

**Not verified by anything beyond static review, pending the operator-attended Step 9 run:** any live relay-to-agent-to-relay round trip, the fire-to-inject p95 latency claim, the live DDL equivalence check, and the "no bodies/no counts" grep audit against real logs. AC-15/16 (the Slice 2 pause/cancel interplay and wake acceptance) are now unit/integration-verified per above; AC-21/AC-22 remain gated on the live Step 9 run per the operator's own ruling that Step 9 is the operator's to execute.

**Summary:** Steps 1–6 of this 10-step, 7-crate feature are implementation-complete and covered by 175+ passing desktop tests plus the Rust test suites (183+9 buzz-workflow, 1032+7 buzz-relay, 949 buzz-acp after this round). Step 7's relay/engine instrumentation was already complete from Steps 2/4; the one sidecar gap found in an earlier pass is fixed. Step 8 is this document, updated this round for the G2A round-2 refine delta, D-2, and the 5.6/5.7 tests. Step 9 (live gate, AC-21/AC-22) remains the operator's to execute next, per the operator ruling on record in `harness_status.json`.

---

Generated: 2026-09-13
