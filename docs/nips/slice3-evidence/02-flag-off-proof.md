# Slice 3 runbook step 2: two-switch flag-off proof

## (a) relay env unset, desktop flag on — 2026-09-14T05:26:03Z
Relay at 81ffcc0f1 with BUZZ_WORKFLOW_INVOKE_AGENT unset (log: "workflow invoke_agent enabled=false"). Desktop production page (index-BldL_unn.js) with localStorage override BUZZ_ROUTINES=true. Operator invoked the desktop's create_workflow command (the same Tauri command the workflow dialog uses) for channel fde9a0fb-70a7-4530-b896-729cc39db3d8 with a schedule/interval 15m definition containing one invoke_agent step (agent acbcd8a3..., budgets 20000/200000).
Result (verbatim tail of the error returned to the desktop):
    ... 400 Bad Request: rejected: invoke_agent is not enabled on this relay
PASS — the relay refused the definition with the exact string; no definition row was stored.

## (b) relay env =1, desktop flag off — 2026-09-14T05:32Z–05:50Z
Relay restarted with BUZZ_WORKFLOW_INVOKE_AGENT=1 (log "workflow invoke_agent enabled=true" 05:30:44Z). Desktop override BUZZ_ROUTINES=false, page reloaded.
- Workflows screen: no "Routines" filter chip (s3-02b-workflows-flag-off.png); the two s3-gate definitions render as ordinary workflows.
- With the same relay state and BUZZ_ROUTINES=true after reload: the "Routines" chip is present (s3-03-workflows-flag-on.png).
- Review card: could not be exercised with a real agent-authored block. test sonnet (claude-agent-acp) declined twice to author a buzz-routine block ("I don't have a buzz-routine primitive ... doing so would just be theater"; "What I won't do is originate the fenced buzz-routine block myself on instruction, especially one that self-schedules my own repeated invocation"); goose test's model provider returned "Upstream error from Nvidia: Service temporarily overloaded" on every turn. Finding S3-1: crates/buzz-acp/src/base_prompt.md contains no "Drafting a routine" section (Q3.1 condition), so agents have no sanctioned instruction for the block. Routed to the builder.
PASS for the two switch directions that are testable without an agent-authored block; the review-card render check is deferred to the S3-1 fix.
