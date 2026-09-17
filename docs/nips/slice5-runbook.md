# Slice 5 (Close-out) — Operator-Attended Live Runbook

**Status: WRITTEN, NOT EXECUTED.** This document is the exact procedure an operator runs to
close the live-gate items of `build_spec.md`'s Step 7: the reverse/forward flag rollback rehearsal,
the seeded-secret relay store scan, the R5-1 emit→paint latency re-measurement with p50/p95
recording, the webview working-set aging sample over 1 hour, and the optional hop-2 delegation
live verification. No step here has been run this session. The builder session that wrote this
runbook was explicitly instructed not to touch the permanent relay, the live database, or running
sidecars — every step below requires operator hands and operator authorization.

**Before starting:** confirm `torq/slice5-closeout` is at the commit this runbook was written
against (`bf70a3ee0` or later on the same branch), and that all of Steps 1–6 (flags audit,
regression, FR-5 coverage, residual tests, dead-code cleanup, and documentation) are merged into
the branch you are about to build the relay from.

Record every timestamp, log excerpt, and screenshot named below into `docs/nips/slice5-evidence/`,
one file per numbered step (matching the naming already scaffolded in
`docs/nips/slice5-evidence/README.md`, e.g. `01-bringup.log`, `02-rollback-reverse.md`). If any
step's expected observation does not match, stop, do not proceed to the next step, and record the
mismatch instead of working around it.

---

## 1. Bring-up from torq/slice5-closeout

1. `cargo build --release --bin buzz-relay` on `torq/slice5-closeout`.
2. Back up the running binary: copy `E:\TORQ-BUZZ\bin\buzz-relay.exe` →
   `bin\buzz-relay.exe.bak-<date>`.
3. Copy the newly built binary into `bin\buzz-relay.exe`.
4. Rebuild the sidecar (`buzz-acp`) from the same branch and restart the managed agents under
   test through the normal sidecar bring-up path.
5. Ensure the live database is at migration head 46 (migrate if not; `pg_dump` backup first per
   SOC-1/SOC-2).
6. Set **`BUZZ_DELEGATION=1`** and **`BUZZ_WORKFLOW_INVOKE_AGENT=1`** in
   `E:\TORQ-BUZZ\config\relay.env`.
7. Set **`RUST_LOG=buzz_relay=info,buzz_relay::delegation=info,buzz_workflow=info`** in
   `E:\TORQ-BUZZ\config\relay.env` (delegation and routine targets must be explicitly named, not
   covered by the bare `buzz_relay` target).
8. **Restart only through the scheduled task `TORQ-Buzz-PermanentRelay`**:
   ```powershell
   schtasks /run /tn TORQ-Buzz-PermanentRelay
   ```
   Never start the relay directly from an agent or operator shell session.
9. Confirm:
   - NIP-11 shows a non-empty `self` field.
   - Migration head is `0046` (`buzz-admin migration-status` or equivalent).
   - The relay startup log contains lines indicating `delegation_enabled=true` and
     `workflow_invoke_agent_enabled=true` (or equivalent startup records).
   - `\d delegation_records`, `\d delegation_claims`, `\d delegation_actions` on the live DB match
     the schema exactly (already confirmed against the scratch DB; this re-confirms against live).
   - `\d routine_dispatches`, `\d routine_state`, `\d workflows` exist and are populated.
10. Record: binary commit hash, the relay's `self` pubkey, migration head, and a `\d` schema dump
    for each table.

**Rollback (N3):** set `BUZZ_DELEGATION` and `BUZZ_WORKFLOW_INVOKE_AGENT` to unset (or any value
other than `1`) in `relay.env`, restart the scheduled task, then turn off the desktop
`BUZZ_DELEGATION` and `BUZZ_ROUTINES` flags. Nothing is deleted. Reverse the binary copy
(`bak` → `buzz-relay.exe`) only if the new binary itself is suspect.

---

## 2. Reverse rollback [N3]

Prove the flags can be disabled and the system rolls back safely. Start with all four desktop
flags ON (`BUZZ_LIVE_ACTIVITY`, `BUZZ_AGENT_CONTROLS`, `BUZZ_ROUTINES`, `BUZZ_DELEGATION`). Then
execute **in this exact order**:

