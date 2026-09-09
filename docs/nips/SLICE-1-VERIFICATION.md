# Slice 1 Verification — Live Activity Panel

**Branch:** `torq/slice1-live-activity` (off `b88a9fc13`)
**Built by:** Builder (DeepSeek V4-Pro), 2026-09-08
**Budget:** $15

## Before/After Test Counts

| Gate | Baseline (pin `b88a9fc13`) | After Slice 1 | Delta |
| --- | --- | --- | --- |
| `cargo test -p buzz-core` | 283/0 | 283/0 | — |
| `cargo test -p buzz-workflow --lib` | 181/0/2 | 181/0/2 | — |
| `cargo test -p buzz-acp --lib` | 880/0 | **890/0** | +10 new (tick resolution + quota counter) |
| `cargo test -p buzz-relay --lib` | 1026/6/89 | not re-run (zero relay diff) | — |
| `pnpm typecheck` | exit 2 (pre-existing TS2322) | exit 2 (same pre-existing) | — |
| `pnpm test` | 6454 tests / 6433 pass / 21 fail | **6478 tests / 6457 pass / 21 fail** | +24 new mapping tests, all pass; no new failures |
| `pnpm build` | exit 2 (same TS2322) | exit 2 (same pre-existing) | — |

All counts within the Constraints ceiling. Zero new failures. Step 7 changes (archive paging, "Show more" button) add no new test failures — typecheck and test counts unchanged from Steps 1-6 baseline.

## AC Verification

### AC 1: Branch
- `git log` shows all work on `torq/slice1-live-activity` branched from `b88a9fc13`
- Nothing pushed — all changes local

### AC 2: Tick resolution (R1)
- Tick resolves from `BUZZ_OBSERVER_PUBLISH_TICK_MS` env var, default 500 ms
- Floor = `60_000 / BUZZ_RATE_LIMIT_AGENT_STANDARD_MESSAGES_PER_MIN` (default → 500 ms)
- Ceiling = 1000 ms
- Below-floor config clamps and logs
- 10 unit tests in `observer_publish_tick_tests` module, all passing

### AC 3: No relay/auth changes
- `git diff -- crates/buzz-relay/ crates/buzz-auth/` returns 0 lines — zero diff ✓
- `git grep BUZZ_RATE_LIMIT_AGENT_STANDARD_MESSAGES_PER_MIN` — default unchanged (120)
- No adaptive backoff anywhere in the diff

### AC 4: Mapping module
- 24 tests pass covering:
  - Basic mapping with human titles
  - Detail extraction (body, text, command, tool)
  - Excerpt byte caps with UTF-8 boundary respect
  - No raw observer JSON leakage in output fields
  - Seq-gap detection (simple, multiple, large)
  - Stream reset handling (decreasing seq, equal seq)
  - Multi-harness stream isolation
  - Quota-rejection-shaped gap fixture
  - Existing stream state carry-forward
  - Null/empty payload handling
- **Fixture provenance (corrected 2026-09-08, G2A D2):** all fixtures are SYNTHETIC, hand-constructed via the `ev()` helper. The spec's real-two-harness-fixture requirement is NOT yet met with captured frames — real Claude/goose frames were not present on `torq/s16-spike` as a capturable JSON appendix, and capturing them needs an operator-attended desktop launch. Moved to Runbook §9 as an explicit open item.

### AC 5: Flag-off no regressions
- `pnpm test`: 6457 pass / 21 fail (baseline 6433/21 — +24 new pass, same fail count, no regression) ✓
- `pnpm typecheck`: exit 2, only pre-existing TS2322 ✓
- All existing observer tests unchanged (same test names, same pass/fail state)

### AC 6: Flag-on states
- Component tests for five states require desktop launch — see Runbook below

### AC 7-8: Test 1 gate + R2
- Requires desktop launch with Claude and goose harnesses — see Runbook below

