# Slice 4.1 (Delegation dispatcher fixes: D-L2, D-L3) — Operator-Attended Live Runbook

**Status: WRITTEN, NOT EXECUTED.** This is the exact procedure an operator runs to close AC-12's
live re-gate: proving, against the live relay, the two specific defects this slice fixed
(turn-exhaustion no longer stranding a delegation `approved` forever, and a genuine store error
during owner resolution auditing and retrying instead of silently vanishing) — not a full re-run
of Slice 4's own runbook, which already covers the rest of the delegation feature and is
unaffected by this slice's changes (I-8, confirmed via the unchanged-elsewhere test counts in
`SLICE-4-1-VERIFICATION.md` §1). The builder session that wrote this runbook did not touch the
permanent relay, the live database, or running sidecars — every step below requires operator
hands and operator authorization.

**Before starting:** confirm `torq/slice4-1-delegation-dispatcher` is at the commit this runbook
was written against (`912538f63` or later on the same branch), built from a tree where
`torq/slice4-delegation` is already merged and live (Slice 4's own runbook already closed).

Record every timestamp, log excerpt, and screenshot named below into
`docs/nips/slice4-1-evidence/`, one file per numbered step (matching the naming scaffolded in
`docs/nips/slice4-1-evidence/README.md`, e.g. `02-turns.md`, `03-owner.md`). If any step's
expected observation does not match, stop, do not proceed to the next step, and record the
mismatch instead of working around it.

---

## 1. Relay + sidecar bring-up

1. `cargo build --release --bin buzz-relay` on `torq/slice4-1-delegation-dispatcher`.
2. Back up the running binary: copy `E:\TORQ-BUZZ\bin\buzz-relay.exe` →
   `bin\buzz-relay.exe.bak-<date>`.
3. Copy the newly built binary into `bin\buzz-relay.exe`.
4. No new migration in this slice (`store_error_outcome`/D-L2/D-L3 are code-only changes to
   `delegation_records`'s consumers, not its schema) — confirm `\d delegation_records` still
   matches `SLICE-4-VERIFICATION.md` §3 exactly (unchanged).
5. **Restart only through the scheduled task `TORQ-Buzz-PermanentRelay`** — never
   `Start-Process` the relay directly from an agent or operator shell session.
6. Confirm `RUST_LOG` still includes `buzz_relay::delegation=info` (per Slice 4's runbook §1.6 —
   unchanged this slice, but re-confirm since it gates whether the audit lines below are visible
   at all).
7. Record: binary commit hash, the relay's `self` pubkey, and confirmation this is the Slice 4.1
   build (not a stale Slice 4 binary).

**Rollback:** identical to Slice 4's runbook §1 rollback — set `BUZZ_DELEGATION` unset, restart the
scheduled task, reverse the binary copy only if the new binary itself is suspect.

## 2. Turn-exhaustion re-gate (D-L2, I-2/I-3)

This proves live what `delegation_turn_exhausted_settles_failed_turns_with_one_notice` proves
against the scratch DB: a delegation that exhausts its turns reaches a terminal state instead of
sitting `approved` forever.

1. Draft and approve a direct delegation A→B with `max_turns: 1` (same setup as Slice 4's
   runbook §7, reused here since D-L2 lives on this exact path).
2. Let B answer once (its outcome `delivered` or `delegated` — either drives the parent record's
   turn count to 0).
3. Confirm, via the live DB, that `delegation_records.state` transitions to `failed` with
   `failure_detail = 'turns'` **within the same dispatch cycle that observed the exhausted turn
   count** — not stuck `approved` waiting for the sweeper's 60s tick (the pre-fix symptom).
4. Confirm exactly one `buzz:delegation-notice` event with outcome `failed` is posted under the
   origin thread, content ending `did not complete: turns`.
5. Confirm action 1's own outcome row (`delegation_actions`) is unchanged (`delivered`/`delegated`,
   whichever B produced) — the fix must never overwrite an already-settled action row.
6. Trigger a second dispatch attempt on the same delegation (e.g. wait for the sweeper's next tick,
   or another child-answer event if applicable). Confirm no second notice is posted
   (`notice_due` guard, I-6).

## 3. Owner-unavailable re-gate (D-L3, I-4)

This proves live what `delegation_owner_deactivated_before_effect_cancels_row_with_notice` proves
against the scratch DB: an agent in the delegation path losing its owner terminates the
delegation with an audit line and a notice, instead of leaving it silently stuck.

1. Set up a direct delegation A→B, approved and dispatched (B has received its wake, has not yet
   answered).
2. As the operator, deactivate B's owner (or B itself, whichever the live deployment's
   "deactivate agent owner" operation actually is — confirm against
   `crates/buzz-db/src/store/delegation.rs`'s `resolve_agent_owners` query, which reads
   `users.deactivated_at`).
