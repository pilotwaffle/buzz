# Slice 4 (Delegation) — Operator-Attended Live Runbook

**Status: WRITTEN, NOT EXECUTED.** This document is the exact procedure an operator runs to
close the live-gate items of `build_spec.md`'s Step 8: the two-switch flag-off proof, a real
end-to-end delegation, hop-2 continuation, hop-3 refusal, the per-run/daily/turn-ceiling and
cost-cap breach scenarios, replay/enumeration-safety, pause/cancel interplay, the AC-22-style
privacy grep (I-14), and the fire-to-inject latency measurement. No step here has been run this
session. The builder session that wrote this runbook was explicitly instructed not to touch the
permanent relay, the live database, or running sidecars — every step below requires operator
hands and operator authorization.

**Before starting:** confirm `torq/slice4-delegation` is at the commit this runbook was written
against (`f704ace09` or later on the same branch), and that Steps 1–7 (contract through the
verification doc) are merged into the branch you are about to build the relay from.

Record every timestamp, log excerpt, and screenshot named below into
`docs/nips/slice4-evidence/`, one file per numbered step (matching the naming already scaffolded
in `docs/nips/slice4-evidence/README.md`, e.g. `01-relay-bringup.log`, `04-end-to-end-a-b.md`).
If any step's expected observation does not match, stop, do not proceed to the next step, and
record the mismatch instead of working around it.

---

## 1. Relay + sidecar bring-up

1. `cargo build --release --bin buzz-relay` on `torq/slice4-delegation`.
2. Back up the running binary: copy `E:\TORQ-BUZZ\bin\buzz-relay.exe` →
   `bin\buzz-relay.exe.bak-<date>`.
3. Copy the newly built binary into `bin\buzz-relay.exe`.
4. Rebuild the sidecar (`buzz-acp`) from the same branch and restart the managed agents under
   test through the normal sidecar bring-up path.
5. Migrate the live database to head 46 **after a `pg_dump` backup** (the migration is additive
   only — three new tables, no changes to existing ones — but back up first per SOC-1/SOC-2
   regardless).
6. Set **`BUZZ_DELEGATION=1`** and **`RUST_LOG=buzz_relay=info,buzz_relay::delegation=info,buzz_workflow=info`**
   in `E:\TORQ-BUZZ\config\relay.env` (mirrors Slice 3's `RUST_LOG` lesson — delegation's own
   `tracing::info!` lines live under the `buzz_relay::delegation` target, distinct from the bare
   `buzz_relay` target, and will be filtered out without the explicit target).
7. **Restart only through the scheduled task `TORQ-Buzz-PermanentRelay`** — never
   `Start-Process` the relay directly from an agent or operator shell session.
8. Confirm:
   - NIP-11 shows a non-empty `self` field.
   - Migration head is `0046` (`buzz-admin migration-status` or equivalent).
   - The relay startup log contains a `delegation_enabled=true` (or equivalent) startup line.
   - `\d delegation_records`, `\d delegation_claims`, `\d delegation_actions` on the live DB match
     `SLICE-4-VERIFICATION.md` §3's migration text exactly (already confirmed against the scratch
     DB in that document; this step re-confirms against the live DB specifically).
9. Record: binary commit hash, the relay's `self` pubkey, and the migration head.

**Rollback (N5):** set `BUZZ_DELEGATION` to unset (or any value other than `1`) in
`relay.env`, restart the scheduled task, then set the desktop `BUZZ_DELEGATION` flag off. Nothing
is deleted — the new tables and any rows they hold are simply no longer reachable through any
code path. Reverse the binary copy (`bak` → `buzz-relay.exe`) only if the new binary itself is
suspect, not merely to disable the feature.

## 2. Two-switch flag-off proof

Prove both switches independently gate the feature before turning either on:

- **(a) Relay env unset, desktop flag on:** with the desktop `BUZZ_DELEGATION` flag enabled via
  Experiments (requires flipping `preview-features.json` locally for this test only, or a build
  with the flag force-enabled — do not commit that change), attempt to approve a drafted
  delegation. Expect the relay to refuse with `restricted: unknown event kind` (the same
  fall-through every other unrecognized kind gets — delegation is not special-cased when the
  switch is off).
- **(b) Relay env `=1`, desktop flag off:** with the relay switch on but the desktop flag off,
  post a `buzz-delegation` fenced code block in a channel. Expect it to render as a plain JSON
  code block — no "Review delegation" button, no card, no `approve_delegation` IPC call at all
  (checked via DevTools network/IPC log).