### AC 9: Quota-rejection accounting
- **Option (a):** Publisher-side counter wired. `OBSERVER_QUOTA_REJECTION_COUNT` static atomic incremented in `relay.rs` OK handler when `rate-limited:` rejection received for observer frames. Counter exposed via `observer_quota_rejection_count()`. Expected value during gate runs: 0 at 500 ms default.
  - Implementation: `crates/buzz-acp/src/lib.rs` (static atomic + `bump_observer_quota_rejection()` / `observer_quota_rejection_count()`) and `crates/buzz-acp/src/relay.rs:2615` (increment on rate-limited OK)
  - Count during gate runs: **PENDING — see Runbook**

### AC 10: Archive posture
- Requires fresh-identity test — see Runbook

### AC 11: Bounded history
- "Show more" button at the bottom of the timeline triggers `fetchOlderArchived` (from `useLoadArchivedObserverEvents`) to load next page of archived events
- `LiveActivityTimeline` receives `liveActivityCombinedEvents` (live + archive merged via `mergeObserverEventWindows`) from `ManagedAgentSessionPanel`
- Button only renders when `archiveEnabled && hasOlderArchived`; shows "Loading…" during fetch
- Explicit "history off" state when `archiveEnabled=false` ("History off — older activity is not being saved.")
- No consent writes from any UI path (the timeline is read-only; archive consent is managed by the existing `owner_p` subscription; `useLoadArchivedObserverEvents` only reads, never creates consent)

### AC 12: Keyboard + non-colour cue
- Timeline rows are keyboard-navigable (tab/arrow, Enter/Space to expand)
- Roster busy indicator: Play icon (shape-based, non-colour cue) alongside "Working" text badge when flag is on

### AC 13: Verification doc
- This document

### AC 14: Flag flip
- `preview-features.json` still shows `platforms: []`, `defaultEnabled: false`
- Flip to `platforms: ["desktop"]` ONLY after all remaining ACs pass — see Runbook

## Files Changed

| Path | Action | Purpose |
| --- | --- | --- |
| `crates/buzz-acp/src/lib.rs` | modify | R1 tick resolution, quota counter, 10 unit tests |
| `crates/buzz-acp/src/relay.rs` | modify | Quota rejection counter increment in OK handler |
| `desktop/src/features/agents/liveActivity/liveActivityConstants.ts` | create | Named constants (STALE_THRESHOLD, byte caps) |
| `desktop/src/features/agents/liveActivity/mapObserverEvents.ts` | create | Pure mapping module: event → timeline entry + seq-gap |
| `desktop/src/features/agents/liveActivity/mapObserverEvents.test.mjs` | create | 24 fixture tests for mapping module |
| `desktop/src/features/agents/liveActivity/LiveActivityTimeline.tsx` | create | Flag-gated timeline component + latency instrumentation |
| `desktop/src/features/agents/ui/ManagedAgentSessionPanel.tsx` | modify | Conditional mount of LiveActivityTimeline behind flag |
| `desktop/src/features/agents/ui/ManagedAgentRow.tsx` | modify | Flag-gated Play icon for roster busy indicator |
| `docs/nips/SLICE-1-VERIFICATION.md` | create | This verification document |

## Deviations from Spec

None. All implementation follows the amended build spec.

## Unresolved Issues

- **AC4 real-frame fixtures:** mapping tests use synthetic fixtures; real Claude + goose frame capture is an open operator-attended item (Runbook §9). Remaining ACs are operator-attended (see Runbook).

## Confidence

**90%** at code level. All 10 tick-resolution tests pass, all 24 mapping tests pass, zero regressions across 3 Rust gates + pnpm test/typecheck. Step 7 archive paging ("Show more" button) wired via `useLoadArchivedObserverEvents` / `mergeObserverEventWindows` — typecheck clean, no new test failures. The remaining 10% uncertainty is the live desktop gate runs (Test 1 on Claude + goose, R2 throughput, archive posture probe) — these require operator-attended desktop launches per the runbook below.

## Quota-Rejection Counter (Step 3 Detail)

`RelayEventPublisher::publish_event` (relay.rs:725) is fire-and-forget: it sends via mpsc and returns `Result<(), RelayError>` where the only error variant is `ConnectionClosed`. Relay admission verdicts (OK/false) are handled in the background task at relay.rs:2590-2621, not surfaced to the caller.

