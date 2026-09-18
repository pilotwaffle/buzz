# Slice 4.1 (Delegation dispatcher fixes) — Evidence Directory

Populated by the operator-attended AC-12 live re-gate. Empty as of this document (Step 5) — no
live relay run has occurred for this slice yet. Naming follows Slice 4's `slice4-evidence/`
convention (`NN-description.{md,log,png,...}`, numbered by runbook step).

Expected files, mapped to `docs/nips/slice4-1-runbook.md`'s numbered steps:

| File(s) | Runbook step | Contents |
|---|---|---|
| `01-relay-bringup.log` | 1 | Relay rebuild from `torq/slice4-1-delegation-dispatcher`, binary commit hash, `self` pubkey confirmed |
| `02-turns.md` | 2 | Turn-exhaustion re-gate: `max_turns=1` delegation → `failed`/`turns` within the same dispatch cycle (not the sweeper's 60s tick), one notice, action 1's outcome preserved, no duplicate notice on retry |
| `03-owner.md` | 3 | Owner-unavailable re-gate: deactivate B's owner mid-delegation → `delegation_context_denied reason="owner_unavailable"` audit line, `failed`/`cancelled`, action cancelled `owner_unavailable`, one notice |
| `04-store-error-inspection.md` | 4 | Code-reading confirmation that `store_error_outcome` is the only decision point for `resolve_agent_owners`/`open_action_as_target` errors — no live fault injection performed (see runbook §4 for why) |
| `05-regression-spotcheck.md` | 5 | One A→B delegation and one A→B→C hop-2 continuation, confirmed unaffected by this slice's changes |

No files exist in this directory as of Step 5. This README is created now (per build_spec.md
Step 5's explicit instruction) so the operator running AC-12 has a checklist of exactly what to
capture. `push_authorized` stays `false` and no flag change is implied by this slice — it fixes
existing dispatcher behavior behind the already-live `BUZZ_DELEGATION` switch, it does not gate
anything new.
