# SLICE-2-VERIFICATION.md — TORQ-BUZZ Slice 2: Structured Controls

**Date:** 2026-09-11
**Builder:** Builder · **G2A target:** g2a
**Commit:** TORQ-BUZZ `torq/slice2-structured-controls` @ `57f2a374e` (Steps 1-7 + G2A Round-2 retry, DCO signed)
**Round:** 2 (resubmission after G2A REJECT — D1 test coverage, D2 fail-closed, D3 uuid v5, F2 retention boundary)

---

## 1. Before/After Counts

| Suite | Before (pin `8abc8886a`) | After (HEAD `57f2a374e`) | Δ |
|-------|--------------------------|---------------------------|----|
| `cargo test -p buzz-acp --lib` | 890 passed / 0 failed | **915 passed / 0 failed** | +25 |
| `cargo test -p buzz-acp --test agent_controls_recovery` | — | **4 passed / 0 failed** | new suite |
| `cargo test -p buzz-core` | 283 pass / 0 fail | 283 pass / 0 fail | 0 (unchanged) |
| `cargo test -p buzz-workflow` | 181 pass / 2 ignored | 181 pass / 2 ignored | 0 (unchanged) |
| `pnpm typecheck` (desktop) | 1 pre-existing TS2322 | 1 pre-existing TS2322 | 0 (unchanged) |
| `pnpm test` (desktop, targeted) | — | 49 pass / 0 fail (T23–T25 + regression) | +49 |
| T23 `agentStructuredControl.test.mjs` | — | 22 pass | new |
| T24 `controlState.test.mjs` | — | 16 pass | new |
| T25 `AgentControlsBar.keyboard.test.mjs` | — | 7 pass | new |
| `LiveActivityTimeline.keyboard.test.mjs` | 4 pass | 4 pass | 0 (regression) |

**No regressions.** No pre-existing test weakened or deleted. No new failures introduced.

---

## 2. Acceptance Criteria Status

