# Slice 3 (Routines) — Operator-Attended Live Runbook

**Status: WRITTEN, NOT EXECUTED.** This document is the exact procedure an operator runs to
close AC-6 (live DDL check), AC-15/16 if 5.6/5.7 are added first, AC-21, and AC-22 of
`build_spec.md`. No step here has been run this session. The builder session that wrote this
runbook was explicitly instructed not to touch the permanent relay, the live database, or
running sidecars — every step below requires operator hands and operator authorization.

**Before starting:** confirm `torq/slice3-routines` is at the commit this runbook was written
against (`dab7e21f9` or later on the same branch), and that Steps 1–8 (schema through this
verification pass) are merged into the branch you are about to build the relay from.

Record every timestamp, log excerpt, and screenshot named below into
`docs/nips/slice3-evidence/`, one file per numbered step (e.g. `01-relay-bringup.log`,
`03-first-fires.md`). If any step's expected observation does not match, stop, do not proceed
to the next step, and record the mismatch instead of working around it.

---

## 1. Relay bring-up (Q1.2)

1. `cargo build --release --bin buzz-relay` on `torq/slice3-routines`.
2. Back up the running binary: copy `E:\TORQ-BUZZ\bin\buzz-relay.exe` →
   `bin\buzz-relay.exe.bak-<date>`.
3. Copy the newly built binary into `bin\buzz-relay.exe`.
4. Add a stable `BUZZ_RELAY_PRIVATE_KEY` to `E:\TORQ-BUZZ\config\relay.env` — generate it
   locally with `scripts/ensure-local-relay-key.sh` (or the local equivalent); **never paste
   the key anywhere, including into this evidence log.**
5. Set `BUZZ_AUTO_MIGRATE=true` for the first start (or run `buzz-admin migrate` directly).
6. **Restart only through the scheduled task `TORQ-Buzz-PermanentRelay`** — never
   `Start-Process` the relay directly from an agent or operator shell session.
   - **Log visibility (S3-7):** the routine settlement/auto-pause/skip INFO lines
     (`routine settled`, `routine_auto_paused`, `routine_skipped_busy`,
     `routine_outcome_rejected`, `routine_fire_deduplicated`) live under the
     **`buzz_workflow`** tracing target (crate `buzz-workflow`, embedded in the relay
     binary), which `RUST_LOG=buzz_relay=info` alone filters out. Set
     **`RUST_LOG=buzz_relay=info,buzz_workflow=info`** in `config\relay.env` before
     restarting, or those lines will not appear in the relay log for the steps below.
7. Confirm:
   - NIP-11 shows a non-empty `self` field (desktop fetch, or
     `curl -H "Accept: application/nostr+json" <relay-url>`).
   - Migration head is `0045` (`buzz-admin migration-status` or equivalent).
   - The relay startup log contains the line `workflow invoke_agent enabled=false` (the
     `BUZZ_WORKFLOW_INVOKE_AGENT` switch stays off until step 3).
8. Record: binary commit hash, the relay's `self` pubkey, and the migration head.

**Rollback:** reverse the binary copy (`bak` → `buzz-relay.exe`), restart the scheduled task.

## 2. Two-switch flag-off proof (Q1.3)

Prove both switches independently gate the feature before turning either on:

- **(a) Relay env unset, desktop flag on:** from the desktop app with `BUZZ_ROUTINES`
  enabled via Experiments, attempt to save a routine (`invoke_agent` step) from the workflow
  dialog. Expect the save to be refused with the relay's exact string:
  `rejected: invoke_agent is not enabled on this relay`.
- **(b) Relay env `=1`, desktop flag off:** with the relay switch on but the desktop flag off,
  post a `buzz-routine` fenced code block in a channel. Expect it to render as a plain YAML
  code block — no "Review routine" button, no Routines filter chip on the Workflows screen,
  and (checked via DevTools network/IPC log) no `get_routine_state` call issued at all.

Record both proofs (screenshot or DevTools log excerpt) before proceeding.

## 3. Live fire, both switches on

1. Set `BUZZ_WORKFLOW_INVOKE_AGENT=1` in `config\relay.env`, restart the scheduled task.
   Confirm the startup log now shows `workflow invoke_agent enabled=true`.
2. Enable `BUZZ_ROUTINES` in the desktop Experiments panel.
3. Create a fresh private channel per agent harness under test (one for Claude, one for
   goose).
4. In each channel, ask the managed agent for recurring work. Expect a `buzz-routine` fenced
   block in its reply.
5. Click "Review routine" → confirm the create-workflow dialog opens seeded with the block's
   YAML → set a 15-minute interval (or cron instants 15 minutes apart) → save enabled.
6. Wait for two fires. For each fire, record:
   - The wake event's full tag set (should be exactly the 10 tags per AC-5).
   - The `turn_started` frame's `triggeringEventIds` containing the wake's event id.
   - The agent's reply.
   - The outcome event (kind:9, agent-signed, threaded under the wake).
   - The run reaching `Completed` and the `routine_state` row's `last_fired_at`/`last_outcome`.
   - The panel showing the routine's state via the Routines filter and detail panel.

## 4. Relay kill mid-window

1. Trigger a due fire and watch the relay log for the `Cron trigger fired` line (claim won).
2. Stop the scheduled task immediately after that line but before the dispatch's wake event
   is confirmed posted (a tight window — may require a couple of attempts).
