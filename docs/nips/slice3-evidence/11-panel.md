# Slice 3 N4 panel check (2026-09-14T16:05Z, page index-BldL_unn.js, BUZZ_ROUTINES override on)

- Workflows screen: "Routines" filter chip present with the flag on, absent with it off (02-flag-off-proof.md). Cards for the four s3-gate definitions render "Every 15 minutes, invoke agent", Active, channel, name, date and an enabled toggle that reflects the definition's enabled flag (daily and disabled off, routine and strikes on). s3-11-routines-list.png.
- Card click opens the Edit workflow dialog; the form shows the INVOKE AGENT step (N4 form step present). s3-11-routine-detail.png.
- Routine state (last fire, last outcome, strikes, budgets, paused reason) is NOT reachable: WorkflowDetailPanel is mounted only inside the "Run history" popover whose trigger is commented out (WorkflowDialog.tsx TODO workflow-run-history-capability). Gap S3-6, routed to the builder.
