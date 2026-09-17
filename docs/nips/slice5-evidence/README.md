# Slice 5 (Close-out) — Evidence Directory

Populated by the operator-attended Steps 1–7 live gate. Empty as of this document (Step 6) — no
live relay run has occurred for this slice yet. Naming follows Slice 3 and Slice 4 convention
(`NN-description.{md,log,...}`, numbered by runbook step).

Expected files, mapped to `docs/nips/slice5-runbook.md`'s numbered steps:

| File(s) | Runbook step | Contents |
|---|---|---|
| `01-bringup.log` | 1 | Relay rebuild from `torq/slice5-closeout`, migration 46 applied, `BUZZ_DELEGATION=1` and `BUZZ_WORKFLOW_INVOKE_AGENT=1` startup log lines, NIP-11 `self` published, schema dumps (`\d`) for all delegation/routine tables |
| `02-rollback-reverse.md` | 2 | Four flag-off steps in reverse order (a: DELEGATION off on desktop + relay env unset, b: ROUTINES off on desktop + WORKFLOW_INVOKE_AGENT unset, c: AGENT_CONTROLS off, d: LIVE_ACTIVITY off), with row counts and log checks at each step showing unchanged counts and no delegation/routine log lines |
| `03-rollback-forward.md` | 3 | Four flag-on steps in forward order (a: LIVE_ACTIVITY on, b: AGENT_CONTROLS on, c: ROUTINES on + WORKFLOW_INVOKE_AGENT=1, d: DELEGATION on + BUZZ_DELEGATION=1), with row counts and log checks at each step, plus observations of a routine firing on schedule and a delegation approval/dispatch succeeding |
| `04-seeded-secret.md` | 4 | Sentinel string used (SENTINEL-<hex>), SQL scan output from all delegation/routine tables and kind-24200 events, relay log grep results (zero sentinel matches outside agent's own messages) |
| `05-latency.md` | 5 | Emit→paint p50/p95 per stage (emit→signed, signed→callback, callback→paint) for Claude and goose agents, n ≥ 10 each; raw samples; summary table; no threshold asserted |
| `06-webview-aging.md` | 6 | 1-hour working-set sample table with timestamp and memory MB per 5-minute interval (12 rows); stability assessment |
| `07-hop2.md` or `07-hop2-not-run.md` | 7 | Live hop-2 delegation observations (A→B→C with parent/child outcome states and continuation wake), or skip explanation with reference to automated test coverage |

No files exist in this directory as of Step 6 (documentation write). This README is created now
(per build_spec.md Step 6's explicit instruction) so the operator running the live gate has a
clear checklist of exactly what to capture.