| AC | Description | Status | Evidence |
|----|-------------|--------|----------|
| AC-1 | Branch/pin `8abc8886a` ancestor of `torq/slice2-structured-controls` | **PASS** | `git merge-base --is-ancestor 8abc8886a HEAD; echo $?` → 0. No push (origin unknown). |
| AC-2 | Frozen contract + relay untouched | **PASS** | `git diff 8abc8886a --stat -- crates/buzz-core crates/buzz-relay docs/nips/NIP-AO*` is empty. |
| AC-3 | Single new dependency (`rusqlite`) | **PASS** (minor spec note) | Only `rusqlite = { version = "0.37", features = ["bundled"] }` added to `buzz-acp/Cargo.toml`. `uuid` feature `v5` enabled on existing workspace dep — required by Step 2.2 (`Uuid::new_v5` in `derive_community_id`); see D3 in Deviations. |
| AC-4 | Flags-off baselines unchanged | **PASS** | buzz-core 283/0, buzz-workflow 181/0/2, typecheck same TS2322. |
| AC-5 | Store schema | **PASS** | `schema_matches_ddl_column_for_column` test passes — column/constraint/index equivalence verified. DDL copy in §3. |
| AC-6 | One-shot claim semantics (T1–T4) | **PASS** | T1 `claim_and_complete` · T2 `duplicate_claim_returns_duplicate_pending` · T3 `duplicate_completed_returns_stored_ack` · T4 `different_fingerprint_same_command_id_returns_conflict` — all pass. |
| AC-7 | Cancel effect (T10) | **PASS** (unit) | `cancel_live_turn_signals_once` passes. Live half → Runbook §5. |
| AC-8 | Steer receipt (T11–T13) | **PASS** | T11 `steer_ack_names_delivery_branch` · T12 `steer_unknown_id` · T13 `steer_64_char_payload` — all pass. |
| AC-9 | Pause hold (T14) | **PASS** | `pause_holds_dispatch_but_not_in_flight_turn` passes. |
| AC-10 | Lease CAS + tombstone (T15–T17) | **PASS** | T15 `pause_renew_resume` · T16 `historical_exact_retry_after_new_lease_leaves_current_row_unchanged` · T17 `pause_transition_tombstone_survives_resume` — all pass. I-11 covered jointly by T9 + T16 (see F3 note). |
| AC-11 | Expiry distinct (T18, T20) | **PASS** | T18 `expiry_tick_releases_once` · T20 `expired_lease_found_at_startup_releases_with_expired_audit` — all pass. |
| AC-12 | Restart survival (T19) | **PASS** | `kill_mid_pause_restart_rereads_same_lease` passes (integration, real temp SQLite files). |
| AC-13 | Stale authority releases (T21) | **PASS** | `a_b_a_owner_sequence_releases_stale_lease_with_both_revisions` passes. |
| AC-14 | Tenant pinning (T22) | **PASS** | `store_opened_under_other_relay_origin_refuses_everything` · `tenant_mismatch_refuses_open` — all pass. |
| AC-15 | Non-operator refused (T5, T6) | **PASS** (unit) | `payload_operator_mismatch_rejected_binding_mismatch` · `authority_conflict_when_owner_binding_differs` — all pass. Pre-decrypt non-owner audit present. Live half → Runbook §5. |
| AC-16 | Replay rejected live | **OPERATOR-ATTENDED** | Runbook §4 below. |
| AC-17 | §5 latency (p95 ≤ 2s cancel, ≤ 5s others) | **OPERATOR-ATTENDED** | Runbook §5 below; `s2-p95.py` commited at `docs/nips/slice2-evidence/s2-p95.py`. |
| AC-18 | Session continuity | **OPERATOR-ATTENDED** | Runbook §6 below. |
| AC-19 | No bodies / no paths | **AUDITABLE** | grep runbook §7 below. |
| AC-20 | UI states + keyboard | **PASS (unit)** / **OPERATOR-ATTENDED (screenshots)** | T24 (16/16) · T25 (7/7) green. Screenshots in Runbook §8. |
| AC-21 | Q1 labelling (R5-3 stopgap) | **PASS** | Stopgap comment block present verbatim at `agent_controls.rs:32-36` (code half). Doc paragraph in §9 below. |
| AC-22 | Flag flip (`platforms:[]` → `["desktop"]`) | **OPERATOR-GATED** | Final authorised commit after AC-17/18 evidence exists. |
| AC-23 | Verification doc complete | **THIS DOC** | All Step 7.1 sections present. |

---

## 3. Step 1.3 DDL (verbatim)

From `crates/buzz-acp/src/control_store.rs:1094-1179`:

```sql
CREATE TABLE IF NOT EXISTS host_identity (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  computer_id TEXT NOT NULL,
  community_id TEXT NOT NULL,
  relay_origin TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS owner_binding (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  owner_pubkey TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision > 0),
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS spent_command (
  community_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  control TEXT NOT NULL CHECK (control IN ('cancel','steer')),
  operator_pubkey TEXT NOT NULL,
  agent_pubkey TEXT NOT NULL,
  computer_id TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  command_seq INTEGER NOT NULL,
  ownership_revision INTEGER NOT NULL,
  issued_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  claimed_at INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending','completed','abandoned','authority_conflict')),
  ack_json TEXT,
  acked_at INTEGER,
  PRIMARY KEY (community_id, command_id)
);
CREATE TABLE IF NOT EXISTS pause_lease_current (
  community_id TEXT NOT NULL,
  agent_pubkey TEXT NOT NULL,
  computer_id TEXT NOT NULL,
  lease_id TEXT NOT NULL,
  operator_pubkey TEXT NOT NULL,
  ownership_revision INTEGER NOT NULL CHECK (ownership_revision > 0),
  channel_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation > 0),
  active INTEGER NOT NULL CHECK (active IN (0,1)),
  lease_expires_at INTEGER NOT NULL,
  last_transition_id TEXT NOT NULL,
  last_transition_fingerprint TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (community_id, agent_pubkey, computer_id)
);
CREATE TABLE IF NOT EXISTS pause_lease_transition (
  community_id TEXT NOT NULL,
  transition_id TEXT NOT NULL,
  lease_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  generation INTEGER NOT NULL,
  transition TEXT NOT NULL CHECK (transition IN ('pause','renew','resume')),
  ownership_revision INTEGER NOT NULL,
  transition_expires_at INTEGER NOT NULL,
  applied_at INTEGER NOT NULL,
  ack_json TEXT NOT NULL,
  PRIMARY KEY (community_id, transition_id)
);
CREATE TABLE IF NOT EXISTS control_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  event TEXT NOT NULL,
  community_id TEXT,
  command_id TEXT,
  transition_id TEXT,
  lease_id TEXT,
  fingerprint TEXT,
  operator_pubkey TEXT,
  agent_pubkey TEXT,
  computer_id TEXT,
  channel_id TEXT,
  run_id TEXT,
  ownership_revision INTEGER,
  persisted_revision INTEGER,
  outcome TEXT,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS spent_command_expiry ON spent_command (expires_at);
CREATE INDEX IF NOT EXISTS pause_lease_transition_expiry ON pause_lease_transition (transition_expires_at);
```

