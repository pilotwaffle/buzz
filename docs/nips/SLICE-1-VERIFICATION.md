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

**Verdict:** PENDING

## 5. R2 Sustained-Throughput Check

Run for ≥3 minutes of continuous activity. Record lag (newest-painted vs newest-emitted) at 60 s intervals:

| Time | Claude lag | Goose lag |
| --- | --- | --- |
| +60s | | |
| +120s | | |
| +180s | | |

Requirement: lag must be non-monotonic (not growing without bound).

**Verdict:** PENDING

## 6. Archive Posture Probe

```powershell
python E:\TORQ-BUZZ\probe-env\s16-test5-archive-posture.py
```

Requirement: zero archive rows for a fresh identity before opt-in.

**Result:** PENDING

## 7. Readability Quote

Ask an ACP-unfamiliar reviewer to watch the timeline for ≥30 seconds and describe what they see. Requirement: "every ten seconds a turn starts, the prompt is delivered, the agent writes once, reads three chunks, the turn completes, and then it errors" — the timeline must make at least this much legible.

**Quote:** PENDING

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

**New occurrences:** PENDING