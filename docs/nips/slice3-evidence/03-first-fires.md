# Slice 3 runbook step 3: live fires (Claude), 2026-09-14

Routine 8bf80c93-d687-41ae-8c14-a8d965678fc5 (s3-gate-claude-routine, interval 15m, budgets 20000/200000) created 05:47:29Z via the desktop create_workflow command (agent-drafted block unavailable, see 02-flag-off-proof.md). Relay 81ffcc0f1, invoke_agent enabled=true.

| scheduled_for | claimed_at | dispatch | wake event | agent turn_started | outcome |
|---|---|---|---|---|---|
| 06:00:00Z | 06:02:52.71Z | run 58ba3d1f, idem s3-claude-1789365772 | 152c3635... | ~06:02:55Z (pool ready 06:02:55.98Z) | timeout at 06:32:53Z (sweeper), strike 1 |
| 06:15:00Z | 06:17:52.94Z | none (previous dispatch open: N6 routine_skipped_busy) | - | - | - |
| 06:30:00Z | 06:32:53.19Z | run 563fa1f2, idem s3-claude-1789367572 | 46313c20... | 06:32:56Z (desktop journal turn_started, triggeringEventIds = wake id) | open at 06:48Z |
| 06:45:00Z | 06:47:53.46Z | none (busy) | - | - | - |

Wake event tags (stored, verbatim): p owner, h channel, buzz:workflow true, buzz:workflow-owner, p agent, buzz:workflow-mention agent, buzz:routine-run, buzz:routine, buzz:routine-idem, ["buzz:routine-budget","20000","200000"] — 10 tags (AC-5). Prompt body carries the trailing `routine-run: <run_id>` line (visible in the channel).

Claim-to-inject: 06:32:53.19Z (claimed_at) -> 06:32:56Z (turn_started) = ~3 s. Note: claimed_at trails scheduled_for by ~2 m 53 s on every fire (06:00 -> 06:02:52 etc.); the §12 number in the spec is measured from claimed_at, but the scheduler's own lag should be explained by the builder (expected ≤ 60 s tick).

Agent replies: both wakes answered within ~20 s as ordinary threaded kind:9 replies (events 7be701e9... 06:03:12Z and c1936068... 06:33:13Z) carrying only h, e(reply) and auth tags — NO buzz:routine-run / buzz:routine-outcome tags, so the relay settlement branch never matched and run 1 was failed as routine_timeout by the sweeper (routine_state.consecutive_failures=1).

ROOT CAUSE (Defect S3-2, blocker): wire-contract mismatch on the budget tag. The relay emits it as two values, ["buzz:routine-budget", per_run, per_day] (spec text), while crates/buzz-acp/src/routine.rs parse_routine_binding reads one value and splits it on ',' (`budget.split(',')`), so per_day is missing, the parser returns None, the wake is queued as a plain prompt, no budget tracking happens and post_routine_outcome never runs. The sidecar log shows no routine lines at all. Every routine run therefore times out; strikes accumulate one per 30 minutes.

Status at 06:48Z: routine disabled by the operator (definition enabled:false) to stop strike accumulation until the fix; re-run of steps 3-4 and 6-10 after the sidecar fix.
