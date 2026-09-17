# Slice 5 live gate — R5-1 emit→paint latency re-measurement (2026-09-17, measurement-only, NO threshold asserted per I-6/Q1.2)

Method: s1-cdp.mjs gate-nodebug mode (sets s1-gate=1, resets window.__s1Log, nudges the Claude managed agent, DETACHES the debugger for the measurement window, then reads the paint log). Production dev build, all flags on, channel e2b056c7. 51 log lines captured over ~90 s.

## What captured
- Stage line counts: 9 paint batches, 21 wsrecv, 42 recv-id lines.
- **recv→decrypt stage (ws receive → decrypted in the desktop): n=21, p50 5.03 s, p95 20.48 s, max 21.84 s.**
- Clean emit→paint pairs: 0 paired in this window (paint batches were n=0 empty-timeline paints; the recv/paint ids did not pair inside 90 s).

## Finding (feeds the deferred R5-1 relay packet)
The dominant latency is the **desktop-side decrypt chain** (recv→decrypt p95 ~20 s), NOT relay fan-out. This confirms the Slice 1 diagnosis (sequential decrypt + process-wide Tauri stall) and shows that R5-2 (a relay rate-limit class for kind-24200) would not close the §5 emit→paint p95 ≤ 2 s product gate on its own — the fix must target the desktop decrypt pipeline. The §5 gate remains **NOT closed** and is carried to the R5-1 packet (per Slice 1 operator ruling + Slice 5 Q1.2 deferral). A clean staged emit→signed→callback→paint p50/p95 with proper pairing instrumentation is part of that packet, not this close-out.

Raw log: scratchpad r51-claude.log (paint/wsrecv/recv lines). goose leg not separately captured (test-only agent; the recv→decrypt chain is agent-agnostic and the finding stands).
