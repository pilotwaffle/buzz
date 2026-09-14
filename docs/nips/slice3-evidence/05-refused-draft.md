# Slice 3 runbook step 5: refused non-owner definition, disabled routine idle

## 5.1 non-owner signer refused at ingest (2026-09-14T05:53Z)
Tool: slice3-evidence/s3-refuse.mjs (nostr-tools; throwaway key generated locally, secret never recorded; signer pubkey 3cda97f6d28be11bfcd6df90d7aabe32db6e62b04f9e4c627a4294031eb1e1c1). It authenticates (NIP-42) as the throwaway key and publishes a kind 30620 definition with one invoke_agent step targeting test sonnet (acbcd8a3...) into channel fde9a0fb-70a7-4530-b896-729cc39db3d8. The agent's own key is never available outside its sidecar, so the test uses a key that is likewise not the agent's owner; the relay guard is "signer must be the target agent's registered owner", which this exercises directly.
- As a non-member: OK false, "forbidden: not a member of this channel" (05:53:22Z).
- After add_channel_members for the throwaway key, as a member but not the owner: OK false, "forbidden: invoke_agent routines must be signed by the target agent's owner" (05:5xZ). Exact spec string.
- `select count(*) from workflows where name='s3-gate-refused-agent-signed'` = 0 after both attempts. No routine_dispatches row can exist for it (no workflow row); re-checked across the following two due windows in 05-refused-draft-windows.log.

## 5.3 saved-but-disabled routine
Definition s3-gate-claude-disabled (8f12a796-6979-47b3-ad94-45084497322d) saved 05:50:25Z with YAML `enabled: false` (definition JSON enabled=false; the workflows.enabled column is TRUE because the store's upsert always inserts TRUE and the column is toggled separately; the scheduler additionally checks the definition's own enabled flag at crates/buzz-workflow/src/lib.rs:766 before firing, so this is not a defect). Expected: no routine_dispatches row across two due windows; recorded in 05-refused-draft-windows.log.