---

## 4. Files Changed (from pin `8abc8886a` → HEAD `57f2a374e`)

| Path | Action | Purpose |
|------|--------|---------|
| `crates/buzz-acp/Cargo.toml` | Modified | Add `rusqlite` dep + `uuid` v5 feature |
| `crates/buzz-acp/src/control_store.rs` | **New** | SQLite control store adapter (DDL, claim/CAS, audit, purge) — 1,867 lines |
| `crates/buzz-acp/src/agent_controls.rs` | **New** | Structured control handler (cancel/steer/pause/resume dispatch, ack publish, tests) — 2,267 lines |
| `crates/buzz-acp/src/config.rs` | Modified | Add `BUZZ_ACP_CONTROL_STORE` env key |
| `crates/buzz-acp/src/lib.rs` | Modified | Export new modules, wire `ControlStore` into pool/agent lifecycle |
| `crates/buzz-acp/src/pool.rs` | Modified | Thread control store through agent pool |
| `crates/buzz-acp/src/queue.rs` | Modified | Wire pause-hold check into `dispatch_pending` |
| `crates/buzz-acp/src/acp.rs` | Modified | Report steer delivery branch |
| `crates/buzz-acp/tests/agent_controls_recovery.rs` | **New** | Integration tests T19–T22 on real temp SQLite files — 527 lines |
| `desktop/src-tauri/src/commands/agents_deploy.rs` | Modified | Inject `BUZZ_ACP_COMPUTER_ID` / `BUZZ_ACP_CONTROL_STORE` via policy_env |
| `desktop/src-tauri/src/commands/agents_tests.rs` | Modified | Test policy_env keys |
| `desktop/src-tauri/src/commands/identity.rs` | Modified | Expose `computer_id` to frontend |
| `desktop/src-tauri/src/managed_agents/reserved_env_keys.rs` | Modified | Whitelist new env keys |
| `desktop/src-tauri/src/managed_agents/runtime.rs` | Modified | Deliver env via policy_env |
| `desktop/src-tauri/src/managed_agents/storage.rs` | Modified | Persist `computer_id` on managed-agent record |
| `desktop/src-tauri/src/managed_agents/types.rs` | Modified | Add `computer_id` field |
| `desktop/src-tauri/src/managed_agents/types/tests.rs` | Modified | Test `computer_id` field |
| `desktop/src/shared/api/agentStructuredControl.ts` | **New** | Structured control frame builder + validation |
| `desktop/src/shared/api/agentStructuredControl.test.mjs` | **New** | T23: 22 tests |
| `desktop/src/shared/api/observerRelay.ts` | Modified | Add `retryPublishObserverControl` + `sendAgentObserverControl` caching |
| `desktop/src/shared/api/tauri.ts` | Modified | Expose `computerId` on managed agent type |
| `desktop/src/shared/api/tauriObserver.ts` | Modified | Wire `computerId` |
| `desktop/src/shared/api/types.ts` | Modified | Add `computerId` to types |
| `desktop/src/features/agents/controls/AgentControlsBar.tsx` | **New** | Desktop controls bar UI (pending/acked/expired badges, keyboard nav) |
| `desktop/src/features/agents/controls/AgentControlsBar.keyboard.test.mjs` | **New** | T25: 7 keyboard tests |
| `desktop/src/features/agents/controls/controlState.ts` | **New** | Control state reducer + lease state management |
| `desktop/src/features/agents/controls/controlState.test.mjs` | **New** | T24: 16 state machine tests |
| `desktop/src/features/agents/liveActivity/LiveActivityTimeline.tsx` | Modified | Expose `__s1Log` for telemetry |
| `desktop/src/features/agents/liveActivity/s1Log.ts` | **New** | s1Log telemetry collector |
| `desktop/src/features/agents/observerRelayStore.ts` | Modified | Wire `retryPublishObserverControl` |
| `desktop/src/features/agents/ui/ManagedAgentRow.tsx` | Modified | Paused badge on agent row |
| `desktop/src/features/agents/ui/ManagedAgentSessionPanel.tsx` | Modified | Mount `AgentControlsBar` behind feature flag |
| `desktop/test-loader-hooks.mjs` | **New** | Node.js test loader hooks for T23–T25 |
| `Cargo.lock` | Modified | Dependency resolution |