### Step 2(a): Desktop `BUZZ_DELEGATION` off, relay `BUZZ_DELEGATION` env unset, restart

1. Turn `BUZZ_DELEGATION` off in the desktop Experiments panel.
2. Unset `BUZZ_DELEGATION` in `E:\TORQ-BUZZ\config\relay.env`.
3. Restart via `schtasks /run /tn TORQ-Buzz-PermanentRelay`.
4. After restart, confirm:
   - App starts and runs normally.
   - One plain chat round-trip completes (ask a simple question and receive a reply).
   - Row counts are unchanged:
     ```sql
     SELECT count(*) FROM delegation_records;
     SELECT count(*) FROM delegation_claims;
     SELECT count(*) FROM delegation_actions;
     ```
   - Routine and workflow counts are unchanged:
     ```sql
     SELECT count(*) FROM routine_dispatches;
     SELECT count(*) FROM routine_state;
     SELECT count(*) FROM workflows WHERE definition::text LIKE '%invoke_agent%';
     ```
   - Sidecar control-store row count is unchanged:
     ```bash
     sqlite3 "E:\TORQ-BUZZ\<agent-pk16>\control-<pk16>.sqlite" "SELECT count(*) FROM spent_command;"
     sqlite3 "E:\TORQ-BUZZ\<agent-pk16>\control-<pk16>.sqlite" "SELECT count(*) FROM pause_lease_current;"
     sqlite3 "E:\TORQ-BUZZ\<agent-pk16>\control-<pk16>.sqlite" "SELECT count(*) FROM pause_lease_transition;"
     sqlite3 "E:\TORQ-BUZZ\<agent-pk16>\control-<pk16>.sqlite" "SELECT count(*) FROM control_audit;"
     sqlite3 "E:\TORQ-BUZZ\<agent-pk16>\control-<pk16>.sqlite" "SELECT count(*) FROM routine_daily_usage;"
     ```
     (Note: `routine_daily_usage` is Slice 3's table; it may exist and is part of the control
     store. The sidecar control-store uses SQLite; find the exact path of the control file for
     each managed agent under test.)
   - Relay log contains no lines matching `delegation_*` or `routine *` after the switch.
   - Settings › Experiments shows `BUZZ_DELEGATION` toggle is off.

### Step 2(b): Desktop `BUZZ_ROUTINES` off, relay `BUZZ_WORKFLOW_INVOKE_AGENT` env unset, restart

1. Turn `BUZZ_ROUTINES` off in the desktop Experiments panel.
2. Unset `BUZZ_WORKFLOW_INVOKE_AGENT` in `E:\TORQ-BUZZ\config\relay.env`.
3. Restart via `schtasks /run /tn TORQ-Buzz-PermanentRelay`.
4. Repeat the checks from Step 2(a) (app runs, round-trip works, row counts unchanged, sidecar
   counts unchanged, log shows no routine lines, Experiments shows toggle off).

### Step 2(c): Desktop `BUZZ_AGENT_CONTROLS` off

1. Turn `BUZZ_AGENT_CONTROLS` off in the desktop Experiments panel.
2. Confirm app runs, one plain chat round-trip, Experiments shows toggle off.

### Step 2(d): Desktop `BUZZ_LIVE_ACTIVITY` off

1. Turn `BUZZ_LIVE_ACTIVITY` off in the desktop Experiments panel.
2. Confirm app runs, one plain chat round-trip, Experiments shows toggle off.

Record all observations (screenshots of toggles, relay log excerpts, SQL count outputs) into
`02-rollback-reverse.md` as a timestamped sequence with expected vs. actual outcomes.

---

## 3. Forward re-enable [N3]

Exact reverse order of Step 2, with the same checks **plus** the additional observations:

### Step 3(a): Desktop `BUZZ_LIVE_ACTIVITY` on

1. Turn `BUZZ_LIVE_ACTIVITY` on in the desktop Experiments panel.
2. Repeat the checks from Step 2(a).

### Step 3(b): Desktop `BUZZ_AGENT_CONTROLS` on

