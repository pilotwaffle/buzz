# SLICE-4-1-VERIFICATION.md — TORQ-BUZZ Slice 4.1: Delegation dispatcher fixes (D-L2, D-L3)

**Date:** 2026-09-17
**Builder:** Builder · **G2A target:** g2a
**Branch:** `torq/slice4-1-delegation-dispatcher`
**Pin:** `f5a8a0d0b` — verified clean at checkout (`git merge-base --is-ancestor f5a8a0d0b HEAD` on the branch tip; `git status --short` empty at the branch-cut moment, per N3/AC-1)
**Commits (pin..HEAD):**
- `b54e00c79` fix(delegation): D-L2 store layer — visible turn-exhausted rows, record-level settle
- `16a578485` fix(delegation): D-L2/D-L3 dispatcher — no silent bail anywhere in the path
- `912538f63` test(delegation): AC-7 store-error coverage via extracted pure decision

---

## 1. Before/After Counts

Baseline is the pin (`f5a8a0d0b`), re-measured directly in an isolated `git worktree add /tmp/pin-check f5a8a0d0b` (a plain, additive git operation touching no files in the working branch) rather than assumed from an earlier in-session measurement.

| Suite | Before (pin) | After (HEAD) | Notes |
|---|---|---|---|
| `cargo test -p buzz-core` | 290 passed, 0 failed | 290 passed, 0 failed | Unchanged — I-1 frozen-path fence confirmed empty |
| `cargo test -p buzz-acp --lib` | 965 passed, 0 failed | 965 passed, 0 failed | Unchanged |
| `cargo test -p buzz-relay --lib` | 1045 passed, 0 failed, 108 ignored | 1047 passed, 0 failed, 109 ignored | +2 passed (the new `dispatch::tests::store_error_outcome_*` unit tests), +1 ignored (net across the `delegation_e2e_tests` module: +1 new turn-exhausted test, rename is not a net add, the store-error test was added then removed — see §4) |
| `cargo test -p buzz-relay --lib delegation::dispatch::tests::` | 0 (module did not exist) | 2 passed, 0 failed | New this build (AC-7, see §4 deviation) |
| `cargo test -p buzz-relay --lib delegation -- --ignored --test-threads=1` | 12 passed, 0 failed | 13 passed, 0 failed | +1 (`delegation_turn_exhausted_settles_failed_turns_with_one_notice`); `delegation_owner_deactivated_before_effect_cancels_row_with_notice` renamed in place (not a net add); `delegation_store_error_on_owner_resolution_audits_and_retries` written then removed in favor of the pure unit test above — see §4. Matches AC-10's required 13/0. |
| `cargo test -p buzz-db --lib` | 121 passed, 1 failed, 261 ignored | 121 passed, 1 failed, 262 ignored | +1 net ignored (−1 removed overdraw test, +2 new store tests — I-7/AC-9). The 1 failure (`runtime::migration::postgres_tests::embedded_migrator_contains_consolidated_initial_schema`) is pre-existing and unrelated, reproduced identically at the pin in an isolated `git worktree add /tmp/pin-check f5a8a0d0b` (not via `git stash`, after an earlier `git stash pop` in this session accidentally applied an unrelated old stash entry from a different branch — cleaned up via `git reset --hard HEAD`, no work lost; see the session record) |
| `cargo test -p buzz-db --lib delegation:: -- --ignored --test-threads=1` | 9 passed, 0 failed | 10 passed, 0 failed | +2 new (`load_returns_turn_exhausted_approved_row_with_no_context`, `fail_delegation_record_is_terminal_and_notice_due_once`), −1 removed (`child_claim_reserves_parent_budget_and_refuses_overdraw`, I-7/AC-9) |
| `cd desktop && pnpm typecheck` | 1 pre-existing TS2322 (`TimelineMessageList.tsx:749`) | 1 pre-existing TS2322 (`TimelineMessageList.tsx:749`) | Unchanged; confirmed via `git status`/`git log` that this build never touched that file |