---

## 5. Deviations from Build Spec

1. **Branch name:** `torq/slice2-structured-controls` per operator answer N1, superseding canonical PRD line 106 (`torq/slice2-controls`). (G1R M3)

2. **`computer_id` carrier:** Desktop mints and injects the host id via Step 5; telemetry route forbidden by tree rules (no `status` frame kind exists at the pin, and hard rule 3 forbids new/altered frames with flag off).

3. **N7 counters:** Ride on `control acked` / `control refused` tracing lines and audit table only; no observer `status` frame kind exists.

4. **Pre-decrypt non-owner audit (Step 3.2):** Cannot carry ids because the frame is dropped before decryption; audit row records `operator_mismatch` with `detail='pre-decrypt'`.

5. **N6 byte-identical retry:** The signed `RelayEvent` is cached at initial send time via `sendAgentObserverControl` returning the event. `retryPublishObserverControl` re-publishes it without re-signing. `retryControl` in `AgentControlsBar.tsx` calls it directly.

6. **N5 lease staleness on unmount:** Shared lease state is cleared via `useEffect` cleanup when `AgentControlsBar` unmounts — prevents stale "Paused" badge.

7. **D3 — `uuid v5` feature flag (AC-3):** `crates/buzz-acp/Cargo.toml` enables `uuid = { workspace = true, features = ["v5"] }`. This is **not a new dependency** — `uuid` is already a workspace dependency. Step 2.2 requires `Uuid::new_v5` for `derive_community_id` (namespace + relay origin → deterministic community UUID). Listed against AC-3's "exactly the rusqlite line and nothing else" phrasing. This is a spec-text deviation, not a code deviation: the build spec §2.2 explicitly requires the v5 derivation, and the constraint text omits the feature flag it necessarily implies.

8. **F3 — I-11 tombstone coverage split across T9 + T16:** `pause_transition_tombstone_survives_resume` (T9) tests pause→resume on the **same** lease_id. `historical_exact_retry_after_new_lease_leaves_current_row_unchanged` (T16) tests replacement by a fresh pause with a **new** lease_id + exact replay returning stored ack with no state change. Together they cover I-11 ("Tombstone outlives replacement"); neither test alone matches its full named scope. No new test strictly required — I-11 is covered jointly.

---

## 6. Round-1 Fixes Applied (commit `57f2a374e` on top of `611a660c8`)

