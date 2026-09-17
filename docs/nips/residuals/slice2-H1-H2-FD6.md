# Slice 2 residual items: H1, H2, F-D6

## Symptom

Three items from Slice 2 (structured controls) were identified during operator-attended gate testing and explicitly deferred from Slice 5 as out-of-scope ([Q4.4] "recorded only"):

### H1: Goose cancel-drain respawn

**Definition:** When a structured cancel is sent to a Goose managed agent, the cancel signal is applied and acknowledged correctly (~100 ms). However, the goose worker process does not drain/shut down within the sidecar's `cancel_with_cleanup` grace period (~4 seconds). The sidecar then treats this as a timeout and spawns a fresh goose worker. The next turn starts a new goose session.

**Technical detail:** The sidecar's structured-control ack and cancel-signal path are correct. The issue is in the upstream goose harness (v1.45.0) — it does not complete graceful shutdown within the timeout, likely a goose-specific behavior difference from claude-agent-acp.

**Evidence pointer:** SLICE-2-VERIFICATION.md §15 H1; `slice2-evidence/` gate logs showing "agent_returned - respawning (cancel-drain timeout)" after every cancel, followed by worker respawn and isNewSession=true on the next turn; goose harness v1.45.0 used in gate.

### H2: Claude native steer -32603 error

**Definition:** When a structured steer is sent to Claude (using native steer delivery), the injected mid-turn user message is followed within ~100 ms by an internal error `-32603 [ede_diagnostic] result_type=user last_content_type=n/a stop_reason=null`. The turn ends and the sidecar invalidates the session. Observed 8 of 8 times on the fresh operator-gate channel.

**Technical detail:** The structured-control path (ack, ring recording, delivery) is correct. The issue is in the upstream claude-agent-acp harness (v0.64.2) — native steer injects content mid-turn in a way that violates a session-state invariant inside claude-agent-acp, producing the -32603 error.

**Evidence pointer:** SLICE-2-VERIFICATION.md §15 H2; `slice2-evidence/` gate logs showing -32603 on every native steer attempt; claude-agent-acp v0.64.2 used in gate. The sidecar already supports `cross_adapter_steering` (the "fallback" steer path) as an alternative delivery method.

### F-D6: Expired unknown-id pending steer acks as rejected

**Definition:** A steer command with an unknown id that is also expired should return status `rejected` with reason `binding_mismatch`. However, due to a CHECK constraint labeling error, the sidecar was inserting "pause_lease_granted" into a field that expected the literal value "pause" or other lease-transition labels, causing the INSERT to fail with a constraint violation rather than returning the correct `rejected` ack.

**Technical detail:** Root cause in `crates/buzz-acp/src/control_store.rs:551`: `audit_event.as_str()` was being injected directly into an audit INSERT, but the field had a CHECK constraint that only accepted specific literal values for pause-lease rows. The fix: compute `transition_label` BEFORE the INSERT and match it to the CHECK constraint, so the correct label is injected for pause rows and a safe fallback is used for non-pause rows (e.g., steer/cancel).

**Accepted disposition (SLICE-2-VERIFICATION §4 "Files Changed", G2A round 8 MINOR):** Fixed in this slice. Verification: all pause-lease tests pass; no more CHECK constraint violations.

**Evidence pointer:** SLICE-2-VERIFICATION.md §4 F-D6; `crates/buzz-acp/src/control_store.rs` insertion logic.

## Proposed Scope

A dedicated follow-up packet to:
1. **H1 (goose cancel-drain):** investigate goose v1.45.0 shutdown behavior; candidate fixes are (a) increase the `cancel_with_cleanup` grace timeout for goose agents (config change), or (b) treat the drain timeout as a soft signal rather than a hard respawn trigger. Requires upstream goose maintainer input.
2. **H2 (Claude native steer -32603):** upstream diagnosis of the -32603 error inside claude-agent-acp; candidate mitigation is to prefer `cross_adapter_steering` for Claude agents (a one-line dispatch-branch preference in the sidecar's steer-delivery logic, or a config field `native_steer_disabled_runtimes`).
3. **F-D6 (resolved):** Already closed in this slice; no residual work needed.

## Not Scheduled

H1 and H2 are upstream harness issues requiring investigation and coordination with the goose and claude-agent-acp maintainers. Deferred pending prioritization and upstream availability.