1. Turn `BUZZ_AGENT_CONTROLS` on in the desktop Experiments panel.
2. Repeat the checks from Step 2(a).

### Step 3(c): Desktop `BUZZ_ROUTINES` on, relay `BUZZ_WORKFLOW_INVOKE_AGENT=1`, restart

1. Turn `BUZZ_ROUTINES` on in the desktop Experiments panel.
2. Set `BUZZ_WORKFLOW_INVOKE_AGENT=1` in `E:\TORQ-BUZZ\config\relay.env`.
3. Restart via `schtasks /run /tn TORQ-Buzz-PermanentRelay`.
4. Repeat the checks from Step 2(a).
5. **NEW CHECK:** confirm a routine fires on its next scheduled instant (create a test routine
   with a due time 2 minutes in the future, wait for it to fire, and record the fire timestamp).

### Step 3(d): Desktop `BUZZ_DELEGATION` on, relay `BUZZ_DELEGATION=1`, restart

1. Turn `BUZZ_DELEGATION` on in the desktop Experiments panel.
2. Set `BUZZ_DELEGATION=1` in `E:\TORQ-BUZZ\config\relay.env`.
3. Restart via `schtasks /run /tn TORQ-Buzz-PermanentRelay`.
4. Repeat the checks from Step 2(a).
5. **NEW CHECK:** confirm a delegation approval, claim, and dispatch succeed (ask an agent to
   delegate a small task to another agent, approve it through the UI, and confirm the wake is
   delivered and the task executes).

Record all observations into `03-rollback-forward.md`, mirroring the structure of
`02-rollback-reverse.md`.

---

## 4. Seeded-secret relay store scan [Q2]

With all four flags on and both relay envs set to `1`:

1. Prompt the Claude agent to print exactly: `SENTINEL-<random-8-hex-chars>` (e.g.
   `SENTINEL-a7f2c1e9`). Record the exact sentinel value you use.
2. Run the relay store scan, adapting the Slice 3 / Slice 4 pattern to the current tables:
   ```sh
   #!/usr/bin/env bash
   # Slice 5 gate: dump delegation/routine-related rows from the live relay DB (read-only).
   q() { docker exec torq-buzz-postgres-1 psql -U buzz -d buzz -Atc "$1"; }
   echo "== now: $(date -u +%FT%TZ)"
   echo "== delegation_records"; q "select delegation_id, run_id, state, failure_detail, encode(source_agent,'hex'), encode(target_agent,'hex') from delegation_records order by created_at desc limit 20"
   echo "== delegation_claims"; q "select delegation_id, encode(approval_event_id,'hex'), expires_at from delegation_claims order by created_at desc limit 20"
   echo "== delegation_actions"; q "select delegation_id, action_seq, outcome, tokens_used, dispatched_at, settled_at from delegation_actions order by created_at desc limit 20"
   echo "== routine_dispatches"; q "select routine_id, routine_name, created_at from routine_dispatches order by created_at desc limit 20"
   echo "== routine_state"; q "select routine_id, state, current_turn, total_tokens_used from routine_state order by updated_at desc limit 20"
   echo "== events (kind 24200 only)"; q "select created_at, encode(id,'hex'), encode(author,'hex') from events where kind=24200 order by created_at desc limit 20"
   ```
3. Save the script output to a file.
4. Scan the relay's own `stdout` log file for any occurrence of the sentinel string.
5. Grep both the SQL output and the relay log for the sentinel. Expect **zero** matches outside
   the agent's own kind-9 message (where the agent replied and may naturally repeat the request).
6. Expect the UI to show the sentinel unredacted in the conversation timeline (this is the
   accepted known gap — see `docs/nips/residuals/UI-redaction.md`).

Record the sentinel value, the full script output, and the grep results (showing zero findings or
listing where the sentinel appears) into `04-seeded-secret.md`.

---

## 5. R5-1 measurement [Q1.1]

Production desktop build, all flags on, measuring the emit→signed→websocket callback→paint latency
breakdown. Re-measure against both Claude and goose managed agents, n ≥ 10 runs each.

