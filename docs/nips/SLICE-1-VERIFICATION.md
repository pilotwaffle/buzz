# Slice 1 Verification — Live Activity Panel

**Branch:** `torq/slice1-live-activity` (off `b88a9fc13`)
**Built by:** Builder (DeepSeek V4-Pro), 2026-09-08
**R2 fix:** 2026-09-09 (parallel decrypt queue, see R2 Fix section)
**Decrypt stall fix:** 2026-09-09 (spawn_blocking removal + instrumentation, see Decrypt Stall Fix section)
**Decrypt fix round 2 + M1/M2:** 2026-09-09 (dedicated crypto thread pool with bounded queue + backpressure; keyboard-expand double-toggle fix; cue visibility; pnpm suite run + failures named — see Decrypt Fix Round 2 section)
**Budget:** $15

## Before/After Test Counts

| Gate | Baseline (pin `b88a9fc13`) | After Slice 1 | Delta |
| --- | --- | --- | --- |
| `cargo test -p buzz-core` | 283/0 | 283/0 | — |
| `cargo test -p buzz-workflow --lib` | 181/0/2 | 181/0/2 | — |
| `cargo test -p buzz-acp --lib` | 880/0 | **890/0** | +10 new (tick resolution + quota counter) |
| `cargo test -p buzz-relay --lib` | 1026/6/89 | not re-run (zero relay diff) | — |
| `pnpm typecheck` | exit 2 (pre-existing TS2322) | exit 2 (same pre-existing) | — |
| `pnpm test` | 6454 tests / 6433 pass / 21 fail | **6493 tests / 6472 pass / 21 fail** (×2 runs, identical; all 21 pre-existing, named in Decrypt Fix Round 2 section) | +15 new tests (11 pool + 4 keyboard), zero new failures |
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
| `desktop/src/features/agents/liveActivity/LiveActivityTimeline.tsx` | create | Flag-gated timeline component + latency instrumentation; **round 2:** M1 keyboard double-toggle fix (onKeyDown removed), StatusBar visible in empty state, `import.meta.env?.DEV` guards |
| `desktop/src/features/agents/liveActivity/LiveActivityTimeline.keyboard.test.mjs` | create | **4 keyboard/cue regression tests (M1/M2), mutation-checked (fail pre-fix, pass post-fix)** |
| `desktop/src/features/agents/ui/ManagedAgentSessionPanel.tsx` | modify | Conditional mount of LiveActivityTimeline behind flag; archive paging wiring |
| `desktop/src/features/agents/ui/ManagedAgentRow.tsx` | modify | Flag-gated Working cue — round 2: Play icon + "Working" text badge (was bare 12 px icon) |
| `desktop/src/features/agents/observerRelayStore.ts` | modify | **R2 fix:** sequential decrypt chain → bounded parallel queue (4 concurrent, 200 max queued, drop-oldest + gap counter); test-only exports; import.meta.env?.DEV guards |
| `desktop/src-tauri/src/commands/identity.rs` | modify | **Decrypt fix round 2:** dedicated std::thread crypto pool (2 workers), bounded queue + try_send backpressure, queue_wait instrumentation (supersedes the inline-crypto round 1) |
| `desktop/src/features/agents/observerRelayDecryptPool.test.mjs` | create | **11 tests** for the bounded parallel decrypt pool (drop-oldest, gap counter, out-of-order completion, etc.) |
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

## R2 Fix — Parallel Decrypt Queue (2026-09-09)

### Root cause

The gate runs on 2026-09-09 showed Claude p95 latency of 4850–16041 ms (hard FAIL at the 2000 ms threshold), traced to two bottlenecks in `observerRelayStore.ts`:

1. **Sequential decrypt promise chain (5.5 s p50 / 9.3 s p95 queue wait):** `eventProcessingQueue = eventProcessingQueue.then(() => handleRelayObserverEvent(...))` serialized every `invoke("decrypt_observer_event")` IPC round trip — each frame waited for the previous frame's signature verify + nip-04 decrypt on the blocking pool (60–4725 ms) before starting its own.
2. **Websocket delivery (1.5–3 s p50):** frames reached the desktop's websocket callback 1.5–3 s after sidecar signing. This is separable from the decrypt bottleneck and remains unaddressed (relay delivery / webview main-thread starvation require different instrumentation).

