# Step 13: three concurrent routine wakes after the S3-5b fix (sidecar aa575e0ce)

Date 2026-09-15. Sidecar rebuilt from `aa575e0ce` (`Start-S16Desktop.ps1 -Prepare` with both managed agents stopped via `stop_managed_agent`; Claude agent restarted via `start_managed_agent`). Relay unchanged (`81ffcc0f1`). Production page, no debugger. Isolated test re-run before the rebuild: `cargo test -p buzz-acp --lib routine` 23/23, `cargo test -p buzz-acp --lib queue::` 145/145 (`CARGO_TARGET_DIR=target-verify`, removed afterwards).

Setup 13:57:47Z: the three gate routines (`s3-gate-claude-routine` 8bf80c93, `s3-gate-claude-strikes` 5dff1e15, `s3-gate-claude-daily` b3f7ddf1) re-enabled together by definition save (`update_workflow`, budgets 1000000/3000000 each, interval 15m, same agent, same channel fde9a0fb). Re-enable reset all three `routine_state` rows to `consecutive_failures=0`, `paused_reason=NULL`.

Fire 13:58:17Z (the relay claimed the pending 13:45 slot for all three on the next tick): relay `routine fired` at 13:58:17.201 (29651fad routine), 13:58:17.219 (fdb37895 strikes), 13:58:17.259 (c197789b daily) -- three wakes within 58 ms while no turn was in flight for the first and the first turn was in flight for the other two.

Sidecar (`%APPDATA%\xyz.block.buzz.app.demo.slice1\agents\logs\acbcd8a3...log`):
- 13:58:17.29 `routine prompt received run_id=29651fad`
- 13:58:31.33 `routine prompt received run_id=fdb37895` (after turn 1 completed), 13:58:31.37 `routine outcome posted run_id=29651fad outcome="succeeded"`
- 13:58:35.81 `routine prompt received run_id=c197789b` (after turn 2 completed), 13:58:35.85 `routine outcome posted run_id=fdb37895 outcome="succeeded"`
- 13:58:51.73 `routine outcome posted run_id=c197789b outcome="succeeded"`

Desktop journal (React fiber `events` on `[aria-label="Agent controls"]`), each routine wake is its own turn with exactly one triggering event and eventDeltaCount 1:
```
13:58:17 turn_started {"source":"channel","triggeringEventIds":["052b3ab8..."]}
13:58:19 prompt_context_delivery {"eventDeltaCount":1,"promptBytes":2151}
13:58:31 turn_completed
13:58:31 turn_started {"source":"channel","triggeringEventIds":["3a19783d..."]}
13:58:31 prompt_context_delivery {"eventDeltaCount":1,"promptBytes":2162}
13:58:35 turn_completed
13:58:35 turn_started {"source":"channel","triggeringEventIds":["d4e69dce..."]}
13:58:35 prompt_context_delivery {"eventDeltaCount":1,"promptBytes":2158}
13:58:51 turn_completed
```

Relay settlement: `routine settled` 29651fad succeeded latency_ms=12692 (13:58:31.41), fdb37895 succeeded 17144 (13:58:35.88), c197789b succeeded 32967 (13:58:51.78). DB: three `routine_dispatches` rows all `succeeded`; `routine_state` for all three `consecutive_failures=0`, `last_outcome=succeeded`, `paused_reason=NULL`. No `routine_skipped_busy`, no dedupe, no timeout.

Contrast with the fourth pass (12-fourth-pass.md, sidecar 90dbc73f9): wakes 2 and 3 were flushed as one turn (`triggeringEventIds` of length 2, `eventDeltaCount=2`) and the third wake was lost to a timeout strike. S3-5b PASS. Routines disabled again by the operator after this check.
