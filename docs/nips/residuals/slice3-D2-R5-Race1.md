# Slice 3 residual items: D-2, R5, Race-1

## Symptom

Three items from Slice 3 (routines) were identified and recorded during gate verification but explicitly deferred from Slice 5 as out-of-scope ([Q4.4] "recorded only"):

### D-2: Goose mid-turn budget-breach cancellation

**Definition:** The Slice 3 spec assumed a routine's budget enforcement could pause the agent mid-turn (while a model call is in flight) if the per-run token budget was exhausted. In the actual code, `run_prompt_task` in `crates/buzz-acp/src/pool.rs` blocks on `session_prompt_blocks_with_idle_timeout`, which awaits directly from the same `AcpClient` that owns the usage tracker — no concurrent reader can check the budget without blocking the turn.

**Technical detail:** A correct mid-turn budget check can only run from inside `read_until_response_with_idle_timeout`, the shared read loop every agent turn (not just routines) uses — a materially larger and riskier change to the hot path than the spec assumed.

**Accepted disposition (SLICE-3-VERIFICATION §5, operator ruling 2026-09-13):** Turn-end-only enforcement is acceptable. The invariant that matters holds: a per-run breach is terminated at turn end, counted `failed(budget_exceeded_per_run)` with exactly one outcome posted, and (for goose) the respawn/retry is handled. Turn-end enforcement (spec 5.3b) is a complete, correct substitute for this slice.

**Evidence pointer:** SLICE-3-VERIFICATION.md §5, Deviation 3; `crates/buzz-acp/src/pool.rs` finalize_routine_turn branch for per-run breach.

### R5: Withhold the reply on per-run breach

**Definition:** The design expected the agent's reply message to be withheld on a per-run budget breach, publishing only the failure notice instead. The relay sweeper would suppress any duplicate.

**Technical detail:** The routine turn's reply is published by the managed agent itself via its own `buzz messages send` CLI call — a subprocess executing outside `buzz-acp`'s control. The sidecar has no visibility into or authority over the agent's own CLI commands. Enforcing "withhold" would require either a relay-side hold/release mechanism gating `kind:9` publication from a routine-bound agent (a relay change), or telling the agent to defer its send (unenforceable, and no mechanism currently signals the outcome to the agent before it decides to reply).

**Accepted disposition (SLICE-3-VERIFICATION §7 contract revision R5, operator ruling 2026-09-14 17:58Z):** On a per-run breach the reply may already be posted; the sidecar then posts the failure notice and the run is counted `failed`. On success the sidecar posts the tagged outcome event as a separate event from the reply. Notice/outcome events remain exactly one per run (test-verified). This is a Slice 5 item (relay-side or protocol-level enforcement).

**Evidence pointer:** SLICE-3-VERIFICATION.md §7; `slice3-evidence/06-per-run-breach.md`; `crates/buzz-acp/src/pool.rs` finalize_routine_turn for breach detection and outcome posting.

### Race-1: Cancel-completion finalize gap

**Definition:** In `crates/buzz-acp/src/pool.rs:3160-3223`, the "turn already completed — treating as success" branch does not call `maybe_finalize_routine`, so a routine turn that completes in the exact cancel-race window posts no outcome event. The relay sweeper then times it out at `routine_outcome_deadline_secs` (1800s = 30 minutes), incrementing one innocent strike.

**Technical detail:** The gap is a pre-existing race condition (present at parent commit `dc9d278bb`, before Slice 3). When a structured cancel arrives exactly as the turn completes and signals its own completion before the cancel handler runs, the ACP client sees the turn complete first and cleans up the session — but the routine finalization routine is skipped, leaving no record until the sweeper timeout.

**Accepted disposition (SLICE-3-VERIFICATION, G2A round-5 D3, recorded non-blocking):** Self-healing — a later successful turn resets the strikes. The fix is simple: add `maybe_finalize_routine(&usage, &PromptOutcome::Ok(EndTurn))` to the relevant branch in `pool.rs:3204` region. This is a Slice 5 item (small, targeted fix once the exact defect is re-confirmed).

**Evidence pointer:** SLICE-3-VERIFICATION.md §5 Deviation 3; `crates/buzz-acp/src/pool.rs:3160-3223`.

## Proposed Scope

A dedicated follow-up packet to:
1. Re-confirm the exact defect for each item against the current codebase (D-2, R5, Race-1)
2. For D-2: implement mid-turn budget check inside `read_until_response_with_idle_timeout` (larger change; may require refactoring to share the usage tracker safely across concurrent readers)
3. For R5: design a relay-side hold/release mechanism or protocol extension to gate `kind:9` publication from a routine-bound agent until its outcome is known, or a prompt-based deferral signal with enforcement
4. For Race-1: add the missing `maybe_finalize_routine` call and test via a synthetic race-window injection

Each item requires operator scoping before implementation, since D-2 and R5 involve trade-offs across the acp/relay boundary, and Race-1 is a one-liner fix that still needs integration testing.

## Not Scheduled

Deferred pending operator prioritization.