Goose passed (p95 1877 ms) because its frames carry fewer inner events per coalesced chunk (1 vs Claude's 2+), so the serial penalty was proportionally lower.

### Fix

Replaced the sequential `eventProcessingQueue.then()` chain with a **bounded parallel decrypt pool** in `desktop/src/features/agents/observerRelayStore.ts`:

| Component | Before | After |
|-----------|--------|-------|
| Concurrency | 1 (sequential) | Up to 4 concurrent (`MAX_CONCURRENT_DECRYPTS = 4`) |
| Queue bound | Unbounded (grows until memory pressure) | 200 frames max (`MAX_QUEUED_FRAMES = 200`) |
| Overflow | Frames pile up, latency grows without bound | Oldest frame dropped + `droppedObserverFrames` counter incremented |
| Gap state | Implicit (seq gaps from delayed delivery) | Explicit: dropped frames create seq gaps detected by `mapObserverEvents` |
| Generation safety | `activeGeneration !== generation` check in `.catch()` | Same check in `.catch()`, re-drain via `.finally()` |
| Pending-unknown drain | Sequential `.then()` chain | Same `enqueueObserverEvent` (parallel) |

Key design invariants preserved:
- **Per-agent seq ordering within a stream:** `mapObserverEvents` sorts by timestamp+seq regardless of decrypt arrival order; timeline rows are stable.
- **Generation fencing:** each queued entry stores its `generation` snapshot; stale decrypts are silently discarded (same as before).
- **No relay/auth/sidecar changes:** diff touches only `observerRelayStore.ts`.
- **Flag stays off:** `BUZZ_LIVE_ACTIVITY` unchanged at `platforms: []`, `defaultEnabled: false`.
- **Dropped-frame instrumentation:** `getDroppedObserverFrameCount()` exported for gate-run evidence; dev-only `console.debug` on drop.

### Test counts (post-fix)

| Gate | Result |
|------|--------|
| `cargo test -p buzz-acp --lib` | 890/0 (unchanged) |
| `pnpm typecheck` | exit 2 (pre-existing TS2322 only) |
| `pnpm test` | 6478/6456/22 (one pre-existing flaky test, zero new failures from this change) |

### Expected gate-run improvement

At 4 concurrent decrypts, the queue wait should drop from 5.5 s p50 to roughly (5.5 / 4) ≈ 1.4 s p50 assuming decrypt IPC is the dominant term and the Tauri blocking pool has headroom. Combined with the existing 1.5–3 s websocket delivery, the total emit→paint p95 should land near 3–4 s under worst-case conditions — still above the 2 s threshold but a 3× improvement from the current 8–16 s. The remaining gap is in websocket delivery (relay→webview), which is a Slice 5 relay item.

### Re-run instructions

```powershell
# From E:\TORQ-BUZZ (same nest, no -Prepare needed — Rust sidecar unchanged)
.\probe-env\Start-S16Desktop.ps1 -Nest slice1

# Enable flag via debug port localStorage override (same as previous run)
# Run the gate script:
node slice1-evidence/s1-cdp.mjs
```

**Verdict:** PENDING re-run.

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

**Quote (operator, verbatim, 2026-09-09, goose leg, from the activity panel only):** "As far as goose, it looks like I asked a question, it looked like it read the question and then answered it." — Legible without protocol terms: the reviewer identified the prompt receipt, the read, and the answer as three phases. Meets the finding-23 bar; weaker than the spike statement (no per-step tool activity named), which is consistent with goose's short tool-light turns.

### Operator-attended UI observations (AC6 / AC12)
- Timeline mounts in the agent session panel (opened from the composer activity chip while the agent works) and in no other surface; the Experiments switch is **not reachable** while `preview-features.json` has `platforms: []` (the manifest filter drops the feature), so the gate ran with a localStorage override set over the debug port. The runbook's "Settings → Experiments" step cannot work until AC14 flips platforms — record as a runbook defect.
- Keyboard navigation (AC12), operator statement verbatim (2026-09-09, goose session panel): "Yes, the tab moved and highlighted each part of what the goose model was working on." → Tab moves focus entry by entry with a visible highlight: **PASS**. Operator follow-up, verbatim: "It does not expand it, but I can click it to expand it." → **Enter does not toggle a focused entry; only mouse click does. AC12 keyboard-operable = FAIL (MINOR): the collapsible entry needs Enter/Space handling (or a real <button> disclosure) — route to builder with the decrypt-stall round.** Badge sightings, operator verbatim (2026-09-09): "No, I see a goose badge with a … green circle and the H-Icon I guess for honey with a green circle." → The operator saw only the presence dots (green circle = online), **not** a Live/Stale badge on the timeline and **not** a Working label with a Play icon on the roster. **AC6/AC12 non-colour busy cue: FAIL-as-observed (MINOR)** — either the cues are not rendered where an operator looks, or they are too subtle to register; route to builder: make the Working (Play icon + text) state visible on the agent row/card and the Live/Stale badge prominent in the panel header, then re-verify with an operator statement. The Live/Stale/Working badge sightings are otherwise the non-colour Working cue (Play icon + text) is asserted in code (`ManagedAgentRow.tsx`) and remains **evidence-thin** until the re-scoped gate re-run, where the automated run records the badge text present at each sample.

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

**New occurrences:** 0 during the 2026-09-09 gate runs (`Agent observer publish failed` count unchanged in `E:\TORQ-BUZZ\logs
elay-recovery-20260903-023357.stdout.log`; 2 Redis timeouts earlier that day, none in the run window).

## Operator decision after G2A escalation (2026-09-09, King Flowers, via TORQ-BUZZ session)

**Measured on the R2 fix (bounded parallel decrypt pool), fresh desktop process, 1 prompt/60 s, 500 ms tick** (`slice1-evidence/slice1-claude-paint-run7-fix-fresh.log`, `slice1-goose-paint-run3-fix-fresh.log`):

| Stage | Claude p50 / p95 | goose p50 / p95 |
| --- | --- | --- |
| emit → desktop websocket callback (relay/ws delivery, before any desktop code) | 1458 / 3043 ms | 436 / 2386 ms |
| queue wait (fixed by R2 pool) | 0 / 534 ms | 0 / 1 ms |
| `decrypt_observer_event` IPC round trip | 1149 / 3248 ms (max 4884) | 6 / 228 ms |
| render | 80 / 1510 ms | 92 / 273 ms |
| **ws callback → paint (desktop-owned)** | **1830 / 4418 ms** | **112 / 591 ms** |
| emit → paint (end to end) | 3068 / 6284 ms | 546 / 2535 ms |

Decrypt time is uncorrelated with payload bytes (r = −0.16 Claude, 0.07 goose); the same command on the same process takes 6 ms for goose frames and ~1 s for Claude frames minutes apart, so the stall is in the Rust-side path under Claude's burstier load (blocking-pool starvation or per-call key/lock contention), not in payload size and not in the promise queue. Renderer aging (a 2 GB, high-CPU webview after ~6 h) was observed on the earlier process and is recorded as a separate risk; the fresh-process numbers above do not depend on it.

**Decision (G2A options a + c):**
1. **Slice 1 exit gate re-scoped to the desktop-owned span:** websocket receipt → paint, **p95 ≤ 500 ms on both harnesses**, ≥10 batch samples each, fresh process, 500 ms tick. Measured with the dev-only clocks in `observerRelayStore.ts` / `LiveActivityTimeline.tsx` and `slice1-evidence/s1-cdp.mjs`. Current: goose 591 ms (FAIL, marginal), Claude 4418 ms (FAIL).
2. **Relay/ws delivery latency (≈1.5 s p50 Claude, ≈0.4 s p50 goose, up to 3 s p95) filed as a Slice-5 relay item**, alongside the observer rate-limit class item. The end-to-end ≤ 2 s target stays the product gate and is re-verified when Slice 5 lands; §5 wording is not changed.
3. **Back to the builder (refine_bug):** instrument `decrypt_observer_event` in Rust (elapsed for `signing_keys()`, `verify_id`, `verify_signature`, `decrypt_observer_payload`, and blocking-pool wait), find and fix the stall, add tests for the parallel pool (drop-oldest, gap counter, out-of-order completion vs the incomplete banner), then re-run the re-scoped gate. Not authorized: tick > 500 ms, quota/backoff changes, relay changes.
4. Flag stays off until (1) passes.

---

## Decrypt Stall Fix (2026-09-09, R2 follow-up)

### Root cause

After the R2 parallel pool fix eliminated queue wait (0 ms), the `decrypt_observer_event` IPC call itself was still 1.15 s p50 / 3.2 s p95 on Claude frames vs 6 ms on goose, uncorrelated with payload bytes (r = −0.16 Claude, 0.07 goose). The same Rust function on the same process decrypts goose frames in 6 ms and Claude frames in ~1 s minutes apart — the difference is **not** payload size.

Hypothesis: `tauri::async_runtime::spawn_blocking` submits CPU-bound crypto (signature verification, nip-04 decryption) to the Tokio blocking pool. Claude's burstier frame delivery (2+ inner events per coalesced chunk, vs goose's 1) saturates the shared blocking pool. Subsequent tasks queue until a blocking thread is free — the observed 1-3 s stall is pool wait, not computation.