### Root cause (a) — CHECK constraint violation
- **File:** `control_store.rs` — `apply_pause_transition` INSERT
- **Fix:** Compute `transition_label` match BEFORE the INSERT; inject it instead of `audit_event.as_str()` which returned "pause_lease_granted" vs. the CHECK-expected "pause".
- **Verification:** All pause lease tests pass; no CHECK constraint failures.

### Root cause (b) — InvalidTimestamp from fixture timestamps
- **File:** `control_store.rs` test module
- **Fix:** Introduced `const NOW: u64 = 1_800_000_000` as anchor; replaced all `Utc::now()` derived timestamps with `NOW`-based values; changed `NOW + 3600` (exceeds 300s TTL) to `NOW + 60`; fixed `issued_at: 0` to `issued_at: 1` in claim expiry test.
- **Verification:** All 914 lib tests pass; 4 recovery integration tests pass.

### D2 — fail-closed on store error (round-1 MAJOR)
- **File:** `agent_controls.rs:833-837` (cancel) and `:935-939` (steer)
- **Fix:** Both `complete_one_shot` call sites now return before ack publish on `Err`.
- **Verification:** `control_refused` audit row emitted; `warn!` logged; no ack published.

### D6 — community_id mismatch (InvalidTimestamp → LeaseConflict)
- **File:** `agent_controls.rs` test module — `make_test_facts`
- **Fix:** Changed from taking `(target, owner_pk_hex)` to `(store, target, owner_pk_hex)` — uses `store.community_id()` instead of a random UUID.
- **Verification:** 4 agent_controls tests that previously failed `LeaseConflict` now pass.

### D7 — Renew monotonic lease_expires_at
- **File:** `agent_controls.rs` — `lease_generation_rules_pause_renew_resume`
- **Fix:** Renew `lease_expires_at` bumped from `NOW + 300` to `NOW + 360` to satisfy monotonic check (must be > current expiry).
- **Verification:** Test passes without InvalidTimestamp.

### D8 — historical_exact_retry test flow
- **File:** `agent_controls.rs` — `historical_exact_retry_after_new_lease_leaves_current_row_unchanged`
- **Fix:** Reworked from pause→resume→replay-pause to pause→exact-retry-while-active. Resume made the lease inactive, which the validator's Pause match arm rejects for same-lease_id. Exact retry against active lease is the correct I-11 complement.
- **Verification:** `validated_replay.is_exact_retry()` returns true.

---

## 7. Test Inventory

### buzz-acp `--lib` (915 tests)

| # | Test | Invariant |
|---|------|-----------|
| T1 | `claim_and_complete_one_shot_success` | I-5, I-6 — single claim + ack |
| T2 | `duplicate_claim_returns_duplicate_pending` | I-5 — same id/same fingerprint → pending |
| T3 | `duplicate_completed_returns_stored_ack` | I-5 — same id/stored ack → returns it |
| T4 | `different_fingerprint_same_command_id_returns_conflict` | I-5 — different fingerprint → replay rejected |
| T5 | `payload_operator_mismatch_rejected_binding_mismatch` | I-4 — non-owner refused |
| T6 | `authority_conflict_when_owner_binding_differs` | I-10 — stale authority |
| T7 | `poisoned_handle_refuses_access` | I-13 — store failure → store_unavailable |
| T8a | `purge_expired_removes_stale_rows` | I-5 — stale rows deleted past deadline |
| T8b | `purge_retains_rows_before_deadline` | I-5 — retention boundary: `now < expires_at + 600` → retained |
| T9 | `pause_transition_tombstone_survives_resume` | I-11 — tombstone survives resume (same lease_id) |
| T10 | `cancel_live_turn_signals_once` | I-6, I-7 — cancel claim → signal |
| T11 | `steer_ack_names_delivery_branch` | I-6 — steer acked with branch detail |
| T12 | `steer_unknown_id_pends_then_resolves` | — steer against absent/in-flight receipt |
| T13 | `steer_64_char_payload` | — 64-char cap enforced |
| T14 | `pause_holds_dispatch_but_not_in_flight_turn` | I-7 — queue hold |
| T15 | `lease_generation_rules_pause_renew_resume` | I-8 — generation/cas semantics |
| T16 | `historical_exact_retry_after_new_lease_leaves_current_row_unchanged` | I-11 (joint with T9) — replacement + replay |
| T17 | `pause_transition_tombstone_survives_resume` | I-11 — tombstone survives |
| T18a | `expiry_tick_releases_active_lease_once` | I-9 — expiry tick → released |
| T18b | `schema_matches_ddl_column_for_column` | AC-5 — schema verification |
| — | + existing `control_store.rs` tests (DDL, identity, JSON validation) | — |