3. Restart the task.
4. Confirm: exactly one wake event exists for that fire, exactly one `routine_dispatches` row
   exists, and no duplicate post occurred. If the kill landed after the claim but before the
   wake was posted, expect the sweeper to eventually fail that dispatch `routine_timeout`
   rather than a re-post — record which case actually happened.

## 5. Refused draft + idle disabled routine

1. As the agent (via `buzz workflows create`, not through the desktop), attempt to submit an
   `invoke_agent` definition directly. Expect the exact refusal string:
   `forbidden: invoke_agent routines must be signed by the target agent's owner`.
2. Confirm the refusal persists across two due windows (no `routine_dispatches` row appears
   for it in either window).
3. Save a routine with `enabled: false`. Confirm it produces no `routine_dispatches` row
   across two due windows while disabled.

## 6. Per-run budget breach

1. Edit the routine's `token_budget_per_run` down to a value below what one turn's real usage
   will be.
2. Trigger a fire and observe:
   - **goose:** the mid-turn cancel path is not implemented this slice (Deviation 3 in
     SLICE-3-VERIFICATION.md §5) — expect turn-end enforcement instead: the breach is
     detected after the turn completes, the worker respawns per the known goose
     cancel-drain-timeout behavior (Slice 2 H1). Record the respawn as expected, not a defect.
   - **Claude:** the breach is detected and enforced at turn end.
3. Confirm: exactly one outcome event posts, carrying `budget_exceeded_per_run`; the run
   reaches `Failed`; `consecutive_failures` increments by 1.

## 7. Daily budget breach

1. Set `token_budget_per_day` just above the usage of one run (so a second run in the same
   UTC day crosses it).
2. Let the routine fire twice within one UTC day.
3. Confirm: the second run's outcome is `budget_exceeded_daily`; `workflows.status` becomes
   `disabled` with `paused_reason='daily_budget'`; exactly one notice event posts to the
   result channel.
4. Re-enable the routine from the Workflows panel. Confirm: `status='active'` **and** the
   `enabled` column is `TRUE` (query the DB directly, not just the panel — this is the G1R
   F-1 case), `consecutive_failures` resets to 0, and the routine fires again on the next due
   instant.

## 8. Ten-strike auto-pause

1. Induce ten consecutive failures — either stop the agent so the sweeper times each run out,
   or give it a prompt that reliably makes it fail.
2. Confirm: on the tenth consecutive `failed*` outcome, `status='disabled'`,
   `paused_reason='strikes'`, exactly one notice event posts, and `routine_auto_paused` is
   logged once (INFO level, relay log).

## 9. Slice 2 interplay

1. With a Slice 2 pause lease active on the target agent's channel, trigger a routine fire.
   Confirm the resulting wake is held (`QueueHoldState`) rather than dispatched immediately,
   and dispatches once the pause lease resumes.
2. Send a structured cancel against a routine-started turn. Confirm it acks `applied` through
   the unchanged Slice 2 path.

   **Note:** the two named unit tests for this interplay (5.6:
   `routine_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume` and
   `structured_cancel_on_routine_started_turn_acks_applied`) were not written in this slice's
   Steps 1–6 (see SLICE-3-VERIFICATION.md §5, Deviation 4). This live check is therefore the
   *only* verification of this interplay claim until those tests exist — treat a failure here
   as a blocker, not a nice-to-have.

## 10. Latency measurement

1. Collect at least 10 fires per harness (Claude and goose) — at the 15-minute scheduling
   minimum, this is at least 150 minutes of wall-clock per harness; plan the session
   accordingly, or use cron instants spaced 15 minutes apart to pack the fires closer together
   within that constraint.
2. For each fire, pair `routine_dispatches.dispatched_at` (or `scheduled_workflow_fires.claimed_at`
   as fallback) with the `turn_started` observer frame whose `triggeringEventIds` contains the
   wake's event id.
3. Compute the fire-to-inject latency for each pair; report the nearest-rank p95 across all
   fires per harness.
4. Run this against a production page build with no debugger attached (per Slice 1's finding
   that an attached debugger and React's DEV profiler both distort timing by seconds).
5. Target: p95 ≤ 60 s. Record the actual p95 and the raw pairs regardless of whether the
   target is met.

---

## Evidence checklist

Every item below must have a corresponding file or excerpt in `docs/nips/slice3-evidence/`
before this runbook is considered closed:

- [ ] Relay bring-up: binary commit, `self` pubkey, migration head, rollback path confirmed
- [ ] Two-switch flag-off proof (both directions)
- [ ] Two fires per harness with full tag/latency/outcome chain
- [ ] Relay-kill-mid-window: no duplicate wake, sweeper or re-post outcome recorded
- [ ] Agent-signed draft refused (exact string), across two windows
- [ ] Disabled routine idle across two windows
- [ ] Per-run breach (goose respawn noted, Claude turn-end enforcement)
- [ ] Daily breach: one notice, re-enable restores firing (column `enabled=TRUE` verified)
- [ ] Ten-strike auto-pause: one notice, `routine_auto_paused` logged once
- [ ] Pause-hold and structured-cancel interplay
- [ ] Fire-to-inject p95 ≤ 60 s over ≥10 fires per harness, raw data attached
- [ ] `grep` audit (AC-22): no prompt substring, no reply substring, no usage-count field name
      (`totalTokens`, `inputTokens`, `outputTokens`, any `cost` field) anywhere in
      `slice3-evidence/*.log` or the relay log excerpt. Configured budget values
      (`per_run`, `per_day`) in sidecar INFO lines are explicitly permitted (I-14) — do not
      flag them.
