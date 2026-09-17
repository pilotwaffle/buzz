# Slice 5 live gate — forward re-enable (2026-09-17)

Exact reverse of Step 2 (live activity -> controls -> routines -> delegation), each confirming state resumes.

- 3(a) BUZZ_LIVE_ACTIVITY on (desktop): LA=true, app alive. PASS
- 3(b) BUZZ_AGENT_CONTROLS on (desktop): AC=true, app alive. PASS
- 3(c) BUZZ_ROUTINES on (desktop) + relay.env BUZZ_WORKFLOW_INVOKE_AGENT=1 re-set + relay restart: startup "invoke_agent enabled=true", delegation still false. PASS
- 3(d) BUZZ_DELEGATION on (desktop) + relay.env BUZZ_DELEGATION=1 re-set + relay restart: startup "invoke_agent enabled=true" AND "delegation dispatch enabled=true". PASS

END STATE: all four desktop flags true (CDP: LA/AC/RT/DG all true, app alive); both relay switches on; persisted tables intact through the full down+up cycle: delegation_records=3, routine_dispatches=39, routine_state=3. Nothing deleted, state resumed. ROLLBACK REHEARSAL (reverse + forward) PASS.
