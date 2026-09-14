# Slice 3 runbook steps 7-8: daily budget and ten-strike auto-pause (Claude), started 2026-09-14T14:15Z

Routines created 14:15:33Z (s3-gate-claude-strikes 5dff1e15, budgets 1000/3000000: every run breaches per_run) and 14:15:45Z (s3-gate-claude-daily b3f7ddf1, budgets 149000/150000: first run ~138k passes, second run breaches the day). Both interval 15m in channel fde9a0fb, same agent.

14:31:16Z both fired in the same second.
- strikes run b3febdc6: `routine prompt received` 14:31:16.26Z, `routine budget breached kind="per_run"` 14:31:44.40Z, outcome budget_exceeded_per_run posted, settled 14:31:44.87Z, consecutive_failures=1. (Turn took 28 s instead of ~4 s because of the injected steer below.)
- daily wake 5bb044ca: consumed as a mid-turn steer (`steer received ... event_id=5bb044ca...`, `steer success`) because the sidecar's mention mode is `steer` and the strikes turn was in flight. No routine turn, no outcome; the dispatch stays open until the 1800 s sweeper. DEFECT S3-5 (routed): routine wakes must always queue as their own prompt, never steer.
Subsequent fires recorded below.
