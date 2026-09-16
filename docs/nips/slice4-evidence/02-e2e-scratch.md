# Slice 4 live gate — automated relay e2e suite (scratch DB, 2026-09-16)

Ran on the live-gate machine against the scratch containers (torq-buzz-postgres-scratch:55433, torq-buzz-redis-scratch:63799), never the live DB. Branch torq/slice4-delegation @ d741b3fc9 (the G2A-approved tip). Isolated CARGO_TARGET_DIR=target-verify, removed after.

Command:
  BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@127.0.0.1:55433/buzz \
  BUZZ_TEST_REDIS_URL=redis://127.0.0.1:63799 \
  cargo test -p buzz-relay --lib delegation -- --ignored --test-threads=1

Result: 7 passed; 0 failed; 0 ignored (finished 2.41s, no flake this run).
  delegation_end_to_end_approve_claim_dispatch_settle ... ok   (hard-rule-10)
  delegation_sweeper_times_out_and_retries_then_notices ... ok (R1 idempotency fix regression)
  delegation_cost_cap_refuses_every_action ... ok
  delegation_budget_exhaustion_fails_budget ... ok
  delegation_flag_off_rejects_both_auth_variants ... ok
  delegation_tenant_route_flag_off_matches_unknown_route ... ok
  delegation_store_unavailable_dispatches_nothing ... ok
