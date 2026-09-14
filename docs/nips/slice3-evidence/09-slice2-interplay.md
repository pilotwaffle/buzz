# Slice 3 runbook step 9: Slice 2 interplay (Claude, sidecar 452e45150)

## 9.1 pause lease holds a routine wake (2026-09-14)
- 13:40:34Z Pause from the session panel: sidecar `pause hold lease_id=84951ed5-... generation=1`, desktop ack applied.
- 13:42:01Z routine fired (relay), wake posted, dispatch row 447a3e81 open. Sidecar: NO `routine prompt received` while the lease was active (13:42-13:44).
- 13:44:09Z Resume: `pause released ... generation=2`, ack applied.
- 13:44:39Z sidecar `routine prompt received run_id=447a3e81-...` (30 s after resume, next queue tick); 13:44:44Z `routine outcome posted ... outcome="succeeded"`; relay settled the run at 13:44:44.26Z.
PASS: the wake is an ordinary queued prompt; it stays held under QueueHoldState and dispatches after resume; the relay's 1800 s outcome deadline covers the hold.
Raw log: slice3-evidence/09-interplay-run.log

## 9.2 structured cancel on a routine-started turn
Attempt 1 (14:14Z fire): the driver clicked Cancel 0.2 s after the sidecar's `routine prompt received` (14:14:15.84Z -> sent 14:14:16.05Z), before the desktop had received the turn_started frame, so the command carried run_id=idle and the sidecar refused it `run binding mismatch` (ack rejected/binding_mismatch at 14:14:16.40Z, 350 ms round trip). The turn ran to completion and settled succeeded at 14:14:20.5Z. This is the Q2 race behaving as specified (operator clicked before the desktop knew the run), not a defect; the Claude routine turn lasts only ~4 s.
Attempt 2 (14:29Z fire): driver waits for the desktop's turnId to become non-idle before clicking. Result in 09-cancel-run.log.
Attempt 2 result (14:29Z fire): desktop turnId d2e47bde seen at 14:29:16; Cancel sent 14:29:17.085Z, sidecar `control received ... control=Cancel`, `control signal sent to in-flight task ... mode=Cancel`, `control acked ... status=Applied sidecar_latency_ms=7`; desktop ack applied at 14:29:17.236Z (151 ms end to end). PASS: structured cancel against a routine-started turn acks applied through the unchanged Slice 2 path. The cancelled run's routine outcome is recorded below.