Counter wired as follows:
1. Static `AtomicU64` in lib.rs: `OBSERVER_QUOTA_REJECTION_COUNT`
2. `bump_observer_quota_rejection()` called from relay.rs:2618 when OK(false, "rate-limited:…") fires
3. `observer_quota_rejection_count()` read in `publish_relay_observer_event` error branch for logging
4. Expected gate-run count: **0** at 500 ms default tick (the tick is ≤ the quota floor by construction)

---

# Runbook — Operator-Attended Steps

## 1. Desktop Launch

```powershell
# From E:\TORQ-BUZZ
.\probe-env\Start-S16Desktop.ps1 -Prepare -Nest slice1
```

The `-Prepare` flag rebuilds the Rust sidecar. First launch may take several minutes.

## 2. Enable the Flag

In the desktop app:
1. Open Settings → Experiments
2. Find "Live activity" (BUZZ_LIVE_ACTIVITY)
3. Toggle ON
4. The flag override is persisted in localStorage and synced across windows

## 3. Verify Five Timeline States

With the flag on:
1. **Live:** Start a Claude or goose agent. The timeline should show events arriving in real time, with a green "Live" badge.
2. **Stale:** Wait > 10 seconds after the agent stops emitting. The badge should change to "Stale" with a clock icon.
3. **Disconnected:** Stop the relay connection (or disconnect from network). The timeline should show the disconnected banner.
4. **Archive-disabled:** With no `owner_p` subscription, the timeline should show "History off — older activity is not being saved."
5. **Incomplete:** Induce a seq gap (stop and restart the agent harness mid-turn). The amber banner should show "Activity may be incomplete (N missed)" with the correct gap count.

## 4. Test 1 Gate (≥10 samples, Claude + goose, 500 ms tick)

```powershell
# Set tick to 500 ms in the agent's environment
$env:BUZZ_OBSERVER_PUBLISH_TICK_MS = "500"

# Launch the desktop with the flag on
# Start a Claude agent session
# Let it run for ≥3 minutes of continuous activity
# Capture paint-log output (console tab → filter "[live-activity] paint")

# Repeat for goose
```

**Measurement:** The dev-only paint log emits `[live-activity] paint perf=NNNms epoch=NNN n=NN` for each batch. Match paint epochs to Rust emit epochs using the spike's batch-boundary time pairing script (`E:\TORQ-BUZZ\probe-env\s16-test1-p95-final.py` — port/copy to this branch). Record ≥10 samples per harness.

**Expected outcomes (per Q1.5):**
- Both p95 ≤ 2000 ms → **PASS**
- Goose > 2000 ms, Claude ≤ 2000 ms → **FAIL-with-cause** (shared message quota) — file Slice 5 relay item (separate rate-limit class for kind-24200 observer frames), do NOT touch quota
- Claude > 2000 ms → **hard REJECT** — stop, report, do not flip flag

**Record raw numbers here:**

| Sample | Claude emit_ms | Claude paint_ms | Claude latency | Goose emit_ms | Goose paint_ms | Goose latency |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | | | | | | |
| … | | | | | | |
| p50 | | | | | | |
| p95 | | | | | | |

**Operator gate run, 2026-09-09 (attended; measurement driven over the WebView2 debug port with `slice1-evidence/s1-cdp.mjs`, nest `slice1` via demo slug, flag on via localStorage override, `BUZZ_OBSERVER_PUBLISH_TICK_MS=500`, Git Bash).**

Emit clock: the newest painted event's own RFC3339 `timestamp` (stamped in Rust at `ObserverEvent` construction; the chunk coalescer and batch envelope both carry the LAST inner event's timestamp, so this equals the spike's "latest emit before the batch" pairing). Paint clock: `Date.now()` at React commit. One sample per painted batch (newest seq advanced). Nearest-rank percentiles. Raw logs: `slice1-evidence/slice1-*-paint-*.log`; script: `slice1-evidence/s1-p95.py`.

