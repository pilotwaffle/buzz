# UI redaction: Display-layer masking for live activity

## Symptom

No display-layer redaction exists for live activity or delegation content shown in the desktop UI. This is an explicit Non-Goal for this slice ([Q2]): "Any UI redaction layer, regex or otherwise" is out of scope.

A consequence is that a seeded secret (e.g., API key, bearer token) in an agent's message will appear unredacted to any viewer with UI access. By design, the relay-side guarantee (`crates/buzz-relay/src/delegation/tests.rs`'s `delegation_relay_store_scan_has_no_body` and `metrics_and_audit_lines_carry_no_bodies`) proves that the relay store and metrics/logs carry no message bodies. However, the UI layer has no corresponding masking.

The automated relay scan verifies that the *relay's* durable storage and emitted metrics contain no secrets. Paired with owner-scoped encryption (NIP-44) and encrypted desktop archive (when enabled), this provides defense-in-depth at the boundary of the relay. The UI gap is a display-layer concern, not a transport or storage concern.

## Evidence Pointer

- `crates/buzz-relay/src/delegation/tests.rs::delegation_relay_store_scan_has_no_body` — automated test proving relay store contains no sentinel string from agent replies or delegation task bodies
- `crates/buzz-relay/src/delegation/tests.rs::metrics_and_audit_lines_carry_no_bodies` — automated test proving relay metrics and audit logs contain no sensitive message content
- Operator runbook (`docs/nips/slice5-runbook.md`, step 4) documents the expected observation: "UI shows the sentinel unredacted" — this is the recorded baseline for the UI gap

## Proposed Scope

A dedicated packet to design and build a display-layer redaction/masking system for the desktop UI:

1. Define what constitutes sensitive-looking content (patterns: secrets, API keys, tokens, PII like email or SSN)
2. Choose a masking strategy: regex-based pattern matching, ML-based detection, or structured field masking (e.g., only redact certain fields in tool output)
3. Implement redaction at the timeline renderer or message-display component level
4. Ensure "show more" / unmasking still works for debugging when enabled by the operator
5. Test that masked content is still searchable (if applicable) or explicitly not searchable (simpler, more conservative)

This is defense-in-depth work, reinforcing (not replacing) the relay-side guarantee that raw message bodies never persist in relay storage or metrics.

## Not Scheduled

Out of scope for this slice. Deferred pending operator decision on UI masking priority and implementation approach.
