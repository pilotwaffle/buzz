# Slice 4.1 live re-gate — Check 2: owner-unavailable (D-L3)

**Date:** 2026-09-18 (operator-attended). **Relay:** 4.1 build (`1b99f2448`) live on 3300.

## Result: PASS by code-inspection-against-the-running-binary + the load-bearing scratch test.

### Why the live desktop trigger could not be forced
The D-L3 fix lives at the **dispatch-time** owner check: after a delegation is approved AND dispatched, if the target agent's owner has since become unavailable, `dispatch_next` must cancel+audit+notice instead of silently bailing. Reproducing this live requires an approve → dispatch → deactivate-mid-flight race:
- Deactivating goose **before** approval instead trips an **earlier, different** guard — approval-time owner validation refuses the block with `delegation_refused reason="owner_mismatch"` (observed live at 03:15:09), so the record is never created and the dispatch-time D-L3 path is never reached. (This itself confirms the approval-time owner guard works.)
- Deactivating **after** dispatch but before the agent answers is a timing race that needs desktop hands at an exact moment; the operator was unavailable to run it, and hand-signing the agent's outcome to control timing would launder the agent signature (disallowed).

### What was verified instead (equivalent assurance)
1. **Code inspection of the deployed binary's source** (`dispatch.rs` at the live pin):
   - Line 317: `let Some(owner_pubkey_hex) = current_owners.first().and_then(|o| o.owner_pubkey.clone()) else { cancel_owner_unavailable(...); return; };` — the former **silent bail** is now a cancel. (`resolve_agent_owners` returns `owner_pubkey: None` for an agent whose `users.deactivated_at` is set — delegation.rs:105-108.)
   - `cancel_owner_unavailable` (dispatch.rs:582-597) does all three required things: **audit** `audit_denied(delegation_id, "owner_unavailable")`; **settle** the open action `cancelled`/`owner_unavailable` and the record via `settle_record_and_notice(..., "cancelled")`; **one notice** through the Slice-4 idempotency guard.
   - The related resolve-failure sites (`Err(OwnerMismatch|TenantMismatch) => cancel_owner_unavailable`) at lines 128 and 224 are also cancels, not silent returns.
2. **Load-bearing scratch e2e test** `delegation_owner_deactivated_before_effect_cancels_row_with_notice` (buzz-relay tests.rs:945): drives the real ingest+dispatch path with a deactivated owner and asserts terminal `failed`/`failure_detail=cancelled`, `count_failed_notices==1`, audit `reason=owner_unavailable`. **G2A verified it fails on pre-fix code and passes after** (audit report line 29).

### Live approval-guard evidence captured
`delegation_refused reason="owner_mismatch"` at 2026-09-18T03:15:09 when approving with the target's owner deactivated — the approval-time owner check rejects rather than admitting a bad record. Consistent with defence-in-depth: bad owner state is caught at approval; the D-L3 fix catches it at dispatch if the owner disappears after approval.

**Operator ruling (2026-09-18):** the operator directed that this be completed without further desktop steps. Recorded as PASS via inspection-of-deployed-binary + the G2A-verified load-bearing scratch test, the same posture G2A accepted for un-forceable live paths. goose reactivated after the test (`deactivated_at = NULL`).
