# Slice 0 Contract Extension — invoke_agent Budgets

**Slice:** 3 (Routines)
**Date:** 2026-09-13
**FR reference:** FR-3 (2026-09-03 bill analysis)

## Fields added

| Field | Type | Required | Validation |
|---|---|---|---|
| `token_budget_per_run` | `u64` | yes | Must be > 0 |
| `token_budget_per_day` | `u64` | yes | Must be > 0 and ≥ `token_budget_per_run` |

## Wire contract (`InvokeAgentWire`)

Both fields are required with `deny_unknown_fields`; missing either is a parse failure.

```json
{
  "action": "invoke_agent",
  "agent_pubkey": "…",
  "prompt": "…",
  "result_channel": "…",
  "idempotency_key": "…",
  "token_budget_per_run": 100000,
  "token_budget_per_day": 1000000
}
```

## Reason

Per-token pricing on large LLM contexts makes routines an unbounded cost unless each fire
carries an explicit cap. `per_run` limits one execution; `per_day` caps total daily spend
per routine (the relay auto-pauses at the day boundary). Both must be required and
non-zero so no routine is ever approved without a deliberate budget decision.

## Compatibility

Legacy actions (`send_message`, `send_dm`, etc.) are unchanged — they decode permissively
and never parse these fields. Only the `invoke_agent` path is affected.