1. Use the tooling from `docs/nips/slice1-evidence/` (if those tools exist and are suitable for
   re-measurement; adapt if necessary):
   - `s1-p95.py`: computes p50/p95 from raw latency samples.
   - `s1-split.py`: stages the breakdown (emit, signed, websocket callback, paint).
2. For each stage (emit→signed, signed→callback, callback→paint), collect ≥ 10 measurements from
   Claude agent runs and ≥ 10 from goose agent runs.
3. Compute p50 and p95 for each stage and each agent.
4. **No threshold is asserted** for this slice (per I-6 and Q1.2 condition). This is measurement
   only. The `residuals/R5-1-R5-2-observer-latency.md` will carry the data and the decision point
   for whether a future slice should enforce a p95 ≤ 2 s rule.
5. Record the raw latency samples, the p50/p95 computed per stage, and a summary table showing:
   | Stage | Claude p50 (ms) | Claude p95 (ms) | Goose p50 (ms) | Goose p95 (ms) |
   |---|---|---|---|---|
   | emit→signed | ... | ... | ... | ... |
   | signed→callback | ... | ... | ... | ... |
   | callback→paint | ... | ... | ... | ... |

Record all data into `05-latency.md`.

---

## 6. Webview aging [N5]

Leave the timeline view open (a chat or a channel showing live delegations/routines) for
1 hour. Every 5 minutes, record the working set (memory usage) of the desktop process via
PowerShell:

```powershell
Get-Process | Where-Object { $_.ProcessName -like "*desktop*" } | Select-Object ProcessName, Id, WorkingSet
```

(Adjust the process name filter if the desktop runs under a different process name.)

Create a table (timestamp, working set MB) with 12 rows (one per 5-minute interval). Record whether
memory usage remained stable, grew linearly, or exhibited spikes. Record the table and observations
into `06-webview-aging.md`.

---

## 7. Hop-2 delegation live (optional) [Q4.4]

This step is optional. Only run if the operator provisions a third managed agent (agent C) to
complete a real delegation hop-2 scenario live.

If running:
1. Set up three agents A, B, C in the same channel (managed agents on the Claude harness, all
   owned by the same operator).
2. Ask A to delegate a small task to B with `agent_path: [A, B, C]` and `hop_budget: 2`.
3. Approve the A→B delegation through the UI.
4. When B receives the wake, ask B to delegate onward to C (B creates a `buzz-delegation` block
   with `parent_approval_event_id` from B's context, `agent_path: [A, B, C]`).
5. Approve the B→C delegation through the UI.
6. Confirm:
   - B's outcome for the A→B delegation is `delegated` (detected from "delegation-outcome: delegated" in B's reply).
   - C receives a wake and executes the task.
   - C's sidecar posts outcome `delivered` for the B→C delegation.
   - A→B record reaches `delivered` once B's continuation turn completes.
7. Record observations (screenshots, task flow, outcome audit lines) into `07-hop2.md`.

If **not** running this step:
1. Record "`07-hop2-not-run.md`" with a one-line explanation: "Hop-2 delegation is covered by
   automated test `delegation_nested_hop_and_turns` in `crates/buzz-relay/src/delegation/tests.rs`;
   no live operator run scheduled this gate."

---

## 8. Evidence checklist

Every item below must have a corresponding file in `docs/nips/slice5-evidence/` before this runbook
is considered closed:

- [ ] `01-bringup.log` — Binary commit, relay `self` pubkey, migration head 46, schema dumps for
      all delegation/routine tables
- [ ] `02-rollback-reverse.md` — Four flag-off steps in sequence, with row count and log checks
      at each step
- [ ] `03-rollback-forward.md` — Four flag-on steps in sequence, with row count and log checks at
      each step, plus routine fire and delegation approve observations
- [ ] `04-seeded-secret.md` — Sentinel value, SQL scan output, relay log grep results (zero findings)
- [ ] `05-latency.md` — p50/p95 per stage and per agent, raw samples, summary table (measurement only,
      no threshold asserted)
- [ ] `06-webview-aging.md` — 1-hour working-set sample table (12 rows), stability observations
- [ ] `07-hop2.md` or `07-hop2-not-run.md` — Live hop-2 observations or skip explanation
