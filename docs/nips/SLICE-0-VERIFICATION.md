# Agent Computer Slice 0 verification

Date: 2026-09-04

Scope: inert delegation/control contracts, workflow action schema, rollout flags,
and archive consent/admission safeguards. Runtime Slices 1–5 are not complete.

## Implemented

- AO: 55 unique executable fixture cases. DG: 40 unique cases. Authority revision
  changes are checked before CAS, effects, and delegation outbox consumption.
- Store outcome classifiers are crate-private reference models. Durable adapters,
  persistent tombstones, and atomic pause release/audit still need runtime proof.
- `invoke_agent` has strict parameters and cadence checks; dispatch is disabled.
- All four Agent Computer rollout flags default off and are hidden from settings.
- SQLite subscriptions are the sole activity archive consent record. Fresh or
  recovered identities remain off; browser markers cannot restore consent.
- Archive commit takes an immediate SQLite write transaction before reading
  consent. Wrong owner scope, malformed kinds, and missing consent deny saves;
  a failed consent query aborts the batch.
- New observer saves are limited to 256 KiB per event, 10,000 events, and 64 MiB
  logical UTF-8 JSON per identity and relay. At capacity, new saves are refused.
  Existing history and subscriptions are preserved; there is no age expiry.

## Verification

| Gate | Result |
|---|---|
| Desktop `pnpm test` | 4,539 passed, 0 failed |
| Desktop `pnpm typecheck` | Passed |
| Desktop `pnpm build` | Passed; production assets rebuilt after E2E verification; chunk size/dynamic import warnings |
| Archive browser tests | 8 passed, using E2E build and Vite preview |
| Focused frontend consent tests | 21 passed |
| `cargo test -p buzz-core` | 277 unit tests and 2 doctests passed |
| Core strict Clippy and formatting | Passed |
| Workflow focused verification | 166 passed, 2 ignored; scoped Clippy/format passed |
| Full `cargo test --workspace --no-fail-fast` | Compiled; failed in 10 test targets |
| Native desktop `cargo check` | Passed in 14m 46s; existing Windows unused/dead-code warnings |
| Native desktop archive tests | 12 new fixtures and all 78 archive tests passed, 0 failed |
| Native desktop `cargo build -p buzz-desktop` | Passed in 3m 58s after production frontend rebuild; existing Windows warnings |
| Local desktop `pnpm tauri build --debug --no-bundle` | Passed in 4m 10s; embedded production assets and all five adjacent Windows sidecars verified |
| Root `cargo build --workspace` | Passed in 16m 28s |
| Desktop static checks | Biome passed with existing warnings; text/pubkey guards passed; file-size check passed using `CHECK_FILE_SIZES_BASE=HEAD` because `origin/main` is absent |

The failed full-workspace targets are `buzz-acp --lib`, `buzz-agent --lib`,
`buzz-agent` integration targets `databricks_oauth`, `fake_llm`,
`golden_transcripts`, and `regressions`, `buzz-backend-kubernetes`,
`buzz-dev-mcp --lib`, `buzz-relay --lib`, and `git-sign-nostr --lib`.
They have not all been diagnosed or fixed. Confirmed examples:

- ACP's inert-process fixture requires `cat`; the isolated failing test passes
  with Git's Unix utilities on PATH. With that environment the ACP unit binary
  reports 682 passed and 4 failed, including timing and capture-file assertions.
- ACP capture scripts interpolate Windows paths into shell commands unquoted;
  generated capture files appear under malformed filenames in the working directory.
- Relay's unit binary reports 860 passed, 4 failed, 40 ignored. Failures include
  shell/Rust HMAC mismatch, mesh timeout, and a tracing callsite assertion. The
  tracing assertion passes in isolation.

No full-suite green result is claimed. Generated shell-fixture artifacts were
moved, without deletion, into `.tmp/slice0-test-artifacts-20260904/`.

The final local debug executable exists at
`desktop/src-tauri/target/debug/buzz-desktop.exe` (120,039,936 bytes; modified
2026-09-04 21:18:12 UTC). Its SHA-256 is
`DC6D12309C26B935262F4A8A052566AA1C84617CBACD677F253AC6BAF6C04ED3`.
It was not launched. All five adjacent Windows sidecars are nonempty and their
SHA-256 hashes match the verified root debug build. Previous staging files were preserved in
`.tmp/slice0-desktop-sidecars-backup-20260904/`. The Windows platform configuration
intentionally excludes `buzz-backend-kubernetes`.

The subsequent Tauri debug build uses `.tmp/slice0-debug-build.config.json` to skip
only the already completed frontend command, reusing verified production assets.
It enables Tauri's embedded-asset build path without creating an installer or
launching the app. This is a local debug artifact, not a release installer or a
live application smoke-test result.

## Remaining gates

The PRD's five live relay/harness spike tests have not been executed. Slice 0
cannot be fully closed, and Slice 1 estimates remain uncommitted, until those
results are recorded.

Automatic approval review rejected automatic startup history pruning and the
legacy subscription reset because their destructive scope lacked explicit user
authorization. Neither was implemented. Approval must specifically cover deleting
old/excess observer history and resetting ambiguous legacy kind-24200 consent
across the local archive's identities and relays; Graphify is outside that scope.

No commit was created. Staged ACP/relay changes are preserved; their binary diff
hash is `242962403954f3faa2a04c7d1e3f00bf1c694819`. Graphify artifacts were not changed.