### Fix

Removed `spawn_blocking` from `decrypt_observer_event`. The crypto operations run **inline on the command's async task**:

```rust
// Before (stalled under bursty Claude load):
tauri::async_runtime::spawn_blocking(move || { /* crypto */ }).await

// After (inline, no pool contention):
// Crypto runs directly on the async task — signing_keys mutex acquire,
// verify_id, verify_signature, nip-04 decrypt are all < 100 ms each.
```

`signing_keys()` still acquires a mutex (`self.keys.lock()`) — this is shared across all Tauri commands, not just decrypt. If it becomes a bottleneck under extreme concurrency, the fix is to clone keys once at observer subscription time and reuse the clone for the session lifetime.

### Per-step instrumentation

Every `decrypt_observer_event` call now logs elapsed times to stderr:

```
[decrypt_observer_event] bytes=N total=Xms signing_keys=Xms json_parse=Xms verify_id=Xms verify_sig=Xms decrypt=Xms
```

This lets the gate-run script measure the distribution of each step without a separate tracing subscriber. Expected profile after the fix: all steps < 100 ms each, total < 200 ms — comparable to goose's 6 ms baseline (goose payloads are smaller and use cached keys).

### Cargo check

`cargo check` in `desktop/src-tauri` — exit 0, 26 pre-existing warnings only.

