# Agent-Computer Promotion Checklist

**Purpose:** Verify that the four Agent-Computer feature flags are safe to promote to live production and safe to pull back via rollback.

**Promotion order:** flags must be enabled in this sequence:

1. `BUZZ_LIVE_ACTIVITY` (Slice 1)
2. `BUZZ_AGENT_CONTROLS` (Slice 2)
3. `BUZZ_ROUTINES` (Slice 3)
4. `BUZZ_DELEGATION` (Slice 4)

Rollback reverses this order: delegation off → routines off → controls off → live activity off.

## Per-Flag Preconditions

### Flag 1: BUZZ_LIVE_ACTIVITY

**Slice gate PASS commit:** `8abc8886a` (Slice 1 exit gate, per SLICE-1-VERIFICATION.md)

**Prerequisites:**
- All Slice 1 acceptance criteria passed
- Relay environment unchanged (NIP-11 `self` subscription gate active for kind 24200)
- Migration head at 45 or later

**How to enable:**
- Set relay env `BUZZ_LIVE_ACTIVITY=1` (if required; Slice 1 is desktop-only gated)
- Set desktop flag `BUZZ_LIVE_ACTIVITY` to true in settings
- Restart relay and desktop

**How to disable:**
- Set desktop flag `BUZZ_LIVE_ACTIVITY` to false
- Optionally unset relay env (Slice 1 has no relay gate)
- Restart desktop

---

### Flag 2: BUZZ_AGENT_CONTROLS

**Slice gate PASS commit:** `e1c321f95` (Slice 2 exit gate, per SLICE-2-VERIFICATION.md)

**Prerequisites:**
- Slice 1 (BUZZ_LIVE_ACTIVITY) is enabled and stable
- All Slice 2 acceptance criteria passed
- Relay environment unchanged (NIP-11 `self` subscription gate active for kind 24200)
- Migration head at 45 or later

**How to enable:**
- Set desktop flag `BUZZ_AGENT_CONTROLS` to true in settings
- Restart desktop

**How to disable:**
- Set desktop flag `BUZZ_AGENT_CONTROLS` to false
- Restart desktop

---

### Flag 3: BUZZ_ROUTINES

**Slice gate PASS commit:** `879c558a0` (Slice 3 exit gate, per SLICE-3-VERIFICATION.md)

**Prerequisites:**
- Slice 2 (BUZZ_AGENT_CONTROLS) is enabled and stable
- All Slice 3 acceptance criteria passed
- Relay environment `BUZZ_WORKFLOW_INVOKE_AGENT=1` must be set
- Migration head at 45 or later

**How to enable:**
- Set relay env `BUZZ_WORKFLOW_INVOKE_AGENT=1`
- Set desktop flag `BUZZ_ROUTINES` to true in settings
- Restart relay and desktop

**How to disable:**
- Set desktop flag `BUZZ_ROUTINES` to false
- Optionally unset relay env `BUZZ_WORKFLOW_INVOKE_AGENT` (setting off prevents any routine dispatch)
- Restart relay and desktop

---

### Flag 4: BUZZ_DELEGATION

**Slice gate PASS commit:** `bf70a3ee0` (Slice 4 exit gate, per SLICE-4-VERIFICATION.md — this is also the pin this Slice 5 branch is based on)

**Prerequisites:**
- Slice 3 (BUZZ_ROUTINES) is enabled and stable
- All Slice 4 acceptance criteria passed
- Relay environment `BUZZ_DELEGATION=1` must be set
- Desktop flag `BUZZ_DELEGATION` must be true (feature gate at relay + desktop)
- Migration head at 45 or later
- Relay NIP-11 `self` endpoint active (all Slices require owner-scoped authentication)

**How to enable:**
- Set relay env `BUZZ_DELEGATION=1`
- Set desktop flag `BUZZ_DELEGATION` to true in settings
- Restart relay and desktop

**How to disable:**
- Set desktop flag `BUZZ_DELEGATION` to false
- Optionally unset relay env `BUZZ_DELEGATION` (setting off prevents any delegation dispatch)
- Restart relay and desktop

---

## Rollback Rehearsal

See `docs/nips/slice5-runbook.md` for the complete reverse-order rollback sequence, with exact step-by-step commands, per-step verification checks, and evidence file names.

---

## Known Limits

The following constraints are recorded as accepted by design and do not block promotion:

- **no display-layer redaction for live activity exists; §6.3 defence-in-depth is pending a separate packet**
- **§5 emit→paint p95 ≤ 2 s product gate: not closed**
- **control latency is log-derived (`control_ack` audit rows), no metric exporter**
- **hop-2 delegation not run live; covered by `delegation_nested_hop_and_turns` and code review**

From the PRD (v0.2e):
- **no PTY takeover in this release** — raw terminal access is explicitly out of scope; Slice 1 renders readable activity, Slice 2 delivers structured controls (cancel/steer/pause), not keystrokes; Phase-2 terminal work is gated on a separate PTY probe
- **pause holds the queue and does not freeze a turn** — the in-flight model call completes at its boundary; only new work dispatch is held at the next safe queue point
- **delegation transfers work not authority** — the target agent runs under its own existing authority and approval gates; a `DelegationExecutionContext` carries provenance and constraints only
- **archive pruning and legacy consent reset pending operator decision** — automatic age/count/byte pruning and resetting ambiguous legacy kind-24200 subscriptions require explicit operator authorization; existing history is preserved and new-save limits prevent further growth without deletion

---

## Promotion Decision

**Promotion decision:** _____ (operator to complete)

**Approved by:** (signature, date)

