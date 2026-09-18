# Slice 3 runbook steps 7-8: daily budget and ten-strike auto-pause (Claude), started 2026-09-14T14:15Z

Routines created 14:15:33Z (s3-gate-claude-strikes 5dff1e15, budgets 1000/3000000: every run breaches per_run) and 14:15:45Z (s3-gate-claude-daily b3f7ddf1, budgets 149000/150000: first run ~138k passes, second run breaches the day). Both interval 15m in channel fde9a0fb, same agent.

14:31:16Z both fired in the same second.
- strikes run b3febdc6: `routine prompt received` 14:31:16.26Z, `routine budget breached kind="per_run"` 14:31:44.40Z, outcome budget_exceeded_per_run posted, settled 14:31:44.87Z, consecutive_failures=1. (Turn took 28 s instead of ~4 s because of the injected steer below.)
- daily wake 5bb044ca: consumed as a mid-turn steer (`steer received ... event_id=5bb044ca...`, `steer success`) because the sidecar's mention mode is `steer` and the strikes turn was in flight. No routine turn, no outcome; the dispatch stays open until the 1800 s sweeper. DEFECT S3-5 (routed): routine wakes must always queue as their own prompt, never steer.
Subsequent fires recorded below.

## Step 8 result: ten-strike auto-pause (strikes routine 5dff1e15)
Fires and outcomes (all budget_exceeded_per_run, per_run=1000): 14:31:16, 14:46:15, 15:01:16, 15:16:17, 15:31:17, 15:46:18, 16:01:17, 16:16:18, 16:31:18, 16:46:19Z (settled 16:46:32.63Z). After the tenth: routine_state consecutive_failures=10, paused_reason='strikes', paused_at 16:46:32.63Z; workflows.status='disabled'; exactly one relay-signed kind:9 notice in the result channel at 16:46:33Z: "Routine \"s3-gate-claude-strikes\" was auto-paused after 10 consecutive failures. Re-enable it from the Workflows screen." (no mention tag, cannot wake anyone). PASS.
Re-enable check (runbook 7.4 / G1R F-1) recorded below.

## Re-enable after auto-pause (runbook 7.4, G1R F-1)
- Workflows card at 16:47Z still showed "Active" with the toggle ON for the auto-paused routine (s3-08-reenable.png); clicking the toggle turned the definition off (updated_at 16:47:39Z, status still disabled); clicking it on again produced no update (S3-8, routed).
- Definition save via update_workflow with enabled:true at 16:48:58Z: relay `routine approved`; workflows.status='active', enabled column TRUE, definition enabled=true; routine_state consecutive_failures=0, paused_reason NULL. PASS for the re-enable rule itself (F-1: column TRUE verified directly in the DB).
- Routine disabled again by the operator right after (definition enabled:false) to stop further strikes.
Relay N5 audit lines for settle/auto-pause are missing (S3-7, routed): the only routine lines in the relay log are created/approved/fired.

## Step 7 daily budget (sidecar 90dbc73f9), 2026-09-15
- 01:51 fire: wake lost to S3-5b (swept as timeout 02:21:01Z, strike). 02:06 fire: `routine_skipped_busy` (open dispatch), no strike.
- 02:21:02Z run succeeded; sidecar routine_daily_usage(b3f7ddf1, 2026-09-15) = 148606 tokens (per_run 149000 / per_day 150000).
- 02:36:02Z run: `routine budget breached kind="per_run"` -- this turn exceeded 149000 on its own, so the per-run check tripped before the daily check (per-run is evaluated first, as specified); outcome budget_exceeded_per_run, settled 02:36:08.86Z.
- Budgets raised to 250000/250000 at 02:3xZ so the next run passes per-run and the day total (148606 + ~150k) trips the daily cap.
- 02:51:02Z run (budgets 250000/250000): `routine budget breached kind="per_run"` again -- the turn's harness total exceeded 250k (the wake prompt carries the growing channel history, so per-turn usage rose from 138k to >250k across the day). Daily accounting unchanged at 148606 because breached runs are not added. Budgets set to 1000000/1000000 at 02:5xZ so the day total crosses on about the third following run.
Observation for the doc: with Claude, per-run usage is dominated by context (cache read/write), grows with channel history, and is not a good proxy for "work done"; budgets for real routines should start in the high hundreds of thousands and the daily cap should be sized from observed per-run totals.
- 03:06:02Z run (budgets 1000000/1000000): succeeded, settled 03:06:10.59Z. Budgets lowered to 500000/500000 at 03:06:36Z so the day crosses on the second following run.
- 03:21:02Z run (500000/500000): succeeded, settled 03:21:09.01Z; day total 479,075 (per_run passes, day still under the cap).
- 03:36:03Z run 3f43afa7 (500000/500000): sidecar `routine prompt received` 03:36:03.20Z, `routine budget breached run_id=3f43afa7... kind="per_day"` 03:36:10.28Z, `routine outcome posted ... outcome="budget_exceeded_daily"` (event 090e1a34...) 03:36:10.34Z. Relay: `routine settled` outcome=budget_exceeded_daily strikes=1 latency_ms=7576 at 03:36:10.42Z, then `routine_auto_paused` paused_reason=daily_budget at 03:36:10.45Z.
- DB after: routine_dispatches 3f43afa7 outcome=budget_exceeded_daily; routine_state consecutive_failures=1, last_outcome=budget_exceeded_daily, paused_reason=daily_budget, daily_notice_day=2026-09-15; workflows.status=disabled with the definition still enabled=true (auto-pause does not touch the definition flag, as designed; the desktop toggle/badge read effective state per S3-6/S3-8).
- Exactly one relay-signed kind:9 notice in the result channel (event 36106d72..., 03:36:10Z): "Routine \"s3-gate-claude-daily\" was auto-paused: daily token budget reached. It stays paused until you re-enable it." No mention tag.
Step 7 PASS (per_day breach -> budget_exceeded_daily, auto-pause with paused_reason daily_budget, one notice, daily_notice_day set). The routine stays disabled; not re-enabled after this check.
