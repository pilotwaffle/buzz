# Slice 3 runbook step 10: fire-to-inject latency (Claude), 2026-09-14

Method: start = relay stdout JSON line "routine fired" (run_id), end = sidecar INFO "routine prompt received run_id=..."; nearest-rank p95; production page, no debugger; sidecar 452e45150, relay 81ffcc0f1. Run 447a3e81 excluded (operator-held under a pause lease, step 9). Tool: slice3-evidence/s3-latency.py. Raw pairs:

```
run_id                               routine                  dispatched_at            received_at              ms   outcome
58ba3d1f-5d2f-4bed-b8b6-6c7f078bffef s3-gate-claude-routine   06:02:52.504 (no sidecar receipt)                timeout
563fa1f2-cd4a-4613-9155-39cc63584eb5 s3-gate-claude-routine   06:32:53.028 (no sidecar receipt)                timeout
a577db04-5637-42f9-9ceb-514d1c91e38f s3-gate-claude-routine   13:12:00.676 13:12:00.782     106 budget_exceeded_per_run
5dc8e761-a80c-4b21-b6c9-ccd4cf8d69f5 s3-gate-claude-routine   13:27:00.957 13:27:01.155     198 succeeded
447a3e81-739b-4b6a-9f16-d0a414b2d7ee s3-gate-claude-routine   EXCLUDED (operator-held run)
1a9a5c68-35ca-495b-8bf2-fdcd397d0055 s3-gate-claude-routine   13:59:15.581 13:59:15.636      55 succeeded
43108e78-b8d7-4e47-83cf-40727baf3250 s3-gate-claude-routine   14:14:15.801 14:14:15.835      34 succeeded
4451506b-daca-4efb-81fa-cfc62e6fb2d3 s3-gate-claude-routine   14:29:16.138 14:29:16.168      31 timeout
b3febdc6-4cf0-42b2-95b1-1e3d0899bbc6 s3-gate-claude-strikes   14:31:16.208 14:31:16.258      50 budget_exceeded_per_run
c3d2ce49-de42-4a6d-b96f-d4b9cde585d2 s3-gate-claude-daily     14:31:16.225 (no sidecar receipt)                timeout
592dcfac-76e6-4493-b494-6c01c88acd50 s3-gate-claude-strikes   14:46:16.523 14:46:16.554      30 budget_exceeded_per_run
7191b8a7-8376-456e-a50a-61549abed5fb s3-gate-claude-routine   14:59:16.821 14:59:16.848      27 succeeded
2537ae9b-b43f-4152-a3b7-3765ba4e8bc6 s3-gate-claude-strikes   15:01:16.919 15:01:16.951      32 budget_exceeded_per_run
d54f46d6-978d-4de7-9146-fda2a8036e5e s3-gate-claude-routine   15:14:17.192 15:14:17.223      31 succeeded
c3ad5bf7-ed98-492d-ac3d-922440dd03a0 s3-gate-claude-strikes   15:16:17.233 15:16:17.255      21 budget_exceeded_per_run
5d7903b5-f9a2-47e9-a02c-e4b914da9f6e s3-gate-claude-routine   15:29:17.509 15:29:17.537      28 succeeded
99fcd034-44b3-4ae2-8fe9-729affad065d s3-gate-claude-strikes   15:31:17.616 15:31:17.651      35 budget_exceeded_per_run
a6a933bb-776c-49ae-a954-6eac76d0c9f4 s3-gate-claude-routine   15:44:17.845 15:44:17.877      32 succeeded
92c23c89-5297-4b35-8190-654b4ff832c6 s3-gate-claude-strikes   15:46:17.913 15:46:17.938      25 budget_exceeded_per_run
1fa9a1f1-bebb-439a-beef-b0f526c37a57 s3-gate-claude-routine   15:59:18.150 15:59:18.180      30 succeeded
b7e43c62-04be-4938-9b90-b063ee4eae5f s3-gate-claude-strikes   16:01:18.211 16:01:18.241      30 budget_exceeded_per_run

n=17 p50=31 ms p95=198 ms max=198 ms (fire-to-inject: relay `routine fired` -> sidecar `routine prompt received`)
```

Result: n=17, p50 31 ms, p95 198 ms, max 198 ms; target p95 <= 60 s: PASS (Claude). goose: not measured (model provider outage, see 02-flag-off-proof.md).
