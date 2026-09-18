# Slice 4.1 live re-gate — Check 1: turn-exhaustion (D-L2)

**Date:** 2026-09-18 (operator-attended, King Flowers + assistant)
**Relay:** 4.1 build (`1b99f2448` / `source/buzz` main), deployed to `bin/buzz-relay.exe`, restarted via `TORQ-Buzz-PermanentRelay`. PID confirmed on 3300; delegation dispatch enabled=true; sweeper 60s tick; workflow invoke_agent enabled=true.
**Agents:** Claude (`acbcd8a3…`, "test sonnet") → goose (`0093afda…`, "goose test"); shared owner `2fedcc9e…`; Group DM `e2b056c7-…`.

## Result: PASS — D-L2 core fix proven live; `failed/turns` detail covered by the scratch test.

### Check 1a — delegation `311cae61-0def-45f9-95b3-1b560a647388`, `max_turns:1`, `token_budget:100000`
- Posted as the Claude managed agent via CDP→`send_managed_agent_channel_message`; operator approved in-desktop (kind-43007).
- Action 1 dispatched to goose (wake threaded under origin — D-L1 shape). `remaining_turns` → 0 at dispatch.
- goose answered; record settled **`failed` / `failure_detail=budget`** with **exactly one** failure notice (`failure_notice_event_id` set).
- **Key D-L2 observation:** the record reached a **terminal `failed` state with one notice** — it did NOT hang at `approved` forever, which was the exact pre-fix bug. (It settled on the `budget` branch because goose's real answer consumed the small token budget before the turns branch; both are correct terminal-failure paths.)

### Check 1b — delegation `0424c089-0904-4e50-b51a-00ffbcf1c6c1`, `max_turns:1`, `token_budget:100_000_000`
- Re-run with an effectively unlimited budget to isolate the turns limit.
- Operator approved; action dispatched; goose answered. Record settled **`delivered`** (terminal success), budget used ~30,738 of 100,000,000.
- This is the **correct** outcome for a single-turn delegation that the agent *completes*: `delivered` is a terminal success state (buzz-db store: `delivered|failed|expired` are terminal). It also serves as the runbook §5 happy-path regression spot-check — PASS.

## Why the specific `failed/turns` detail was not forced live
`failed/turns` requires the target agent to reply with outcome **`delegated`** (wants to continue) while `remaining_turns==0`, so `dispatch_next` re-enters and hits the fixed line `dispatch.rs:56` (`if row.remaining_turns == 0 { settle … "turns" }`). A real free-tier agent (goose) given a one-shot task **delivers** rather than **delegates**, so it cannot be reliably coaxed into the `delegated` continuation, and hand-posting goose's signed outcome would launder the agent's signature (disallowed).

The `delegated`-continuation trigger and the `failed/turns` settlement are covered by the passing scratch e2e test **`delegation_turn_exhausted_settles_failed_turns_with_one_notice`** (buzz-relay `tests.rs`), which drives outcome `"delegated"` through the real ingest path and asserts `state=failed`, `failure_detail='turns'`, one notice, action row unchanged. G2A verified this test is load-bearing (fails on pre-fix code).

**Operator ruling (2026-09-18):** accept check 1 as PASS — D-L2 core fix (no more stuck-`approved`) proven live; the `turns` detail accepted via the passing scratch test, same posture G2A already accepted for un-forceable live paths.
