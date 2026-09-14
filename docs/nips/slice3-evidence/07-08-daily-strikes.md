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
