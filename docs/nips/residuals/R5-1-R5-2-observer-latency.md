# R5-1 / R5-2: Observer latency remeasurement and rate-limit class review

## Symptom

Two outstanding gaps in the observer (live activity) path were explicitly deferred from Slice 5 as Non-Goals:

- **R5-1:** emit→paint latency (p95) for observer frames has not been re-measured in this slice; Slice 1 reported the metric but Slice 5 is measurement-only (operator-attended, not builder-executed) and the p95 has not been re-confirmed against the current codebase and load.
- **R5-2:** rate-limit class for observer frames has not been revisited; the current relay limit is 100 telemetry events/second per agent pubkey (NIP-AO), not independently justified for the current feature density.

Both are explicitly listed as "recorded only" in `build_spec.md` Non-Goals ([Q1]).

## Evidence Pointer

Existing measurement tooling:
- `docs/nips/slice1-evidence/s1-p95.py` — measures emit→signed→websocket callback→paint latency for observer frames
- `docs/nips/slice1-evidence/s1-split.py` — stages breakdown into emit→signed, →websocket callback, →paint, with percentile analysis
- `docs/nips/slice1-evidence/s1-cdp.mjs` — Chrome DevTools Protocol harness integration

The live measurement is deferred to the operator runbook at `docs/nips/slice5-runbook.md`, step 5 (R5-1 measurement), which the builder never executes per policy (SOC-1/SOC-2: live steps operator-only).

## Proposed Scope

A dedicated follow-up packet to:
1. Run the p95 re-measurement live on the current relay + desktop build with Claude and at least one other agent (e.g., Goose from the test harness)
2. Collect ≥10 measurements per agent over a 10-minute window using the existing `s1-p95.py` tooling
3. Analyze the percentile distribution and compare against the Slice 1 baseline
4. Review the 100 events/sec rate limit in context of the current frame volume and latency SLA
5. Decide whether R5-1 and R5-2 require code changes (e.g., increased rate limit, optimized batching, delivery-path changes) or whether current performance is acceptable

Evidence decision: if re-measurement shows p95 > 2s, or if rate-limit analysis suggests the current class is insufficient, escalate to a separate performance-optimization packet. Otherwise, accept the current limits and close.

## Not Scheduled

Deferred pending operator decision on measurement timing and acceptable latency thresholds.
