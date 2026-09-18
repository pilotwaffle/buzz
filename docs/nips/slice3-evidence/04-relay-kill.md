# Slice 3 runbook step 4: relay kill across a due instant (2026-09-14)

- Last claim before the test: scheduled_for 13:30 bucket, claimed 13:42:01Z (5 dispatch rows total).
- 13:56:35Z relay PID 47924 stopped (Stop-Process from the operator session; the task now runs in the interactive session). Process count 0.
- The due instant for the 13:45 bucket (anchored ~13:57) passed while the relay was down.
- 13:58:07Z the permanent-relay watchdog task restarted the relay on its own (PID 99532) before the operator's scheduled restart at 13:58:34Z (which then adopted the running relay). New log relay-recovery-20260914-135758.stdout.log.
- 13:59:15.58Z `routine fired` once: scheduled_workflow_fires row (13:45 bucket, claimed 13:59:15.83Z), exactly one dispatch 1a9a5c68 (13:59:15.97Z), settled succeeded at 13:59:20.32Z. Events with buzz:routine-run since 13:55: 2 (the wake and the outcome) -> no duplicate wake, no duplicate post.
PASS: restart recovers due work through the durable claim without double-firing; the kill landed before the claim, so the recorded case is "claimed once after restart" (not the sweeper case).
Raw log: slice3-evidence/04-relaykill-run.log