All `--ignored` suites ran against the local scratch containers (`torq-buzz-postgres-scratch` on `127.0.0.1:55433`, `torq-buzz-redis-scratch` on `127.0.0.1:63799`, confirmed via `docker ps`), never the shared deployment instance.

---

## 2. Invariant Status (I-1 .. I-9)

| Invariant | Description | Status | Evidence |
|---|---|---|---|
| I-1 | Frozen paths untouched | **PASS** | `git diff f5a8a0d0b HEAD --stat -- crates/buzz-core docs/nips/NIP-AO.md docs/nips/NIP-AO.fixtures.json docs/nips/NIP-DG.md crates/buzz-relay/src/handlers/event.rs crates/buzz-workflow migrations preview-features.json` is empty. |
| I-2 | Turn-exhausted records are visible | **PASS** | `load_delegation_record` (`crates/buzz-db/src/store/delegation.rs`) now maps a turn-exhausted `approved` record to `Ok(Some(row))` with `row.context == None`, `row.remaining_turns == 0`, distinguished from every other loader error (still `Err(_) => Ok(None)`) by the new `latest_action_open` subselect and match-arm split. `load_returns_turn_exhausted_approved_row_with_no_context` (store test) and the real-path `delegation_turn_exhausted_settles_failed_turns_with_one_notice` both cover this. |
| I-3 | Turn-exhausted records terminate | **PASS** | `dispatch_next`'s `row.remaining_turns == 0` branch calls `settle_current_and_notice(..., "turns")` before ever touching `context`; `delegation_turn_exhausted_settles_failed_turns_with_one_notice` drives the real approve→outcome→dispatch path and asserts `state == "failed"`, `failure_detail == "turns"`, action 1's outcome unchanged (`delegated`), exactly one notice, and idempotence on a second `dispatch_next` call. |
| I-4 | Owner-unavailable terminates with audit | **PASS** | `cancel_owner_unavailable` (new helper) settles the record `failed`/`cancelled`, cancels the open action row (`owner_unavailable`) if one exists, posts exactly one failure notice, and is reached from both the pre-CAS (`dispatch_next`'s `OwnerMismatch`/`TenantMismatch` match arm) and post-CAS (`dispatch_action`'s owner-None-after-CAS site) call sites. `delegation_owner_deactivated_before_effect_cancels_row_with_notice` (renamed from the Slice 5 gap-pinning test, assertions flipped) drives the real path and asserts the audit line contains `reason="owner_unavailable"`. |
| I-5 | Store errors do not terminate | **PASS** | Every DB error from `resolve_agent_owners`, `open_action_as_target`, `resolve_lineage`, and the post-CAS write/rollback sites calls `audit_denied(id, "authority_unavailable")` and returns without settling — never `cancel_*`/`settle_*`. The `resolve_agent_owners`/`open_action_as_target` decision is now the pure, unit-tested `store_error_outcome` function (`dispatch::tests::store_error_outcome_continues_on_ok`/`_audits_and_retries_on_err`); see §4 for why the live e2e reproduction was replaced. |
| I-6 | At most one failed notice | **PASS** | Both new/changed paths route through `record_delegation_failure_notice`'s existing `notice_due` guard, unchanged. `count_failed_notices == 1` asserted in `delegation_turn_exhausted_settles_failed_turns_with_one_notice` (including after a second `dispatch_next` call) and in `delegation_owner_deactivated_before_effect_cancels_row_with_notice`. |
| I-7 | Obsolete test gone | **PASS** | `child_claim_reserves_parent_budget_and_refuses_overdraw` deleted from `crates/buzz-db/src/store/delegation.rs`; `grep -rn child_claim_reserves_parent_budget_and_refuses_overdraw crates/ docs/` shows only historical mentions in `SLICE-4-VERIFICATION.md`/`SLICE-5-CLOSEOUT.md` and a doc-comment note in `tests.rs`, never live code (AC-9). `buzz-db --lib` ignored count reflects the removal net of the 2 new store tests. |
| I-8 | No other behaviour change | **PASS** | `buzz-core`, `buzz-acp --lib`, `buzz-relay --lib` (non-ignored) all match the pin baseline exactly (§1); every pre-existing delegation e2e test still passes, with only the two intentionally-flipped assertions (`delegation_turn_exhausted_...`/`delegation_nested_hop_and_turns` bullet 4) changed in meaning. |
| I-9 | No silent bail anywhere in the dispatch path [G1R A-1] | **PASS** | Every `else { return; }` / `Err(_) => return` in `dispatch_next`, `dispatch_action`, and `settle_current_and_notice` is preceded by an `audit_denied` call, a `settle_*`/`cancel_*` call, or is the successful-dispatch exit — including all nine G1R A-1 post-CAS/notice-path sites in `dispatch_action` (tag build, sign, `lookup_community_host`, hex decode, `get_event_by_id_for_event_write`, `insert_event_with_thread_metadata`) and `settle_current_and_notice` (settle Err, host Err, notice-post Err), each calling `audit_denied(id, "authority_unavailable")` — audit-only, per N1/Risk 5, since the CAS has already committed at those sites. Verified by reading the full diff to `dispatch.rs` against this exact invariant text before committing. |