| Run | Harness | Load | n | p50 | p90 | p95 | max | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | claude-agent-acp (Sonnet) | prompt re-sent every ~10 s (stacked) | 59 | 846 | 3161 | 3550 | 3942 | latency rose every minute (p50 417 → 1435 → 2686 → 3632) |
| 2 | claude-agent-acp | 1 prompt/60 s, queue NOT cleared | 14 | 5564 | 8601 | 8689 | 8689 | contaminated by run-1 backlog; excluded |
| 3 | claude-agent-acp | 1 prompt/60 s, agent restarted, window reloaded | 38 | 3944 | 8041 | 8368 | 8928 | first paint already 4.2 s behind emit |
| 4 | claude-agent-acp | as run 3, + receipt/decrypt clocks | 21 | 2946 | 4417 | 4850 | 5498 | split below |
| 5 | claude-agent-acp | as run 4, + ws-enqueue clock | 19 | 10057 | 14896 | 16041 | 16041 | split below |
| G | goose (openrouter nemotron, via desktop form) | 1 prompt/60 s | 58 | 373 | 1297 | **1877** | 2140 | 0 turn errors |

**Stage split (runs 4 and 5, `s1-split.py` / `s1-split2.py`):**

| Stage | run 4 p50 / p95 | run 5 p50 / p95 | goose p50 / p95 |
| --- | --- | --- | --- |
| emit → frame signed by sidecar (`created_at`, 1 s floor) | −28 / 409 | — | — |
| signed → desktop handler entry | 1458 / 4239 | — | 206 / 937 |
| emit → websocket callback (before queue) | — | 3182 / 8669 | — |
| queue wait (ws callback → handler entry) | — | 5526 / 9346 | — |
| decrypt IPC (`decrypt_observer_event`) | 60 / 2183 | 134 / 4725 | 7 / 618 |
| render (decrypt done → paint) | 78 / 269 | 147 / 2858 | 137 / 311 |

Host during runs: i7-13700H, CPU 14–31 %, 22 GB RAM free — not resource-bound. Relay: 0 quota rejections, 0 `publish failed`, 0 dropped events, 0 Redis timeouts in the window (sidecar + relay logs).

**Verdict:** **FAIL (AC7).** claude-agent-acp p95 = 4850 ms (run 4) / 8368 ms (run 3) / 16041 ms (run 5), all > 2000 ms → per the G2A conditions this is a **hard REJECT to the builder**, not a Slice-5 escape. goose p95 = 1877 ms → PASS. The sidecar tick is not the problem (emit → signed ≤ 0.4 s). The loss is downstream: (a) frames reach the desktop's websocket callback 1.5–3 s after signing at p50 (relay delivery and/or webview main-thread starvation — not separable with current instrumentation), then (b) sit in the **sequential decrypt promise chain** in `observerRelayStore.ts` (`eventProcessingQueue.then(...)`, one `invoke("decrypt_observer_event")` round trip per frame, signature verify + decrypt on the blocking pool) for 5.5 s p50 / 9.3 s p95, with individual decrypt calls stalling up to 4.7 s. Claude frames carry more and larger coalesced chunks (inner events per frame p50 2 vs goose 1), which is why goose passes and Claude does not. Fix scope is inside R2's remit: parallel/batched decrypt, bounded queue with drop-oldest + gap state, and (separately) instrument relay delivery. Flag NOT flipped.

## 5. R2 Sustained-Throughput Check

Run for ≥3 minutes of continuous activity. Record lag (newest-painted vs newest-emitted) at 60 s intervals:

| Time | Claude lag | Goose lag |
| --- | --- | --- |
| +60s | | |
| +120s | | |
| +180s | | |

Requirement: lag must be non-monotonic (not growing without bound).

Lag = paint epoch − newest painted event's emit epoch, from the same logs.

| Time | Claude run 1 (stacked) lag | Claude run 3 (1/min) lag | Goose lag |
| --- | --- | --- | --- |
| +60 s | 300–1243 ms | 3590 ms (p50 of minute 1) | 343 ms |
| +120 s | 1435 ms (p50 of minute 2) | 1637 ms | 379 ms |
| +180 s | 2320–3550 ms | 7025 ms | 377 ms |
| post-run | run 2 (no reset) started at 3.3 s and climbed to 8.7 s while draining run-1 backlog | | |

