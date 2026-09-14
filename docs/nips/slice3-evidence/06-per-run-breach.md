# Slice 3 runbook step 6: per-run budget breach (Claude), 2026-09-14T13:12Z, sidecar 452e45150

Routine re-enabled at 13:11:28Z with budgets 20000/200000 (re-enable reset consecutive_failures 1 -> 0 and cleared paused_reason: AC-12b/F-1 observed live). Fire claimed 13:12:00.68Z (interval re-anchored to the re-enable), wake 4aafdf4b..., dispatch run a577db04.

Sidecar log (INFO, verbatim fields):
- 13:12:00.782Z routine prompt received run_id=a577db04-... routine_id=8bf80c93-... per_run=20000 per_day=200000
- 13:12:13.718Z routine budget breached run_id=a577db04-... kind="per_run"
- 13:12:13.903Z routine outcome posted run_id=a577db04-... outcome="budget_exceeded_per_run" event_id=2b8afd7b...
Harness usage for the turn (desktop journal, prompt result): cachedReadTokens 67004, cachedWriteTokens 70813, inputTokens 4, outputTokens 413, totalTokens 138234. The sidecar budgets on the harness's turn total (pool.rs: turn_total_tokens, else input+output), so cache reads and writes count against the per-run budget; a Claude session with a 67k-token context therefore breaches any budget below ~140k on every turn. Not a code defect, but a semantics the doc must state and the routine form should hint at.

Relay: outcome event 2b8afd7b (kind 9, agent-signed, threaded reply to the wake, tags buzz:routine-run + buzz:routine-outcome=budget_exceeded_per_run) settled the run at 13:12:14.03Z (13.4 s after claim); routine_dispatches.outcome=budget_exceeded_per_run; routine_state consecutive_failures=1, last_outcome=budget_exceeded_per_run. Exactly one outcome event. PASS for detection, outcome and settlement.

DEFECT S3-3 (Q2.1 "withhold the reply on a per-run breach"): the agent's normal reply "Current UTC time: 2026-09-14T13:12:00Z ..." was posted as event d9542fde at 13:12:12Z, one second BEFORE the breach notice 2b8afd7b at 13:12:13Z. On the turn-end path the reply is published before the budget is evaluated, so a breached run still posts its result. Routed to the builder.

Budgets raised to 300000/3000000 at 13:14:42Z (relay log "routine approved", strikes reset) for the happy-path fires in 03-first-fires.md.