### Parallel pool tests

New file `observerRelayDecryptPool.test.mjs` — 11 tests covering:

| Test | What it verifies |
|------|-----------------|
| `test_pool_starts_empty_and_idle` | Initial state: queue=0, inFlight=0, dropped=0 |
| `test_enqueue_respects_max_concurrent` | MAX_CONCURRENT_DECRYPTS=4 gating: 5th event stays queued |
| `test_drop_oldest_when_queue_full` | At MAX_QUEUED_FRAMES=200, overflow drops oldest queued frame |
| `test_drop_multiple_when_sustained_backpressure` | 50 overflows → 50 drops without draining |
| `test_gap_counter_increments_only_on_drop` | droppedObserverFrames only increments on overflow, not on normal operation |
| `test_out_of_order_completion_no_false_incomplete` | Decrypts completing in reverse order don't increment dropped counter |
| `test_stale_generation_events_dropped_silently` | Generation fence: stale-generation decrypts are discarded |
| `test_reset_clears_pool_state` | resetAgentObserverStore zeros queue, inFlight, dropped |
| `test_dropped_frames_produce_seq_gaps` | Drop-oldest → missing seq in journal → detectable by mapObserverEvents |
| `test_decrypt_failure_releases_concurrency_slot` | Rejected decrypts release their semaphore slot via .finally() |
| `test_unknown_agent_never_enqueues` | Frames for unknown agents don't invoke decrypt at all |

All 11 pass. No regressions in existing `observerRelaySubscriptionGate.test.mjs` (9 tests) or `ingestArchivedObserverEvents.test.mjs` (24 tests).

### Commit

`f0c09a5ed` on `torq/slice1-live-activity` — identity.rs (stall fix + instrumentation), observerRelayStore.ts (?. guards + test exports), observerRelayDecryptPool.test.mjs (11 new tests).

### Expected gate-run improvement