**Verdict:** **FAIL for claude-agent-acp** — lag grows monotonically under sustained activity and continues to grow after load stops until the queue drains (minutes). **PASS for goose** (flat ~0.4 s). Cause as in §4: sequential decrypt queue behind the websocket callback; the timeline's paint path itself is not the bottleneck (render p50 < 150 ms).

## 6. Archive Posture Probe

```powershell
python E:\TORQ-BUZZ\probe-env\s16-test5-archive-posture.py
```

Requirement: zero archive rows for a fresh identity before opt-in.

**Result: PASS** (2026-09-09, fresh nest `~/.buzz-demo-slice1`, brand-new identity, no opt-in): `save_subscriptions` owner_p row exists only for kind 44200 (agent-metric archive, default-on, unrelated); kinds containing 24200 → **0 rows**; `archived_events` kind 24200 → **0 rows**. Probe: `E:\TORQ-BUZZ\probe-env\s16-test5-archive-posture.py <db> slice1-pre-opt-in`.

## 7. Readability Quote

Ask an ACP-unfamiliar reviewer to watch the timeline for ≥30 seconds and describe what they see. Requirement: "every ten seconds a turn starts, the prompt is delivered, the agent writes once, reads three chunks, the turn completes, and then it errors" — the timeline must make at least this much legible.

**Quote:** PENDING (operator statement to be pasted verbatim; requested 2026-09-09)

### Operator-attended UI observations (AC6 / AC12)
- Timeline mounts in the agent session panel (opened from the composer activity chip while the agent works) and in no other surface; the Experiments switch is **not reachable** while `preview-features.json` has `platforms: []` (the manifest filter drops the feature), so the gate ran with a localStorage override set over the debug port. The runbook's "Settings → Experiments" step cannot work until AC14 flips platforms — record as a runbook defect.
- Live/Stale/Working cues and keyboard navigation: PENDING operator statement (requested 2026-09-09).

## 8. Flag Flip (Step 12)

Only after all ACs above pass (or step 10 FAIL-with-cause + operator sign-off):

Edit `preview-features.json`:
```json
"id": "BUZZ_LIVE_ACTIVITY",
"platforms": ["desktop"]
```

`defaultEnabled` stays `false`.

## 9. Real-Frame Fixture Capture (AC4 evidence — OPEN ITEM)

`mapObserverEvents.test.mjs` currently uses synthetic, hand-constructed fixtures — the spec's real-two-harness-fixture requirement is NOT yet met with captured frames. During the Test 1 gate runs (§4), which already launch both harnesses:

1. Capture decoded observer events from both harnesses (Claude and goose) — the paint-log / batch-boundary pairing run already decodes frames; dump ≥10 consecutive decoded events per harness to JSON.
2. Save them as `desktop/src/features/agents/liveActivity/fixtures/claude-live-frames.json` and `goose-live-frames.json`.
3. Add a test block in `mapObserverEvents.test.mjs` that loads both real fixtures and asserts mapping correctness, seq-gap handling, and the no-raw-JSON invariant on the real frames (keep the synthetic unit tests as unit tests).
4. Update the test file header to state the real provenance once the fixtures are real.

**Status:** OPEN — requires operator-attended desktop launch alongside §4.

## Slice 5 Proposed Relay Item

(Issue to be filed if Goose fails Test 1 at 500 ms)
- **Title:** Separate rate-limit class for kind-24200 observer frames
- **Rationale:** Observer telemetry shares the agent's `LimitType::Messages` quota with real chat messages. At the 500 ms tick with the default 120/min quota, headroom is zero — any agent activity beyond observer emits consumes the shared budget.
- **Proposal:** Add a distinct `LimitType::ObserverMessages` with its own per-minute cap, so observer telemetry is independently rate-limited and a busy chat session can't starve the observer stream.

## Finding-39 Count

273 historical "Agent observer publish failed: Redis error: timed out" across relay logs (pre-existing). Any new occurrences during gate runs should be recorded here.

**New occurrences:** 0 during the 2026-09-09 gate runs (`Agent observer publish failed` count unchanged in `E:\TORQ-BUZZ\logselay-recovery-20260903-023357.stdout.log`; 2 Redis timeouts earlier that day, none in the run window).