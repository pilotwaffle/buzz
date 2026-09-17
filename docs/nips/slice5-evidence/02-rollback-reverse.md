# Slice 5 live gate — reverse rollback rehearsal (2026-09-17)

Baseline (all four flags ON): delegation_records=3, delegation_claims=3, delegation_actions=4, routine_dispatches=39, routine_state=3, workflows w/invoke_agent=4. Relay from Slice-4 D-L1 build (functionally equiv to slice5 tip; slice5 changed no relay behavior), migration 46.

## Step 2(a) — BUZZ_DELEGATION off  [PASS]
- 14:0xZ desktop flag BUZZ_DELEGATION -> false (Experiments).
- relay.env BUZZ_DELEGATION commented out (backup relay.env.bak-*); relay restarted via schtasks TORQ-Buzz-PermanentRelay.
- relay startup: "delegation dispatch enabled=false" (invoke_agent still true — that is routines, disabled in 2b), TCP listening 3300.
- Row counts unchanged: 3/3/4, 39, 3, 4 (nothing deleted). App alive; desktop confirms BUZZ_DELEGATION=false via CDP.

## Step 2(b) — BUZZ_ROUTINES off  [PASS]
- desktop flag BUZZ_ROUTINES -> false; relay.env BUZZ_WORKFLOW_INVOKE_AGENT commented out; relay restarted.
- relay startup 14:43: "workflow invoke_agent enabled=false" AND "delegation dispatch enabled=false" (both env switches now off), TCP listening 3300.
- Row counts unchanged: routine_dispatches=39, routine_state=3, delegation_records=3, workflows w/invoke_agent=4. Nothing deleted.

## Step 2(c) — BUZZ_AGENT_CONTROLS off  [desktop-flag-only, no relay switch]
## Step 2(d) — BUZZ_LIVE_ACTIVITY off  [desktop-flag-only, no relay switch]

- 2(c) BUZZ_AGENT_CONTROLS -> false, 2(d) BUZZ_LIVE_ACTIVITY -> false (desktop-flag-only). App alive.
- END OF REVERSE ROLLBACK: all four flags false (CDP: LA/AC/RT/DG all false, app alive). Persisted tables intact end to end: delegation_records=3, delegation_claims=3, delegation_actions=4, routine_dispatches=39, routine_state=3. Nothing deleted. PASS.