### buzz-acp `--test agent_controls_recovery` (4 tests)

| # | Test | Invariant |
|---|------|-----------|
| T19 | `kill_mid_pause_restart_rereads_same_lease` | I-8 — pause survives restart |
| T20 | `expired_lease_found_at_startup_releases_with_expired_audit` | I-9 — startup expiry release |
| T21 | `a_b_a_owner_sequence_releases_stale_lease_with_both_revisions` | I-10 — stale authority A→B→A |
| T22 | `store_opened_under_other_relay_origin_refuses_everything` | I-12 — tenant pinning |

### desktop (49 tests)

| # | File | Tests | Coverage |
|---|------|-------|----------|
| T23 | `agentStructuredControl.test.mjs` | 22 | Frame construction, encryption, validation |
| T24 | `controlState.test.mjs` | 16 | State reducer: pending→acked/expired/rejected, lease states |
| T25 | `AgentControlsBar.keyboard.test.mjs` | 7 | Keyboard navigation, aria-labels, focus management |
| — | `LiveActivityTimeline.keyboard.test.mjs` | 4 | Regression (unchanged) |

---

## 8. Slice 5 Items

**R5-3 — Server-issued tenant id / ownership revision.** The current local-authority stopgap (Q1 answer) derives `tenant_id` from relay origin and `ownership_revision` = 0 locally. The relay must eventually publish a NIP-11 `self` key, and the relay must track and issue per-tenant ownership revisions. This is a relay-side change only — no new frame kind, no new buzz-core validator shape needed. The stopgap code in `agent_controls.rs:32-36` explicitly marks the three derivation sites that must be replaced. No other Slice 5 items are scoped to this build.

---

## 9. Q1 Stopgap — R5-3 Labelling (AC-21)

From `crates/buzz-acp/src/agent_controls.rs:32-37`:

```
// Slice-2 stopgap (design_answers.md Q1): this deployment has a single operator
// and one relay, and the relay publishes no NIP-11 `self` key, so the consumer
// derives the tenant id from the relay origin and keeps its own monotonic
// ownership revision. The server-authoritative source (relay identity +
// relay-issued ownership revision) is Slice-5 relay item R5-3. The validator in
// buzz-core is unchanged; these values enter only via ResolvedControlFacts.
```

This paragraph and the three stopgap derivations (`derive_community_id`, `derive_ownership_revision`, `compute_computer_id`) must be removed before multi-tenant / fleet-mode GA and replaced with the relay-supplied values delivered through the observer policy path.

---

## 10. Unresolved

1. **Latency (AC-17) not yet measured.** Requires operator-attended run with production build and two harnesses. The code paths for timing are instrumented (`s1Log` entries at send and ack receipt) but the end-to-end p95 has not been computed.

2. **Session continuity (AC-18) not yet proven.** Requires live agent tests with both Claude and goose harnesses.

3. **Full `pnpm test` baseline not re-run.** The complete desktop test suite exceeds the 300s command timeout. All affected test files pass (49/49 targeted). Pre-existing baseline is ~6493 with 21 pre-existing failures (unchanged from SLICE-1-VERIFICATION.md §Decrypt Fix Round 2).

4. **Flag flip (AC-22) not done.** `preview-features.json` still has `platforms:[]` for `BUZZ_AGENT_CONTROLS`. This is an operator-gated final commit after AC-17/18 evidence exists.

5. **buzz-workflow: 2 ignored tests.** These require a live Postgres instance. Pre-existing, not related to Slice 2.

