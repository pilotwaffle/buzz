# Slice 0 Close-Out — 2026-09-05

Re-execution of the Slice-0 focused gate from `SLICE-0-VERIFICATION.md` (2026-09-04)
against pin `fb23b9ea0db5c052063eebbc57415e630ab275ae` on branch `torq/slice0-closeout`.

Builder: TORQ V5 Harness Builder (DeepSeek V4-Pro)
Spec: `.torq/v5/artifacts/01_design/build_spec.md` (TORQ-CONSOLE)

---

## 1. Gate Results

| # | Command | Baseline (2026-09-04) | Today (2026-09-05) | Delta | Verdict |
|---|---------|----------------------|---------------------|-------|---------|
| 1 | `cargo test -p buzz-core` | 277 unit + 2 doctests, 0 failed | 283 unit + 2 doctests, 0 failed | +6 unit, 0 failed | PASS (no regression) |
| 2 | `cargo test -p buzz-workflow` | 166 passed, 0 failed, 2 ignored | 181 passed, 0 failed, 2 ignored, 1 doctest ignored | +15 passed, 0 failed | PASS (no regression) |
| 3 | `cargo test -p buzz-acp --lib` | 682 passed, 4 failed | 880 passed, 0 failed | +198 passed, -4 failed | PASS (improvement — known failures resolved) |
| 4 | `cargo test -p buzz-relay --lib` | 860 passed, 4 failed, 40 ignored | 1026 passed, 6 failed, 89 ignored | +166 passed, +2 failed, +49 ignored | **REGRESSION** — 2 new failures in `api::media::tests` (sidecar-dependent) |
| 5 | `pnpm typecheck` (desktop) | pass | 1 error | +1 error | **REGRESSION** — `TimelineMessageList.tsx:749` type assignment error |
| 6 | `pnpm test` (desktop) | 4,539 passed, 0 failed | 6,432 passed, 22 failed | +1,893 passed, +22 failed | **REGRESSION** — 22 failures, mostly Tauri `window.__TAURI_INTERNALS__` / relay timeout |
| 7 | `pnpm build` (desktop) | pass | FAILED (exit 2) | — | **REGRESSION** — same TS error as typecheck blocks build |

**Buzz-relay regression detail (command 4):** 6 failing tests, all in `api::media::tests`:
1. `media_read_accepts_range_header_only_after_auth`
2. `upload_concurrency_limit_is_scoped_by_community`
3. `media_reads_reject_unauthenticated_get_and_head_before_sidecar_gate`
4. `upload_rate_limiter_is_scoped_by_community`
5. `media_read_with_valid_server_scoped_token_reaches_sidecar_gate`
6. `media_read_rejects_upload_verb_wrong_server_and_wrong_x`

These require a running media sidecar (MinIO at `127.0.0.1:19000`). The sidecar connectivity may have changed since the baseline.

**Desktop test regression detail (command 6):** 22 failures across:
- 1 failure: `N cards share a snapshot, one poll, failure recovery and live subscription lifecycle` (Tauri channel subscription)
- 1 failure: `provenance context follows exact local inventory and rejects failed cached reads` (Tauri)
- 2 failures: `pointer activation` / `context-menu activation` / `failed reopen` (pointer/retry UI)
- 3 failures: `archive sync` tests (start gate, lifecycle leases, realm ownership)
- 8 failures: `loadThreadReplies` / `useThreadReplies` (relay timeout / expected event absent)
- 1 failure: `selected review chrome and diff query stay aligned across fetch phases`
- 3 failures: `does not start the backend task` / `stops the backend task` (Tauri backend)
- 3 failures: `Failed to subscribe to live channel updates` (Tauri `window.__TAURI_INTERNALS__.transformCallback`)
- 1 failure: `Failed to hydrate visible reactions` (relay timeout)

**Desktop typecheck / build detail (commands 5, 7):**
```
src/features/messages/ui/TimelineMessageList.tsx(749,20): error TS2322:
  Type '(item: VirtualizedTimelineItem) => number' is not assignable to type 'number'.
```

**PATH workaround applied for command 3:** `$env:PATH = "C:\Program Files\Git\usr\bin;$env:PATH"` (per-invocation only, not session-wide).

Archive browser tests, focused frontend consent tests, native desktop archive tests, Clippy/format, and full workspace `cargo test` were not re-executed this run; 2026-09-04 `SLICE-0-VERIFICATION.md` remains last evidence for those gates.

---

## 2. Fixture Inventory

### DG (Delegation) — 40 cases

Command: `python -c "import json; d=json.load(open('docs/nips/NIP-DG.fixtures.json')); print(f'Total: {len(d[\"cases\"])}'); [print(c['name']) for c in d['cases']]"`

All 40 names listed in `SLICE-0-CLOSEOUT-2026-09-05.evidence/dg-fixture-names.txt`. Count matches expected 40.

### AO (Agent Observer) — 55 cases

