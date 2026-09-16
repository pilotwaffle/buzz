# Slice 4 (Delegation) — Evidence Directory

Populated by the operator-attended Step 8/9 live gate. Empty as of this document (Step 7) — no live relay run has occurred for this slice yet. Naming follows Slice 3's `slice3-evidence/` convention (`NN-description.{md,log,png,...}`, numbered by runbook step).

Expected files, mapped to `docs/nips/slice4-runbook.md`'s numbered steps:

| File(s) | Runbook step | Contents |
|---|---|---|
| `01-relay-bringup.log` | 1 | Relay rebuild from `torq/slice4-delegation`, migration 46 applied, `BUZZ_DELEGATION=1` startup log line, NIP-11 `self` published |
| `02-flag-off-proof.md` | 2 | Both directions of the two-switch flag-off proof, including the `GET /delegations/tenant` 404-byte-equality check |
| `03-first-fires.md` | 3 | Fresh private channel, unique per-run token, agents A/B/C bring-up |
| `04-end-to-end-a-b.md` | 4 | A→B: block → card → approve → summary → wake → B's answer → outcome → card chip `delivered` |
| `05-hop2-a-b-c.md` | 5 | Hop 2 A→B→C with `delegated`, continuation, final answer; hop 3 refused |
| `06-turn-ceiling.md` | 6 | `max_turns=1` parent delegating → `failed(turns)` notice |
| `07-cost-cap.md` | 7 | Cost-capped delegation → approve → `cost_unknown` notice |
| `08-budget-exhaustion.md` | 8 | Token budget 200 → `budget_exceeded` |
| `09-replay-and-conflict.md` | 9 | Replay of the same 43007 twice (no second wake); conflicting reuse refused |
| `10-enumeration-probe.md` | 10 | Approve for a nonexistent pubkey and for another operator's agent; both `OK` replies captured byte-for-byte |
| `11-pause-cancel-interplay.md` | 11 | Pause-hold and cancel interplay on a delegation turn |
| `12-store-scan.md` + `s4-db.sh` | 12 | Relay store scan over the three delegation tables + relay log grep for prompt/reply sentinels (AC-22/I-14 pattern) |
| `13-latency.md` + `s4-latency.py` | 13 | Fire-to-inject latency, n ≥ 10, adapted from Slice 3's `s3-latency.py` to `buzz:delegation-run` |
| `14-stripped-context-probe.md` | 14 | Hand-crafted kind-9 with delegation tags signed by the operator → sidecar logs `delegation_context_denied`, no turn |

No files exist in this directory as of Step 7. This README is created now (per build_spec.md Step 7's explicit instruction) so the operator running Steps 8/9 has a checklist of exactly what to capture.