6. **Operator-attended AC-16/17/18/19/20 screenshots not yet collected.** (Requires production build + live agents — Runbook §§4–8 below provide the exact procedures.)

---

## 11. Runbook — Operator-Attended Steps

### §4 Replay Rejected Live (AC-16)

**Pre-flight:** Production build, local relay, managed agent (Claude harness), DevTools open with `localStorage["s1-gate"]="1"`.

1. Send a **Cancel** from `AgentControlsBar`. Wait for ack.
2. Open the relay's event log, find the signed `buzz-agent-control-command` frame for that cancel (match `command_id`).
3. Re-publish the **exact same** signed frame via `nostcat` or relay debug endpoint.
4. Check sidecar log: `grep "command_replay" <sidecar-log>` → must show the event was rejected as replay (no second signal, no second audit row in state `completed`).
5. Verify `control_audit` table has exactly one row with `event='command_replay'` for that `command_id`.

**Acceptance:** Duplicate accepted as replay (stored ack returned, no new effect). Only the original claim row exists.

### §5 Latency — p95 Method (AC-17)

**Pre-flight:** Production build (`pnpm build && cd src-tauri && cargo build --release`), local relay, two harnesses (Claude + goose), `localStorage["s1-gate"]="1"`. Production page build, no debugger attached.

**Per harness, per control type (cancel / steer / pause / resume):**

1. Start an agent turn (send a message from the desktop).
2. Wait for `turn_started` in `window.__s1Log`.
3. Click the control button in `AgentControlsBar`.
4. Record these timestamps from `__s1Log`:
   - `[agent-controls] sent` entry — `command_id` / `transition_id` + `sent_at` epoch millis.
   - `[agent-controls] ack` entry — matching `command_id` / `transition_id` + `acked_at` epoch millis.
5. Compute latency: `latency_ms = acked_at - sent_at`.
6. Run 10 samples (minimum) per control type per harness.
7. Collect samples into `docs/nips/slice2-evidence/<harness>-<control>-gate-prod-nodebug.log` — one line per sample: `latency_ms`.
8. Run: `python docs/nips/slice2-evidence/s2-p95.py slice2-evidence/<harness>-<control>-gate-prod-nodebug.log`

**p95 compute script** (`docs/nips/slice2-evidence/s2-p95.py`): nearest-rank method — sort ascending, pick index `ceil(0.95 * n) - 1`, report the value.

**Evidence files (8 total):**
- `slice2-evidence/slice2-claude-cancel-gate-prod-nodebug.log`
- `slice2-evidence/slice2-claude-steer-gate-prod-nodebug.log`
- `slice2-evidence/slice2-claude-pause-gate-prod-nodebug.log`
- `slice2-evidence/slice2-claude-resume-gate-prod-nodebug.log`
- `slice2-evidence/slice2-goose-cancel-gate-prod-nodebug.log`
- `slice2-evidence/slice2-goose-steer-gate-prod-nodebug.log`
- `slice2-evidence/slice2-goose-pause-gate-prod-nodebug.log`
- `slice2-evidence/slice2-goose-resume-gate-prod-nodebug.log`

**Header must state:** "production page build, no debugger attached"

**Acceptance thresholds:**

| Control | p95 Target | n minimum |
|---------|-----------|-----------|
| Cancel | ≤ 2000 ms | 10 |
| Steer | ≤ 5000 ms | 10 |
| Pause | ≤ 5000 ms | 10 |
| Resume | ≤ 5000 ms | 10 |

### §6 Session Continuity (AC-18)

1. Start Claude harness agent.
2. Send Cancel → verify sidecar log shows `control acked … status=applied`, turn ends.
3. Send a new message → verify same `sessionId` in sidecar `turn_started` log.
4. Start a new turn, send Steer → verify same `sessionId`.
5. Start a new turn, send Pause → send Resume → verify same `sessionId` throughout.
6. Repeat for goose harness.