---

## 3. Files Changed

| File | Commit(s) | Summary |
|---|---|---|
| `crates/buzz-db/src/store/delegation.rs` | `b54e00c79` | `DelegationRecordRow.latest_action_open` field + subselect; `load_delegation_record`'s turn-exhausted match-arm split (I-2); new `fail_delegation_record` function (record-level terminal settle, no action-row write, mirrors `settle_action`'s UPDATE/`notice_due` logic per Risk 1); deleted `child_claim_reserves_parent_budget_and_refuses_overdraw` (I-7); added `load_returns_turn_exhausted_approved_row_with_no_context`, `fail_delegation_record_is_terminal_and_notice_due_once`. |
| `crates/buzz-relay/src/delegation/dispatch.rs` | `16a578485`, `912538f63` | D-L2: early turn-exhausted settle in `dispatch_next` before context rebuild; `settle_current_and_notice` split on `row.latest_action_open` (open action vs. record-level). D-L3: `audit_denied`/`post_failure_notice_if_due`/`cancel_owner_unavailable`/`settle_record_and_notice` helpers; every silent-return site in `dispatch_next`, `dispatch_action`, `settle_current_and_notice` now audits (I-9), including the nine G1R A-1 sites. AC-7 follow-up: `resolve_agent_owners`/`open_action_as_target`'s error handling extracted into the pure `store_error_outcome` function plus a co-located `#[cfg(test)] mod tests`. |
| `crates/buzz-relay/src/delegation/tests.rs` | `16a578485`, `912538f63` | `delegation_owner_deactivated_before_effect_cancels_row_with_notice` (renamed, assertions flipped to the fixed behavior, tracing-capture added). New `delegation_turn_exhausted_settles_failed_turns_with_one_notice`. `delegation_nested_hop_and_turns` bullet 4 flipped; bullet 7 fixed (a fresh, never-settled A→B delegation inserted so B has a genuinely open action, since D-L2's fix means the capped parent no longer stays stuck `approved`). `delegation_sweeper_times_out_and_retries_then_notices` updated to use `load_record_retrying` (now works at `remaining_turns==0`). Doc comment at the top of `delegation_nested_hop_and_turns` rewritten to cite the per-hop-cap ruling instead of the deleted overdraw test. `delegation_store_error_on_owner_resolution_audits_and_retries` written, then removed — see §4. |

---

## 4. Deviations

- **N1 "settle+audit" reads "audit, never vanish."** As specified: a transient store error never settles the record; it audits `authority_unavailable` and leaves it `approved` for the sweeper.
- **N2 Detail word.** `owner_unavailable` outcomes use `failure_detail='cancelled'` in the row/notice (NIP-DG's frozen vocabulary); the audit line's `reason=owner_unavailable` carries the precise cause. No NIP-DG vocabulary change made.
- **N3 Branch cut.** Cut from tip `b3c56a793` after the operator's evidence-file commit; `git status --short` was confirmed empty at that moment.
- **AC-7 deviation (the one substantive scope change from the literal spec text).** The spec calls for a live e2e test (`delegation_store_error_on_owner_resolution_audits_and_retries`, "may reuse the `delegation_store_unavailable_dispatches_nothing` fixture technique") that forces a genuine `sqlx::Error` specifically at `resolve_agent_owners` while leaving the preceding `load_delegation_record` call to succeed. This was attempted and is recorded here as infeasible in this environment, not abandoned without effort: seven distinct fault-injection techniques were tried —
  1. Holding a transaction on a `max_connections(1)` pool before calling `dispatch_next`: starves `load_delegation_record` itself, not `resolve_agent_owners` (wrong call site).
  2. Spawning `dispatch_next` and busy-polling to grab the pool's connection the instant it's released (`pg_stat_activity`-based detection): `dispatch_next`'s local-Postgres query chain resolves within a single Tokio task poll, faster than a separately-scheduled polling task can reliably interleave with it, even measuring in-process (`Pool::num_idle`) rather than round-tripping to Postgres.
  3. A `before_acquire` pool hook that lets the Nth checkout through and fails the rest: `before_acquire` only runs on *reuse* of a pooled idle connection (confirmed by reading `sqlx-core-0.9.0/src/pool/inner.rs`'s `check_idle_conn`); a rejected connection is simply discarded and the pool opens a fresh one within its `max_connections` budget, so the acquire always eventually succeeds.
  4. The same hook with a two-worker-thread runtime, hoping genuine OS-thread parallelism would let a separate polling task win the race: it did not — `dispatch_next`'s entire call (including a `resolve_agent_owners` invocation, confirmed `Ok`) completed before the polling task's first scheduler turn.
  5. `pg_terminate_backend` on the exact backend PID once `pg_stat_activity` shows it running `resolve_agent_owners`'s query text: never observed the query in the `active` state before it finished — the query itself is too fast on local loopback Postgres for an external poll (which itself costs a full round-trip) to catch it mid-flight.

  The underlying cause is the same across every attempt: this dispatcher's per-hop DB work is a handful of sub-millisecond local queries with no natural yield point long enough for external test code (in-process or via a second connection) to interleave a fault into one specific call without also catching an adjacent one, or missing the window entirely.

  **Resolution (operator-directed):** the `Err(_) => audit_denied(...)` decision at both `resolve_agent_owners` and `open_action_as_target` was extracted into a pure, DB-free function, `store_error_outcome<T, E>(result: &Result<T, E>) -> StoreErrorOutcome` (`Continue` | `AuditAndRetryLater`), used identically at both call sites with no behavior change (verified by diff and by the unchanged e2e suite result). `store_error_outcome_continues_on_ok` and `store_error_outcome_audits_and_retries_on_err` (`crates/buzz-relay/src/delegation/dispatch.rs`, `#[cfg(test)] mod tests`) cover the decision deterministically. AC-7's live e2e test was written, confirmed infeasible via the above, and removed rather than left flaky or timing-dependent; **flagged for G2A to independently accept or reject this substitution** — the residual risk is that this unit test proves the *decision* is correct but does not exercise the real `sqlx`/`tokio` machinery end to end the way the other two real-path tests (`delegation_turn_exhausted_...`, `delegation_owner_deactivated_...`) do.
- **Residual, out of scope (not "fixed while here").** The step-e cancel path (`dispatch_action`, pre-CAS `action_conflict`) already audits and settles the action `cancelled/owner_changed` but posts no notice and leaves the record `approved` — this is a pre-existing, packet-named-out-of-scope gap (spec Gotchas), not touched here.

---

## 5. Test Inventory

**New (this build):**
- `buzz-db`: `load_returns_turn_exhausted_approved_row_with_no_context`, `fail_delegation_record_is_terminal_and_notice_due_once` (`--ignored`).
- `buzz-relay`: `delegation_turn_exhausted_settles_failed_turns_with_one_notice` (`--ignored`). Pre-fix run (code reverted to the pin via `git checkout f5a8a0d0b -- ...`) failed with the record staying `approved` and the loader returning `None` for the turn-exhausted row, matching I-2/I-3's stated pre-fix symptom.
- `buzz-relay`: `dispatch::tests::store_error_outcome_continues_on_ok`, `dispatch::tests::store_error_outcome_audits_and_retries_on_err` (pure, always-run).

**Renamed + flipped (this build):**
- `delegation_owner_deactivated_before_effect_cancels_row_with_notice` (was `delegation_owner_deactivated_before_effect_cancels_row`). Pre-fix run (assertions reverted to the original gap-pinning form) failed: no audit line, record left `approved`, matching I-4's stated pre-fix symptom.
- `delegation_nested_hop_and_turns` bullet 4 (asserts `failed`/`turns`, one notice, action 1 outcome preserved, instead of "stays `approved`"); bullet 7 updated (fresh A→B delegation inserted, since bullet 4's capped parent no longer satisfies bullet 7's "still has an open action" premise after the D-L2 fix — `open_action_as_target` requires `state='approved'`).

**Removed (this build, I-7/AC-9):**
- `child_claim_reserves_parent_budget_and_refuses_overdraw` (`buzz-db`) — obsolete per the 2026-09-17 per-hop-cap operator ruling (`SLICE-5-CLOSEOUT.md` §9, `design_answers_TBAC-06-slice4-delegation.md` Q3 blockquote).
- `delegation_store_error_on_owner_resolution_audits_and_retries` (`buzz-relay`) — written, confirmed infeasible as a live e2e test in this environment, replaced by the two `store_error_outcome` unit tests. See §4.

**Full `buzz-relay --lib delegation -- --ignored --test-threads=1` (13/0):** `delegation_budget_exhaustion_fails_budget`, `delegation_cost_cap_refuses_every_action`, `delegation_end_to_end_approve_claim_dispatch_settle`, `delegation_flag_off_rejects_both_auth_variants`, `delegation_nested_hop_and_turns`, `delegation_outcome_settles_through_real_ingest_path`, `delegation_owner_deactivated_before_effect_cancels_row_with_notice`, `delegation_relay_store_scan_has_no_body`, `delegation_store_unavailable_dispatches_nothing`, `delegation_sweeper_times_out_and_retries_then_notices`, `delegation_tenant_route_flag_off_matches_unknown_route`, `delegation_turn_exhausted_settles_failed_turns_with_one_notice`, `metrics_and_audit_lines_carry_no_bodies`.

---

## 6. Runbook Pointer

See `docs/nips/slice4-1-runbook.md` for the operator runbook (written, not executed this session, per the SOC-1/SOC-2 pattern established in Slice 3/4/5) and `docs/nips/slice4-1-evidence/README.md` for the evidence-collection procedure. AC-12's live re-gate (`slice4-1-evidence/02-turns.md`, `03-owner.md`) is an operator action, not performed here; `push_authorized` remains `false` and no flag change was made.

---

## 7. G2A Notes

- **Independently verify AC-7's substitution decision (§4).** This is the one place this build deviated from the spec's literal test name/technique. The operator (via this session's user) directed the extract-and-unit-test approach after reviewing the seven failed fault-injection attempts; G2A should form its own view on whether a pure unit test on the extracted decision is sufficient coverage, or whether the live e2e test should be attempted again with a different technique before this slice is accepted.
- **AC-6 (no silent bails) is verified by reading**, not by an automated grep — G2A should independently re-read `dispatch.rs`'s full `dispatch_next`/`dispatch_action`/`settle_current_and_notice` control flow against I-9's exact site list (nine G1R A-1 sites plus the pre-existing ones) rather than trusting this document's claim.
- **The `buzz-db --lib` migration-count failure and the desktop `TimelineMessageList.tsx` typecheck error are both pre-existing** (reproduced identically against the unmodified pin tree); neither was introduced or touched by this build.
- **Residual gap, explicitly out of scope:** the step-e cancel path's missing notice (§4) — not fixed here, not asked for by this packet.