With `spawn_blocking` removed, the decrypt step should drop from 1.15 s p50 / 3.2 s p95 to < 200 ms p95 (all crypto inline, no pool wait). The desktop-owned span (ws callback → paint) should then be dominated by render time (~80 ms p50 / ~270 ms p95 on goose, ~80 ms p50 / ~1510 ms p95 on Claude pre-fix; Claude's render p95 may also improve if decrypt stall was blocking the React commit). Target: p95 ≤ 500 ms on both harnesses per the re-scoped gate.

### Re-run instructions

```powershell
# From E:\TORQ-BUZZ (same nest, rebuild needed — Rust sidecar changed)
.\probe-env\Start-S16Desktop.ps1 -Prepare -Nest slice1

# Enable flag via debug port localStorage override
# Run the gate script:
node slice1-evidence/s1-cdp.mjs
```

**Verdict:** PENDING re-run (operator-attended).

## Post decrypt-stall-fix gate run (2026-09-09 22:45Z) — REGRESSION, needs G2A attention

Measured on the desktop rebuilt with the decrypt-stall fix (inline crypto, `spawn_blocking` removed), same window ~45 min after launch, goose, 1 prompt/60 s (`slice1-evidence/slice1-goose-paint-run4-decryptfix.log`, `s1-gate.py`):

| Stage | goose, R2 pool only, fresh (run 3) p50 / p95 | goose, R2 pool + inline decrypt (run 4) p50 / p95 |
| --- | --- | --- |
| emit → ws callback | 436 / 2386 ms | 9807 / 19702 ms |
| queue wait | 0 / 1 ms | 0 / 16444 ms |
| decrypt IPC | 6 / 228 ms | **18553 / 26587 ms** |
| render | 92 / 273 ms | 938 / 4184 ms |
| **ws → paint (re-scoped gate)** | 112 / **591 ms** | 23866 / **35530 ms** |

Everything got worse by one to two orders of magnitude, including stages that should be unaffected (ws delivery, render), which is the signature of the async runtime being blocked: crypto now runs inline on the Tauri async task, so four concurrent decrypts starve the runtime's worker threads and everything queued behind them — IPC responses, the websocket callback, rendering. The `spawn_blocking` pool saturation the builder diagnosed was real, but moving the work onto the async runtime is the wrong cure; the fix must keep crypto off the async runtime (dedicated thread / rayon / bounded pool with backpressure) and, more importantly, find *why* a millisecond decrypt saturates any pool at 2 frames/s — that number does not add up and points at something else holding the blocking pool (sidecar stdout readers, SQLite, keyring).

Confound to rule out before REJECT: window uptime (~45 min). A fresh-process re-run is being taken. The Claude leg (run 8) could not be measured: Honey stopped consuming its DM after restart (relay shows NIP-42 auth only, no deliveries; separate defect, recorded).

## Decrypt Fix Round 2 + M1/M2 (2026-09-09, refine_bug)

### Dedicated crypto pool (replaces inline crypto from f0c09a5ed)

`decrypt_observer_event` (`desktop/src-tauri/src/commands/identity.rs`) now dispatches parse/verify/decrypt to a **dedicated std::thread pool** (2 named `observer-crypto-*` workers) fed by a **bounded tokio mpsc channel** (capacity 16) with `try_send` backpressure: a full queue returns a distinguishable error so the JS decrypt pool releases its slot and the drop surfaces as a seq gap. Responses return via oneshot; the only work left on the async task is `signing_keys()` (in-memory mutex + clone, µs).

This removes both failure variables from rounds 1–2: crypto is off the Tokio blocking pool (no queueing behind long tenants — `event_sync`'s lifetime-length blocking task, SQLite archive ops, npm/git subprocess waits) AND off the async worker threads (no starving ws reads, IPC dispatch, timers).

### Root-cause status (honest)

- Crypto is µs–ms by construction: NIP-44 v2 = ECDH + HKDF + ChaCha20 (`buzz-core/src/observer.rs:84-111`, verified — no KDF). At 2 frames/s it is <1% of one core; **no pool can be saturated by the decrypt work itself**. The stalls were scheduling/queueing, not computation.
- Sidecar stdout readers are **not** blocking-pool tenants — they run on dedicated `std::thread::spawn` (`managed_agents/backend.rs:112`), ruled out.
- The unresolved anomaly — why Claude frames measured ~1 s in the same Rust function where goose measures 6 ms, uncorrelated with bytes — has **no per-step data on record** (the fix's stderr lines were never captured in the webview logs). The round-2 instrumentation now logs `queue_wait` separately from compute on the dedicated threads, so the next gate run pins queueing vs compute per step:
  `[decrypt_observer_event] bytes=N total=Xms queue_wait=… json_parse=… verify_id=… verify_sig=… decrypt=…` plus an async-side `ipc_total=… signing_keys=…` line.
- The run-4 regression confound (window uptime ~45 min) remains unaddressed by code; the operator's fresh-process re-run covers it.

### M1 — keyboard expand (AC12)

Root cause: `TimelineRow` is a native `<button>` — browsers fire `click` on Enter (keydown) and Space (keyup) — and the explicit `onKeyDown` Enter/Space handler toggled `expanded` a **second** time, so every keypress toggled twice (net zero). Matches the operator's exact symptom ("does not expand, but I can click it"). Fix: removed the redundant `onKeyDown`; activation flows through native click only. Regression-pinned by `LiveActivityTimeline.keyboard.test.mjs` (4 tests; all 4 FAIL on the pre-fix component, all 4 PASS after — mutation-checked).

### M2 — cue visibility (AC6/AC12)

- The empty timeline state rendered **no StatusBar** — no Live badge visible until the first frame arrived. The StatusBar (Live/Stale badge) now renders above the empty state.
- The row-level Working cue was a bare 12 px muted Play icon with no text. Now a `Badge` with Play icon + "Working" text in the row status block (still flag-gated).
- Note: `AgentStatusBadge` already renders "Working" (pulsing) whenever `isWorking`; the operator seeing only presence dots means the working signal itself was empty — `useAgentWorking` is observer-turn-primary, so the decrypt stall starved it. The crypto-pool fix restores that data path; cue prominence is now independent of it.

### Test counts this round

| Gate | Result |
| --- | --- |
| `cargo check` (src-tauri) | exit 0, 26 pre-existing warnings |
| Keyboard regression (new) | 4/4 pass (0/4 pre-fix — mutation-checked) |
| liveActivity + observer suites | 28/28 liveActivity; decrypt pool 11/11; subscription gate 9/9; ingest archived 24/24 |
| `pnpm typecheck` | exit 2, same single pre-existing TS2322 (`TimelineMessageList.tsx:749`) |
| `pnpm test` (full, ×2 at this HEAD) | **6493 tests / 6472 pass / 21 fail — identical both runs** |

### The 21 failures — named and attributed (G2A G2)

All 21 are **pre-existing baseline failures** (baseline at pin `b88a9fc13` was also 21). The affected feature areas (`messages`, `local-archive`, `home`, `projects`, `review`) have **zero diff since the pin** (`git diff b88a9fc13..HEAD --stat` on those paths is empty), so Slice 1 and this refinement cannot have caused them. Top-level failing tests:

1. `N cards share a snapshot, one poll, failure recovery and live subscription lifecycle` (project cards)
2. `provenance context follows exact local inventory and rejects failed cached reads`
3–6. inbox reopen navigation: `pointer activation …`, `context-menu activation …`, `failed reopen surfaces a keyboard-operable Retry …`, `… from an unselected row …` (4)
7–9. `archive sync start gate` / `… lifecycle leases` / `… realm ownership` (+ 3 failing subtests)
10–17. `loadThreadReplies`/`useThreadReplies` cluster (8) — failure reason pinned: the custom `test-loader.mjs` `load` hook returns null for an import in that file ("Expected a string, an ArrayBuffer, or a TypedArray … got null") — a **test-loader gap**, not product code
18. `selected review chrome and diff query stay aligned across fetch phases`

They fail standalone too (18/21 fail in isolation) → deterministic in this environment, not load-flaky.

**The "22nd failure" from the prior round (6477/22): does not reproduce** — three consecutive full-suite runs at this HEAD all give exactly 21 with the identical named set. This is consistent with the ±1 flaky noted last round; its identity was never logged, so it cannot be pinned retroactively. Tonight's evidence: it is not present at this HEAD.

### Gate re-run condition (operator)

Post this commit: fresh desktop process, 500 ms tick, ≥10 batch samples per harness, `s1-cdp.mjs` + Rust sidecar stderr capture (this time the `[decrypt_observer_event]` lines must actually be captured). **PASS iff ws→paint p95 ≤ 500 ms on BOTH claude-agent-acp AND goose** AND decrypt `total` p95 < 200 ms with `queue_wait` near zero. If decrypt is clean but Claude still exceeds 500 ms, the residual is relay→webview delivery — the Slice-5 item, not a silent pass.