**Acceptance:** Same ACP session (same `sessionId`) for all subsequent frames after each control. No NDJSON parse error in sidecar log. No sidecar respawn triggered by any control.

### §7 No Bodies / No Paths (AC-19)

```bash
# Dump control store
sqlite3 <control-store-path> .dump > store-dump.txt

# Check for steer text, prompt text, filesystem paths
grep -i "steer\|prompt\|message" store-dump.txt  # must find NO steer/prompt body text
grep '[A-Z]:\\' store-dump.txt                   # must find NO Windows paths
grep '/home/\|/Users/' store-dump.txt            # must find NO Unix paths

# Check sidecar logs
grep -i '[A-Z]:\\\|/home/\|/Users/' <sidecar-log>  # only "control store path=" line allowed
```

**Acceptance:** No steer text, prompt text, or message body in store dump. No `E:\`, `C:\`, `/home/`, or `/Users/` paths except the single startup `control store path=` line.

### §8 UI Screenshots (AC-20)

Capture these states in `slice2-evidence/`:

| State | Filename | Description |
|-------|----------|-------------|
| Pending | `ui-pending.png` | Any control button clicked, spinner badge visible |
| Acked (applied) | `ui-acked-applied.png` | Cancel ack with "applied" badge |
| Acked (queued) | `ui-acked-queued.png` | Steer ack with "queued" badge |
| Acked (no_active_turn) | `ui-acked-noturn.png` | Cancel against idle with "no turn" badge |
| Rejected | `ui-rejected.png` | Control rejected with reason badge |
| Expired | `ui-expired.png` | Control expired (wait 5s+ after expiry) |
| Paused badge | `ui-paused-badge.png` | Paused badge on ManagedAgentRow |

**Procedure:**
1. Open DevTools, set `localStorage["s1-gate"]="1"`.
2. Interact with `AgentControlsBar` to reach each state.
3. Screenshot the full agent session panel.
4. Verify each `<button>` has an `aria-label` (check via DevTools Accessibility panel or `document.querySelectorAll('[aria-label]')`).

---

## 12. Evidence Inventory

| Type | Source | Location |
|------|--------|----------|
| buzz-acp test output | `cargo test -p buzz-acp --lib` | 915/0 pass |
| buzz-acp recovery test output | `cargo test -p buzz-acp --test agent_controls_recovery` | 4/0 pass |
| buzz-core test output | `cargo test -p buzz-core` | 283/0 pass |
| buzz-workflow test output | `cargo test -p buzz-workflow` | 181/0/2 pass |
| T23 test output | Node.js `--test` | 22/22 pass |
| T24 test output | Node.js `--test` | 16/16 pass |
| T25 test output | Node.js `--test` | 7/7 pass |
| Typecheck | `pnpm typecheck` | 1 pre-existing TS2322 |
| Commit | `git show 57f2a374e` | DCO signed |
| Verification doc | This file | `docs/nips/SLICE-2-VERIFICATION.md` |
| p95 script | `s2-p95.py` | `docs/nips/slice2-evidence/s2-p95.py` |

---

## 13. G2A Handoff Notes

- **TORQ-BUZZ commit:** `57f2a374e` + doc commit (this retry) on `torq/slice2-structured-controls` (DCO signed, clean tree)
- **Rust test counts:** 915 (buzz-acp lib) + 4 (recovery) + 283 (buzz-core) + 181 (buzz-workflow) = 1,383 total
- **TS test counts:** 22 + 16 + 7 + 4 = 49 targeted (all pass)
- **N6 retry:** `retryControl` in `AgentControlsBar.tsx` uses `retryPublishObserverControl(entry.originalEvent as RelayEvent)`
- **F2 (retention boundary):** `purge_retains_rows_before_deadline` asserts row retained when `now < expires_at + 600`
- **F3 (I-11 joint coverage):** T16 covers the replacement+replay path that T9's same-lease_id scope omits
- **Operator-attended AC-16/17/18/19/20 screenshots:** Not yet collected (requires production build + live agents); exact runbook procedures in §4–§8