- **(c) `GET /delegations/tenant` 404-equality, flag off:** with the relay switch off, compare:
  ```sh
  curl -s -o /dev/null -w "%{http_code}\n" https://<relay-host>/delegations/tenant
  curl -s -o /dev/null -w "%{http_code}\n" https://<relay-host>/delegations/nonexistent-route-xyz
  ```
  Both status codes must be identical (mirrors `delegation_tenant_route_flag_off_matches_unknown_route`'s
  assertion exactly — status-code equality, not a specific hardcoded value, since axum's own
  unmatched-fallback status is an implementation detail this check does not pin). With the switch
  **on**, repeat the first `curl` — its status must now differ from the unknown-route status
  (proving the route is structurally conditional, not just uniformly refusing).

Record all three proofs (screenshot or DevTools/curl log excerpt) before proceeding.

## 3. Fresh channel and agent setup

1. Create a fresh private channel with a unique per-run token in its name or topic (so evidence
   from this run cannot be confused with a prior gate's).
2. Bring up three managed agents on the Claude harness in that channel: A (source), B (first-hop
   target), C (second-hop target). Confirm each has its own operator-approved identity and that
   A and B are both owned by the same operator (required for a direct delegation), and C is owned
   by the same operator too (required for the hop-2 continuation in step 5).
3. Enable `BUZZ_DELEGATION` in the desktop Experiments panel for the operator's own desktop
   session.

## 4. End-to-end delegation, A → B

1. Ask A (in the shared channel) to delegate a small, verifiable task to B. Expect A to post one
   fenced `buzz-delegation` code block per `base_prompt.md`'s "Drafting a delegation" section: a
   fresh `delegation_id`, `origin_event_id` omitted, `agent_path: [A, B]`, `hop_budget: 1`, a
   small `max_turns`, a `token_budget`, and `expires_at` within an hour.
2. As the operator, click "Review delegation" on A's block. Confirm the dialog shows A/B by name
   (via `useManagedAgentsQuery`), the turns/token-budget/expiry/hop values from the block, and (if
   you set a `cost_cap_microusd` for this test) the exact warning text
   `cost cap set: this delegation cannot run in this release`.
3. Click "Approve". Confirm:
   - `approve_delegation` returns an event id (visible in the desktop's own success toast/log).
   - A `DelegationSummaryCard` appears in the channel, collapsed, chip `Approved` transitioning to
     `Running`.
   - The relay's `delegation_records` row is `approved`; `delegation_claims` has one row;
     `delegation_actions` has action 1 with a `wake_event_id` set.
   - B receives a wake (relay-signed kind-9) in the origin channel, threaded under the origin
     message; the wake's tag list matches `test-fixtures/delegation-wake-tags.json`'s
     `wakeTagNames` exactly (same order, same set).
4. Confirm B's turn context (`<context>` block, spec 4.3) correctly states the reply destination
   is the origin thread, states the token budget remaining, and that this is a delegated task.
5. Let B answer in the origin thread (its own reply is an ordinary message — not modified by this
   slice).
6. Confirm B's sidecar posts exactly one outcome event (kind-9, agent-signed, threaded under the
   **origin** event — not the wake), outcome word `delivered`, with `buzz:delegation-tokens=<n>`.
7. Confirm the `DelegationSummaryCard`'s chip transitions to `Delivered` with the tokens-used
   figure shown, and that clicking "Show notices" reveals the wake and the summary notice as raw
   entries.
8. Confirm `delegation_records.state = 'delivered'`, `token_budget_remaining` decremented by the
   outcome's token count, `answer_event_id` set.

## 5. Hop-2 continuation, A → B → C

1. Ask B (instead of answering directly) to delegate the same task onward to C: same-shape
   `buzz-delegation` block, `agent_path: [A, B, C]`, `parent_approval_event_id` set from B's own
   `<context>` block's `operator_approval_event_id`, `hop_budget: 2`, `token_budget` ≤ B's
   remaining budget.
2. Operator approves C's block the same way as step 4.2–4.3.
3. Confirm B's turn ends with the literal last line `delegation-outcome: delegated` in its reply
   (per `base_prompt.md`), and B's sidecar-posted outcome word is `delegated` (not `delivered`) —
   this is the `delegated_outcome_from_last_line` detection working live.
4. Confirm the parent record (A→B) stays open (not yet a terminal state) while the child (B→C) is
   in flight, and that C's wake carries `buzz:delegation-child-answer` unset (this is the child's
   own first wake, not the parent's continuation).
5. Let C answer; confirm C's sidecar posts `delivered` for the B→C delegation.
6. Confirm the **parent** (A→B) then receives its own continuation wake carrying
   `buzz:delegation-child-answer=<C's outcome event id>`, with content using the continuation
   phrasing ("the sub-delegation you requested has completed..."), and that B's *second* turn
   (this continuation) ends with either `delivered` (B relays C's answer itself) or another
   `delegated` (a third hop — not expected in this test, hop budget 2 already reached).
7. Confirm the A→B record reaches `delivered` once B's continuation turn completes, with
   `answer_event_id` set to B's final reply.

## 6. Hop-3 refusal

1. Have C attempt to delegate onward to a fourth agent D (or reuse A to create a 4-element
   `agent_path`). Expect the relay to refuse the approval with `blocked: delegation refused`
   (hop-budget/path-length violation, I-8) — no wake, no new `delegation_records` row.

## 7. Turn-ceiling refusal

1. Set up a fresh direct delegation A→B with `max_turns: 1`.
2. After B's first turn delivers, have B (or the operator, via a second approval reusing the same
   `delegation_id` if the harness allows it) attempt a second action. Expect `turn_limit_exceeded`
   → the record becomes `failed`, exactly one failure notice reads `did not complete: turns`.

## 8. Cost-cap refusal

1. Draft a delegation with `cost_cap_microusd` set to any non-null value.
2. Approve it. Expect every action CAS to refuse `cost_unknown` (I-10) — the record becomes
   `failed`, one failure notice reads `did not complete: cost_unknown`. No wake is ever dispatched
   for this delegation.
3. Confirm the desktop review dialog showed the exact warning text
   `cost cap set: this delegation cannot run in this release` before the operator approved it in
   step 1 — the operator was warned in advance, not surprised by the refusal.

## 9. Token budget exhaustion

1. Set `token_budget` low enough that one real turn's usage will exceed it (e.g. 200).
2. Let the delegation dispatch and B answer normally.
3. Confirm the sidecar's `finalize_delegation_turn` detects `turn_tokens > budget_remaining` and
   posts outcome `budget_exceeded` rather than `delivered`; confirm the record settles `failed`
   with detail `budget`.

## 10. Replay and conflicting-reuse refusal

1. Replay the identical 43007 approval event (same signature, same bytes) a second time. Expect:
   accepted (idempotent), but no second wake and no new `delegation_records`/`delegation_claims`
   row.
2. Sign a **second** 43007 for the same `delegation_id` but a different `idempotency_key`. Expect
   `blocked: delegation refused`, and confirm the relay log records an `approval_replay` audit
   line (I-5).

## 11. Enumeration-safety probe

1. Approve a delegation whose `target_agent` is a pubkey that does not exist as any agent on the
   relay. Capture the full `OK` reply byte-for-byte.
2. Approve a delegation whose `target_agent` is a real agent, but owned by a **different**
   operator (use a second operator key, or an intentionally cross-owner test agent). Capture the
   full `OK` reply byte-for-byte.
3. Confirm the two captured replies are **byte-identical** (`blocked: delegation target
   unavailable` in both cases, per I-13) — an attacker probing for valid target pubkeys must not
   be able to distinguish "doesn't exist" from "exists but isn't yours to target."

## 12. Pause-hold and cancel interplay

1. With a Slice 2 pause lease active on B's channel, trigger a delegation wake to B. Confirm the
   wake is held (`HoldQueue`) rather than dispatched immediately, and dispatches once the pause
   lease resumes.
2. Send a structured cancel against a delegation-started turn on B. Confirm it acks `applied`
   through the unchanged Slice 2 path, and that the sidecar posts exactly one outcome (`failed`,
   detail `cancelled`) for that turn — never a second one.

   **Note:** the two named unit tests for this interplay (spec 4.5:
   `delegation_prompt_is_held_under_active_pause_lease_and_dispatched_after_resume` and
   `structured_cancel_on_delegation_started_turn_acks_applied`) were not written in this slice's
   Steps 1–7 (see `SLICE-4-VERIFICATION.md` §8). This live check is therefore the *only*
   verification of this interplay claim until those tests exist — treat a failure here as a
   blocker, not a nice-to-have.

## 13. Relay store scan (privacy audit, I-14)

Adapt Slice 3's `docs/nips/slice3-evidence/s3-db.sh` pattern to the three delegation tables:

```sh
#!/usr/bin/env bash
# Slice 4 gate: dump delegation-related rows from the live relay DB (read-only).
q() { docker exec torq-buzz-postgres-1 psql -U buzz -d buzz -Atc "$1"; }
echo "== now: $(date -u +%FT%TZ)"
echo "== delegation_records"; q "select delegation_id, run_id, state, failure_detail, remaining_turns, token_budget_remaining, encode(source_agent,'hex'), encode(target_agent,'hex') from delegation_records order by created_at desc limit 10"
echo "== delegation_claims"; q "select delegation_id, encode(approval_event_id,'hex'), expires_at from delegation_claims order by created_at desc limit 10"
echo "== delegation_actions"; q "select delegation_id, action_seq, outcome, tokens_used, dispatched_at, settled_at from delegation_actions order by created_at desc limit 10"
```

Run this and grep both its output and the relay's own stdout log for the same sentinel set Slice
3's `ac22-grep.txt` used (adapted): a distinctive substring from this test's actual origin message
content, `totalTokens`, `inputTokens`, `outputTokens`, `cachedReadTokens`, `"cost"`, `cost_usd` —
expect **zero** matches anywhere except the explicitly-permitted `tokens=<n>` integer on
`delivered`/`budget_exceeded` outcome lines and outcome tags (I-14 names this exception
explicitly). Record the full grep output (matches and non-matches both) in
`13-store-scan.md`/`ac22-grep-slice4.txt`, mirroring Slice 3's format.

## 14. Fire-to-inject latency

1. Collect at least 10 delegation dispatches (reuse the A→B/A→B→C runs from steps 4–5, plus extra
   throwaway direct delegations if 10 isn't reached from the functional tests alone).
2. For each dispatch, pair the relay's `delegation wake dispatched` DEBUG line (or the
   `delegation_actions.dispatched_at` timestamp) with the sidecar's own reception — adapt Slice
   3's `s3-latency.py`, replacing the `routine prompt received run_id=` regex with a
   `buzz:delegation-run` equivalent line from the sidecar's `delegation prompt received` log
   (added in `crates/buzz-acp/src/pool.rs`'s `run_prompt_task`).
3. Compute the fire-to-inject latency for each pair; report the nearest-rank p95 across all
   dispatches.
4. Run this against a production page build with no debugger attached (per Slice 1/3's finding
   that an attached debugger and React's DEV profiler both distort timing by seconds).
5. Target: p95 ≤ 60 s (same bar as Slice 3's routine dispatch). Record the actual p95 and the raw
   pairs regardless of whether the target is met.

## 15. Stripped-context probe

1. Hand-craft a kind-9 event, signed by the **relay's own operator key** (not a forged key —
   this tests the admission gate's context-shape check, not its signer check, which steps 10–11
   and the unit tests already cover), carrying a `buzz:delegation-run` tag and a `buzz:delegation`
   tag but a **stripped or malformed** `buzz:delegation-context` tag (e.g. truncated JSON, or a
   `target_agent` naming a different agent than the one receiving it).
2. Publish it directly to the relay (bypassing the normal claim/dispatch path — this simulates
   what a compromised or buggy relay-side emitter might produce).
3. Confirm the sidecar's admission gate logs `delegation_context_denied` with a reason word and
   drops the event before it reaches `queue.push` — no turn starts, no reply is generated.

---

## Evidence checklist

Every item below must have a corresponding file or excerpt in `docs/nips/slice4-evidence/`
before this runbook is considered closed:

- [ ] Relay + sidecar bring-up: binary commit, `self` pubkey, migration head 46, live `\d`
      output confirmed matching, rollback path confirmed
- [ ] Two-switch flag-off proof (all three: relay-off/desktop-on, relay-on/desktop-off,
      tenant-route 404-equality both directions)
- [ ] End-to-end A→B: block → card → approve → summary → wake (exact tag order) → B's answer →
      outcome (`delivered`) → card chip
- [ ] Hop-2 A→B→C: `delegated` outcome from B, continuation wake with `buzz:delegation-child-answer`,
      final `delivered` on the parent
- [ ] Hop-3 refused (`blocked: delegation refused`)
- [ ] Turn-ceiling refusal (`failed(turns)`, one notice)
- [ ] Cost-cap refusal (`cost_unknown`, one notice, desktop warning shown before approval)
- [ ] Token budget exhaustion (`budget_exceeded`, `failed(budget)`)
- [ ] Replay (no second wake) and conflicting-idempotency-key reuse refused (`approval_replay`
      audited)
- [ ] Enumeration probe: nonexistent-target and foreign-owned-target replies byte-identical
- [ ] Pause-hold and structured-cancel interplay (flag this as the *only* verification until the
      spec-4.5 unit tests exist)
- [ ] Relay store scan + grep audit: zero prompt/reply/cost-field leakage anywhere except the
      permitted `tokens=<n>` integer
- [ ] Fire-to-inject p95 ≤ 60 s over ≥10 dispatches, raw data attached
- [ ] Stripped-context probe: `delegation_context_denied` logged, no turn started

**Residual, not run this gate:** the goose managed-agent harness. Slice 3's live gate found goose
unavailable that day (provider outage); this slice's delegation wire path (relay wake, sidecar
tag parse, admission gate, outcome post) is agent-agnostic and is covered for goose by the unit
tests in `SLICE-4-VERIFICATION.md` §6. Re-run steps 4–12 on goose once a working model is
configured; no code change is implied by skipping it this gate.