3. Trigger the next dispatch attempt for this delegation (sweeper tick, or any redispatch path).
4. Confirm the relay log contains a `delegation_context_denied` line with
   `reason="owner_unavailable"` under the `buzz_relay::delegation` target.
5. Confirm `delegation_records.state = 'failed'`, `failure_detail = 'cancelled'`.
6. Confirm the open action row (if any) is settled `cancelled` with detail `owner_unavailable`.
7. Confirm exactly one failure notice is posted.

## 4. Store-error path — inspection only, not a live fault injection

AC-7's decision (a genuine store error during `resolve_agent_owners`/`open_action_as_target`
audits `authority_unavailable` and leaves the record `approved` for the sweeper to retry) is
covered by a direct unit test on the extracted `store_error_outcome` function
(`SLICE-4-1-VERIFICATION.md` §4) — a live, forced-fault reproduction was attempted in the
development environment and found infeasible (see that section for the seven techniques tried).
**Do not attempt to force a live store error against the production relay/database to re-verify
this** — that would require the same kind of connection-pool or backend-level fault injection
against a shared, live system, which is out of proportion to what this check is worth and risks
disrupting real traffic. Instead:

1. Confirm via code reading (not live testing) that `resolve_agent_owners` and
   `open_action_as_target`'s error branches in `dispatch_next` both route through
   `store_error_outcome`, and that its only two outcomes are `Continue` (on `Ok`) and
   `AuditAndRetryLater` (on any `Err`) — i.e. there is no code path where a store error at either
   site settles the record.
2. If a genuine store error at either site is ever observed organically in the live relay's logs
   (a transient DB blip during normal operation), capture that log line as incidental evidence —
   do not wait for or engineer one.

## 5. Regression spot-check

1. Re-run one full A→B delegation (Slice 4's runbook §4) to confirm the happy path is unaffected
   by this slice's changes.
2. Re-run the hop-2 continuation (Slice 4's runbook §5) once, to confirm bullet 7's fix (a fresh,
   never-settled child delegation for the nested-hop test) reflects real multi-hop behavior, not
   just a test-fixture adjustment.

---

## Evidence checklist

Every item below must have a corresponding file or excerpt in `docs/nips/slice4-1-evidence/`
before this runbook is considered closed:

- [ ] Relay bring-up: binary commit, `self` pubkey, confirmed Slice 4.1 build
- [ ] Turn-exhaustion re-gate: `failed`/`turns`, one notice, action 1 outcome preserved, no
      duplicate notice on retry (`02-turns.md`)
- [ ] Owner-unavailable re-gate: audit line with `reason="owner_unavailable"`, `failed`/`cancelled`,
      action cancelled `owner_unavailable`, one notice (`03-owner.md`)
- [ ] Store-error path: code-reading confirmation only, no live fault injection attempted
- [ ] Regression spot-check: one A→B delegation, one A→B→C hop-2 continuation, both unaffected

**Residual, not this slice's scope:** the step-e cancel path's missing notice
(`SLICE-4-1-VERIFICATION.md` §4) — a pre-existing, packet-named-out-of-scope gap, not re-gated
here.