Command: same enumeration against `docs/nips/NIP-AO.fixtures.json`. All 55 names listed in `SLICE-0-CLOSEOUT-2026-09-05.evidence/ao-fixture-names.txt`. Count matches expected 55.

### Anti-enumeration

Fixture name `target_enumeration_probe` is present in `NIP-DG.fixtures.json` (case index 3). Covered by the DG manifest test.

### Manifest Tests

| Command | Result |
|---------|--------|
| `cargo test -p buzz-core --lib fixture_manifest_executes_every_declared_case` | 1 passed, 0 failed |
| `cargo test -p buzz-core --lib shared_malicious_fixture_manifest_is_executable_and_complete` | 1 passed, 0 failed |
| `cargo test -p buzz-core --lib cross_owner_fails_closed_with_enumeration_safe_message` | 1 passed, 0 failed |

### NIP-DG Golden Hash Vector

The `hash_vector` is present in `NIP-DG.fixtures.json` (type: dict). Hash preimage domain: `buzz-delegation/request/v1` (NIP-DG.md line 13). Verified by `fixture_manifest_executes_every_declared_case` (same test as above) — the hash vector assertion is inside that test. 1 passed, 0 failed.

---

## 3. Kind-24200 Evidence

### BLOCKED

**Missing capability: Schnorr (BIP340) signature library for NIP-42 AUTH events.**

The 24200 live probe requires signing a NIP-42 AUTH event with a Schnorr (BIP340) signature to authenticate against the relay at `ws://127.0.0.1:3300`. The relay requires AUTH for all connections — it sends a challenge frame immediately on connect and rejects subscriptions with `auth-required: not authenticated` until a valid signed AUTH event is received.

The builder's Python environment has `cryptography` (ECDSA only, no Schnorr) and `nacl` (Ed25519 only). No nostr Python library with Schnorr support is installed. The `buzz.exe` CLI requires a private key for relay operations but does not expose raw subscription/listen functionality needed to capture observer frames.

**What was attempted:**
- Connected to `ws://127.0.0.1:3300` — relay sends `["AUTH","<challenge>"]` immediately
- Sent AUTH event with ECDSA signature — relay returns `OK` (event accepted) but does not authenticate the connection (Schnorr verification fails server-side)
- Subscription to `kinds:[24200], #p:[ephemeral]` rejected with `auth-required: not authenticated`

**What is needed to unblock:**
- A Schnorr (BIP340) signing library in the builder's Python environment (e.g., `coincurve`, `secp256k1` with Schnorr support, or a `nostr` Python SDK), OR
- Operator interaction to capture the owner-client receive transcript directly, OR
- A `buzz.exe` subcommand that supports raw subscription with relay AUTH

**What was NOT attempted (per spec):**
- No operator private key material was touched
- No handcrafted frames signed with operator keys
- The ephemeral keypair generated for the negative case (`3559687f...`) was used only for the unsuccessful AUTH attempt and was never written to disk

### Relay Receipt

- Scheduled task `TORQ-Buzz-PermanentRelay`: Ready
- Readiness endpoint `http://127.0.0.1:8380/_readiness`: HTTP 200, `{"status":"ready"}`
- Relay URL: `ws://127.0.0.1:3300`
- Config: `BUZZ_REQUIRE_AUTH_TOKEN=false`, `BUZZ_REQUIRE_RELAY_MEMBERSHIP=false`
- AUTH challenge received on connect — relay requires NIP-42 AUTH for all connections

---

## 4. Still-Open Items Blocking Full Slice-0 Closure

1. **(a) §16 live spike** — next packet (Slice 1). The spike validates the full agent-computer lifecycle end-to-end: managed agent creation, observer frame generation, owner delivery, and control commands. Not yet executed.

2. **(b) Operator DECISION on legacy kind-24200 consent reset** (PRD §13 line 269). This is a decision, not a task. Pending operator input.

3. **(c) Operator DECISION on automatic age/count/byte pruning** (SLICE-0-VERIFICATION.md lines 84–88). This is a decision, not a task. Pending operator input.

The 24200 BLOCKED in section 3 is additional evidence still required; it does not replace (a)–(c).

---

## 5. Contract Extension Pending

FR-3/FR-4 token budgets (`token_budget_per_run`, `token_budget_per_day`, per-delegation token budget) do not yet exist in `InvokeAgent`, `DelegationRequest`, or the NIP-DG hash preimage. These are carried by Slice 3/4 packets. Recorded; no action taken.

---

## Integrity

- `docs/nips/SLICE-0-VERIFICATION.md` is byte-identical to its state at pin `fb23b9ea0` (not modified).
- `preview-features.json` and `desktop/src/shared/features/agentComputerFlags.ts` are byte-identical to the pin (no diff).
- No `.rs`, `.ts`, `.tsx`, test, fixture, NIP, or flag files were modified.
- This document and the evidence directory are the only new files on branch `torq/slice0-closeout`.