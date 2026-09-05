NIP-AO
======

Agent Observability
-------------------

`draft` `optional`

This NIP defines an ephemeral, encrypted event kind for streaming readable agent
activity and carrying structured operator controls between AI agent processes and
their owners' desktop clients via Nostr relays. It also freezes the durable claim
and pause-lease state-machine seams required to make an at-least-once ephemeral
transport safe.

## Motivation

AI agent harnesses execute long-running sessions that invoke tools, send protocol
frames to models, and emit intermediate reasoning. Owners need real-time visibility
into this activity for debugging, auditing, and control — without that telemetry
being stored on any relay or visible to third parties.

Kind 24200 provides a dedicated, encrypted, ephemeral channel for this purpose.
It is strictly scoped to the agent↔owner relationship. Events are never stored by
the relay. Implementations nevertheless persist small control metadata locally:
spent command ids, terminal acknowledgements, and the current pause lease. That
metadata contains no prompt, steer-message body, raw ACP frame, or tool output.

## Definitions

- **Agent**: An AI process with its own Nostr keypair, executing a session on behalf of an owner.
- **Owner**: The human (or system) whose pubkey the agent was provisioned under.
- **Observer Frame**: A single kind 24200 event carrying one unit of telemetry or control.
- **Session**: A bounded agent execution correlated by a shared `sessionId`.
- **One-shot command**: Cancel or steer, identified by a `command_id`; it can
  execute at most once and never creates a lease.
- **Pause lease**: The only renewable control state. It holds new queue work at
  safe boundaries while allowing the current model call to finish.
- **Resume**: An idempotent state transition that releases a named pause lease;
  it is not a second lease and does not inject bytes into ACP stdin.
- **Acknowledgement**: An agent-signed telemetry frame bound to the exact command
  or lease-transition fingerprint.

## Event Kinds

| Kind  | Name                  | Direction         |
|-------|-----------------------|-------------------|
| 24200 | Agent Observer Frame  | agent↔owner (both)|

Kind 24200 falls in the ephemeral range (20000–29999) defined by NIP-01. Relays
MUST NOT persist it.

## Event Structure

```json
{
  "kind": 24200,
  "pubkey": "<sender_pubkey>",
  "created_at": <unix_timestamp>,
  "content": "<NIP-44 v2 ciphertext>",
  "tags": [
    ["p",     "<recipient_pubkey>"],
    ["agent", "<agent_pubkey>"],
    ["frame", "telemetry" | "control"]
  ]
}
```

Events MUST have exactly one `p` tag, exactly one `agent` tag, and exactly one
`frame` tag.

**Telemetry** (agent → owner): `pubkey`=agent, `p`=owner, `agent`=agent.
**Control** (owner → agent): `pubkey`=owner, `p`=agent, `agent`=agent (target).

`frame` MUST be `"telemetry"` or `"control"`. Relays SHOULD silently drop events
with unrecognized `frame` values (returning OK to the publisher for forward
compatibility). Clients MUST ignore events with unrecognized `frame` values. An `h` tag MAY be included when the session runs within a NIP-29 group
context.

For the structured controls and acknowledgements defined below, every routing
tag has exactly two elements and the only allowed tags are one each of `p`,
`agent`, and `frame`, plus at most one `h`. If present, `h` equals the encrypted
target channel. Duplicate, malformed, or additional tags fail closed at the
consumer even when a relay already accepted the event.

## Encryption

All `content` fields MUST be encrypted with NIP-44 v2 (XChaCha20-Poly1305 over a
secp256k1 ECDH shared secret).

- **Telemetry**: encrypted with `(agent_privkey, owner_pubkey)`
- **Control**: encrypted with `(owner_privkey, agent_pubkey)`

Plaintext SHOULD be zeroized from memory immediately after encrypt/decrypt.
Decrypted payload MUST NOT exceed 65,535 bytes.

## Decrypted Payload

### Telemetry (`frame=telemetry`)

The `content` field decrypts to an `ObserverEvent` JSON object:

```json
{
  "seq":         <monotonic_integer>,
  "timestamp":   "<rfc3339_string>",
  "kind":        "<frame_kind>",
  "agentIndex":  <integer> | null,
  "channelId":   "<channel_uuid>" | null,
  "sessionId":   "<session_id>" | null,
  "turnId":      "<turn_id>" | null,
  "payload":     { ... }
}
```

`seq`, `timestamp`, `kind`, and `payload` are REQUIRED. `agentIndex`, `channelId`, `sessionId`,
and `turnId` are OPTIONAL — they MAY be `null` when the value is not yet known
(e.g., `sessionId` before session establishment). Clients MUST handle `null` values
gracefully.

`seq` is monotonically increasing per session (drop detection). `timestamp` is an
RFC 3339 datetime string with sub-second precision (e.g., `"2026-04-29T12:00:41.500Z"`).
`agentIndex` identifies the agent in multi-agent scenarios. `sessionId`/`turnId`
correlate frames across a session and turn. `payload` is kind-specific (MAY be `{}`).
Unknown `kind` values MUST be ignored.

### Frame Kinds

| `kind`             | Description                                              |
|--------------------|----------------------------------------------------------|
| `acp_read`         | Inbound ACP protocol frame (model → harness)             |
| `acp_write`        | Outbound ACP protocol frame (harness → model)            |
| `turn_started`     | A new agent turn has begun                               |
| `session_resolved` | Session completed or terminated                          |

### Readable output excerpts

A Live Activity readability projection MUST NOT render an unbounded raw ACP
frame. Text projected as an output excerpt uses this closed object:

```json
{
  "text": "<redacted UTF-8, at most 4096 bytes>",
  "truncated": true
}
```

The limit is measured in UTF-8 bytes, not characters. NUL and non-layout control
characters are invalid; tab, CR, and LF are allowed. Producers SHOULD truncate
and set `truncated=true` before encryption. Consumers MUST reject an oversized
projection rather than silently allocate or render it. “Show more” MAY load from
the owner's bounded local archive when explicitly enabled; it never fetches raw
activity from relay history. Redaction is defense in depth, not the confidentiality
boundary—NIP-44 plus owner-scoped routing remains that boundary.

### Control (`frame=control`)

The original experimental `{"type":"cancel_turn","channelId":"..."}` shape
is a legacy payload. Existing feature-off behavior MAY continue to consume it,
but it is not a Slice-0 structured command: it has no replay key, run binding,
deadline, or acknowledgement. Producers implementing structured controls MUST
use one of the versioned closed schemas below. Unknown or duplicate JSON members
at any object level are invalid; they are not extension points.

All structured-control plaintext is capped at 16,384 UTF-8 bytes before JSON
allocation. Opaque `computer_id` and `run_id` values are 1–128 bytes and contain
only ASCII alphanumerics plus `- _ . : /`. Public keys and event ids are lowercase
64-character hex. UUIDs MUST be non-nil. Sequence values start at one and are
monotonic in their direction; they are diagnostics for gaps and ordering, not a
substitute for durable replay claims.

### Cancel and steer: one-shot command

```json
{
  "format": "buzz-agent-control-command",
  "version": 1,
  "command_id": "<uuid>",
  "control": "cancel" | "steer",
  "operator_pubkey": "<owner hex pubkey>",
  "target": {
    "computer_id": "<opaque host id>",
    "agent_pubkey": "<agent hex pubkey>",
    "channel_id": "<channel uuid>",
    "run_id": "<opaque active-run id>"
  },
  "seq": 7,
  "issued_at": 1800000000,
  "expires_at": 1800000060,
  "steer_message_event_id": "<durable operator-message event id; steer only>"
}
```

`expires_at` MUST be later than `issued_at` and at most 300 seconds later. A
consumer treats `now >= expires_at` as expired and rechecks that rule inside the
durable claim transaction. `event.created_at` MUST equal `issued_at`.

Cancel MUST omit `steer_message_event_id`. Steer MUST include it, and the agent
MUST resolve that event from the current server-resolved tenant before claiming
the command. The referenced event MUST already be durable, be authored by the
same operator, be in the exact channel, and have `created_at <= issued_at`.
Steering therefore carries no prompt text: the durable operator message is the
intent, while native ACP steer and cancel-and-merge are delivery optimizations.

The runtime matches every payload binding against trusted current facts:
server-resolved tenant, current agent owner plus its monotonic ownership/visibility
revision, computer, agent, channel, and run. Every ownership or visibility
transition MUST advance that revision, including an A -> B -> A transition.
It also verifies the outer kind-24200 event id/signature, author, timestamp,
ciphertext envelope, and exact routing tags. A valid optional `h` tag MUST equal
`target.channel_id`. The payload `operator_pubkey` never establishes identity;
it merely must match the verified event signer and current owner.

#### Frozen command fingerprint and spent-command claim

The consumer computes SHA-256 over this exact byte preimage:

```text
"buzz-agent-control/command/v1\0"
|| community_uuid[16]
|| command_uuid[16]
|| control_u8                         # cancel=1, steer=2
|| lp(operator_pubkey)
|| lp(computer_id) || lp(agent_pubkey) || channel_uuid[16] || lp(run_id)
|| seq_u64_be || issued_at_u64_be || expires_at_u64_be
|| steer_present_u8
|| (lp(steer_message_event_id) when present)
```

`lp(s)` is `len(s)` as an unsigned 64-bit big-endian integer followed by UTF-8
bytes. `community_uuid` is supplied only by host resolution; it is absent from
wire JSON. The lowercase hex digest is the immutable command fingerprint.

Before any ACP or queue effect, one transaction MUST:

1. recheck its own current time against `expires_at`;
2. lock and re-resolve the agent owner row, requiring both owner and monotonic
   revision to match the validated claim;
3. claim `(community_id, command_id)` with the full fingerprint, bindings, and
   ownership revision;
4. enqueue an outbox row carrying that revision; and
5. commit all of them or none.

The outbox consumer MUST re-resolve the same owner/revision immediately before
the ACP or queue effect. A mismatch cancels the row as `authority_conflict`;
it never executes under authority that changed after validation. The comparison
MUST reject a higher revision even when the owner pubkey is again identical,
covering an A -> B -> A ownership transition.

A fresh claim executes its enqueued row once. An exact duplicate that is already
pending waits for the original acknowledgement. An exact duplicate with a
terminal stored acknowledgement returns that acknowledgement. It never executes
again. Reuse of the id with any different fingerprint/binding is
`command_replay`; store failure is `store_unavailable`. Spent rows and terminal
acknowledgements MUST remain at least through the signed deadline plus the
deployment's maximum accepted transport-skew window.

The Slice-0 spent-command outcome and disposition classifiers in `buzz-core`
are crate-private reference models used by the executable conformance matrix.
They are not a durable adapter and cannot prove that a transaction ran. The
control runtime slice MUST co-locate a sealed store adapter with the actual
claim, command, and outbox rows, and only that adapter may mint a private permit
that the effect consumer accepts.

### Command acknowledgement

An agent returns this payload in an agent→owner `frame=telemetry` event:

```json
{
  "format": "buzz-agent-control-ack",
  "version": 1,
  "ack_id": "<uuid>",
  "command_id": "<uuid>",
  "command_fingerprint": "<lowercase sha256>",
  "control": "cancel" | "steer",
  "operator_pubkey": "<owner hex pubkey>",
  "target": {
    "computer_id": "computer-1",
    "agent_pubkey": "<agent hex pubkey>",
    "channel_id": "<channel uuid>",
    "run_id": "run-1"
  },
  "command_seq": 7,
  "seq": 42,
  "acked_at": 1800000001,
  "status": "applied" | "no_active_turn" | "queued" | "rejected",
  "reason": "binding_mismatch" | "unsupported" | "internal_error",
  "detail": {"text": "<redacted, at most 512 UTF-8 bytes>", "truncated": false}
}
```

`reason` is required exactly for `rejected`; `detail` is optional display-only
text and never changes the outcome. Cancel cannot report `queued`; steer cannot
report `no_active_turn` because its durable message enters the normal queue when
no turn is active. `acked_at` MUST be within `[issued_at, expires_at)` and equal
the acknowledgement event's timestamp. Every command id, fingerprint, control,
target, operator, and original sequence field MUST exactly match the validated
command. The outer acknowledgement MUST be signed by the target agent and routed
to that operator. An acknowledgement alone is not proof of a claim; the terminal
ack and spent-command row are committed together.

### Pause lease and resume release

```json
{
  "format": "buzz-agent-pause-lease",
  "version": 1,
  "transition_id": "<uuid>",
  "lease_id": "<uuid stable across transitions>",
  "generation": 1,
  "transition": "pause" | "renew" | "resume",
  "operator_pubkey": "<owner hex pubkey>",
  "target": {
    "computer_id": "computer-1",
    "agent_pubkey": "<agent hex pubkey>",
    "channel_id": "<channel uuid>",
    "run_id": "run-1"
  },
  "seq": 8,
  "issued_at": 1800000000,
  "transition_expires_at": 1800000060,
  "lease_expires_at": 1800000300
}
```

`transition_expires_at` is the delivery deadline and follows the same 300-second
maximum as a one-shot command. It is deliberately distinct from
`lease_expires_at`. Pause uses generation 1 and requires `lease_expires_at`.
Renew increments the current durable generation by exactly one, requires an
active unexpired matching lease, and moves the lease deadline strictly forward.
Resume also increments by exactly one and MUST omit `lease_expires_at`; it
releases the named lease. Pause/renew durations are at most 3,600 seconds; the
product default is 300 seconds.

The queue-hold effect is agent-wide on `(community_id, agent_pubkey, computer_id)`;
otherwise a busy agent could continue dispatching from another channel. The
lease identity is narrower: its durable row stores the complete original
`ControlTarget`, and every renew/resume MUST echo the same computer, agent,
channel, and run. A channel/run shift is a lease conflict, not a new scope. The
UI uses the stored target when releasing a hold whose original run is no longer
active; expiry remains the fail-safe release path. Once that row is inactive or
expired, a fresh `pause` with a new lease id MAY atomically replace it using a
new channel/run. It remains bound to the same tenant, agent, and computer queue
scope. The current owner and its non-zero monotonic ownership/visibility
revision MUST instead match the fresh pause; an inactive row retained from an
earlier owner/revision neither grants that earlier authority nor blocks the
current owner. Retained history therefore prevents replay without pinning future
work to an obsolete run or owner. The current durable compare-and-swap row
persists owner and its non-zero revision alongside lease id, generation, active
state, deadline, last transition id, and the frozen transition fingerprint. That
fingerprint uses the command construction above with domain
`buzz-agent-control/pause-lease/v1\0`, both UUIDs, generation, transition byte
(`pause=1`, `renew=2`, `resume=3`), target/operator/sequence/timestamps, and an
optional-deadline marker.

The CAS MUST lock and re-resolve the target's owner row, require both owner and
revision to match the validated claim, and recheck the transition deadline and,
for pause/renew, the lease deadline using its own clock. In the same transaction
it claims `(community_id, transition_id)` with the complete tenant-bound
fingerprint and ownership revision, changes current lease state, and writes the
matching audit/outbox record carrying that revision. A mismatch during this
transition transaction fails as `authority_conflict` without changing state.
The transition claim is a separate durable tombstone retained through the signed
transition deadline plus the deployment's maximum accepted transport-skew
window; replacing the current lease row must never erase it. An exact historical
retry therefore changes no current state, even after an intervening lease, while
reuse of a transition id with different bindings is a conflict. A stored
historical acknowledgement may be replayed for correlation, but its old
`queue_state` must not overwrite the current durable queue projection.

Replaying a pause after expiry cannot resurrect the old lease. Stale/skipped
generations and binding conflicts fail closed. Store classification is
authoritative: when concurrent identical deliveries both read the pre-transition
row, the losing CAS reports the durable exact transition claim and is
duplicate-suppressed even though its earlier read did not yet identify a retry.
By contrast, a store result of `applied` conflicts with a token whose earlier
read already proved the transition applied. Store failure never changes queue
state.

The Slice-0 pause outcome and disposition classifiers are likewise
crate-private reference models, not evidence of a durable lease write. The
control runtime slice MUST co-locate its sealed lease adapter and private effect
permit with the transaction that commits the transition-id tombstone, current
lease row, and audit/outbox row.

On startup and before every dispatch, the queue reads the durable lease and the
current agent owner/revision together. An active lease whose persisted owner or
revision differs from current authority produces
`authority_changed_must_release`, never `hold_queue`. Integration MUST then use
one CAS transaction to mark that lease inactive/running and emit a
`pause_lease_authority_released` audit record containing both persisted and
current authority revisions. A revision-only mismatch with the same owner pubkey
is sufficient, so an A -> B -> A transition cannot preserve an old hold. It MUST
re-evaluate durable state before dispatch.
A new/current owner MAY submit a fresh pause with a new lease id only after that
release commit; it cannot replace the still-active stale-authority row directly.
Transition-id tombstones and the distinct lease id continue to suppress replay.
An active row with `now >= lease_expires_at` likewise MUST atomically become
running and emit `pause_lease_expired`; it must never remain held because a timer
was lost. Simultaneous resume, expiry, and authority-release attempts converge
through the same current-row CAS.

Pause and renew hold only *new* queue dispatch at the next safe boundary. They do
not freeze a model call, suspend a process, or write to ACP stdin.

### Pause-lease acknowledgement

The agent returns `buzz-agent-pause-lease-ack` in a signed telemetry frame. It
echoes `transition_id`, transition fingerprint, lease id, generation, transition,
operator, complete target, and transition sequence; adds `ack_id`, its own `seq`,
and `acked_at`; and reports:

```json
{
  "status": "applied" | "already_applied" | "rejected",
  "queue_state": "paused" | "running",
  "detail": {"text": "<redacted, at most 512 UTF-8 bytes>", "truncated": false}
}
```

A successful pause/renew reports `paused`; a successful resume reports `running`.
The acknowledgement timestamp is within the transition delivery window. The
full Rust wire shape is frozen in `crates/buzz-core/src/agent_control.rs` and the
55-case executable conformance matrix is `NIP-AO.fixtures.json`.

## Ephemerality Contract

- Relays MUST NOT persist kind 24200 events to any durable storage.
- Relays MUST NOT include kind 24200 events in search indexes.
- Relays MUST NOT include raw kind 24200 ciphertext or decrypted bodies in audit
  logs. A structured-control consumer MUST separately record bounded audit
  metadata (`control_issued`, terminal ack/expiry, and pause lease lifecycle).
- Relays SHOULD fan out kind 24200 events only via in-memory pub/sub,
  never via a database write path.
- Clients SHOULD subscribe with `since=<now>`; historical replay is not supported.
- Clients SHOULD buffer received events in a bounded in-memory ring buffer.

## Authorization

**Telemetry** (agent → owner):
- `event.pubkey` MUST equal the agent pubkey.
- `p` tag MUST equal the owner pubkey.
- Relay MUST verify `is_agent_owner(agent, owner)` via authenticated ownership lookup.

**Control** (owner → agent):
- `event.pubkey` MUST equal the owner pubkey.
- `p` tag MUST equal the agent pubkey.
- Relay MUST verify `is_agent_owner(agent, owner)` where agent is resolved from the
  `agent` tag.
- After decryption, the agent MUST repeat the owner check against current runtime
  facts, capture the monotonic owner/visibility revision, and match the complete
  structured target. The claim/CAS and effect consumer MUST recheck that same
  revision. Relay authorization does not replace consumer-side command
  validation or durable replay claims.

Both directions require relay confirmation of the agent-owner relationship via
database lookup. `#p` tag matching alone is insufficient. Unauthorized publish or
subscribe attempts MUST be rejected with `AUTH required`.

## Relay Behavior

On receiving a kind 24200 event, a relay MUST:

1. Validate the event signature per NIP-01.
2. Verify authorization per the rules above.
3. Fan out to matching subscribers via in-memory pub/sub.
4. NOT invoke the normal event ingestion or persistence path.

Relays SHOULD enforce a rate limit of 100 telemetry events/second per agent
pubkey. Owner-to-agent control MUST have its own bounded abuse controls but MUST
not be starved behind bursty telemetry.
Relays are RECOMMENDED to reject events whose `created_at` falls outside a ±5-minute
freshness window to prevent replay of captured events.

## Client Behavior

Clients subscribe with:

```json
{"kinds": [24200], "#p": ["<own_pubkey>"], "since": <now>}
```

On receiving an event, a client MUST:

1. Verify the event signature.
2. Decrypt `content` using own secret key and `event.pubkey`.
3. Parse the decrypted payload and dispatch on `kind` (legacy telemetry) or the
   exact structured-control `format`.
4. Ignore unknown telemetry `kind` values. Reject an unknown structured-control
   format/version and every malformed, unknown-field, or duplicate-field payload.
5. For a structured control, resolve current owner/target facts and durable state,
   run the frozen validator, then perform its required transaction before effects.

Clients SHOULD verify that the `agent` tag matches a known/trusted agent pubkey
before decrypting.

Clients SHOULD buffer events in a bounded ring buffer (RECOMMENDED maximum: 800 events).
Clients MUST NOT request historical kind 24200 events (no `since` in the past, no
`until`, no `ids` queries).

## Security Considerations

**Metadata leakage.** Routing tags (`p`, `agent`, `frame`, `created_at`) are
cleartext. A relay operator can observe that agent X is streaming to owner Y at what
rate. For maximum metadata privacy, implementors MAY wrap events in NIP-59 gift wrap.

**No forward secrecy.** NIP-44 does not provide forward secrecy; compromise of the
agent's private key allows decryption of any captured ciphertext.

**Replay attacks.** Relay timestamp freshness alone is insufficient: a captured
event can be replayed inside the allowed window or after a consumer restart.
Structured commands therefore use a tenant-scoped atomic `command_id` claim;
pause mutations use lease id, generation, transition id, and a durable CAS. The
transaction checks its own clock. Exact retries reuse the original result and
never repeat effects; conflicting reuse fails closed.

**Rogue relays.** The ephemerality contract is relay policy, not cryptography.
NIP-44 encryption ensures stored events remain opaque to the relay operator absent
key compromise.

**Best-effort delivery.** Kind 24200 remains at-least-once/best-effort transport:
frames can be dropped or duplicated during reconnect. The UI may retry an
unacknowledged command only with byte-identical bindings and the same id. A retry
never weakens expiry and never executes an already-spent id again. Operators see
`unacknowledged/expired` rather than a false success when no terminal ack arrives.

**Confused-deputy payload pairing.** A consumer MUST validate the plaintext
obtained from the same signed event it is authorizing. It must not accept a parsed
payload and unrelated signed envelope as independently supplied arguments. The
reference API's verify→decrypt→parse→bind entry point enforces that ordering.

**Lease liveness.** Pause is persistent state, so a process timer is not the
source of truth. Restart recovery and every pre-dispatch check read the durable
deadline plus current owner/revision. An expired or stale-authority hold is
released and audited via CAS, preventing an agent from remaining silently paused
after a crash or ownership/visibility change.

**Operational persistence vectors.** Telemetry may transiently exist in process
memory, crash dumps, and application logs. Implementations SHOULD minimize logging
of decrypted payloads and MUST NOT log it at INFO level or above.

## Relationship to Other NIPs

- **NIP-01**: Kind 24200 is in the ephemeral range (20000–29999); standard event
  structure and signature rules apply.
- **NIP-42**: Recommended for relay-side authentication gating.
- **NIP-44**: Required encryption algorithm for all `content` fields.
- **NIP-29**: An `h` tag MAY be included when the agent session is scoped to a
  NIP-29 group.
- **NIP-XX (PR #2226)**: NIP-XX defines the agent *output* plane; this NIP defines
  the *observability* plane (internal agent activity). They are complementary and
  non-overlapping.

## Examples

### 1. Telemetry Event — `acp_write` frame

**Wire event (encrypted):**

```json
{
  "id":         "a1b2c3d4...",
  "kind":       24200,
  "pubkey":     "agent_pubkey_hex",
  "created_at": 1777464041,
  "content":    "<NIP-44 v2 ciphertext>",
  "tags": [
    ["p",     "owner_pubkey_hex"],
    ["agent", "agent_pubkey_hex"],
    ["frame", "telemetry"]
  ],
  "sig": "..."
}
```

**Decrypted payload:**

```json
{
  "seq":        42,
  "timestamp":  "2026-04-29T12:00:41.500Z",
  "kind":       "acp_write",
  "agentIndex": 0,
  "channelId":  "52a85618-0f8f-4542-94ec-599e6e1c6f2e",
  "sessionId":  "a1b2c3d4",
  "turnId":     "e5f6g7h8",
  "payload": {
    "jsonrpc": "2.0",
    "method":  "tools/call",
    "params":  { "name": "shell", "arguments": { "command": "ls -la" } }
  }
}
```

---

### 2. Structured cancel command

**Wire event (encrypted):**

```json
{
  "id":         "e5f6a7b8...",
  "kind":       24200,
  "pubkey":     "owner_pubkey_hex",
  "created_at": 1777464042,
  "content":    "<NIP-44 v2 ciphertext>",
  "tags": [
    ["p",     "agent_pubkey_hex"],
    ["agent", "agent_pubkey_hex"],
    ["frame", "control"]
  ],
  "sig": "..."
}
```

**Decrypted payload:**

```json
{
  "format": "buzz-agent-control-command",
  "version": 1,
  "command_id": "0ae54a89-480f-47bd-bb09-5c87f716f9e2",
  "control": "cancel",
  "operator_pubkey": "<owner_pubkey_hex>",
  "target": {
    "computer_id": "computer-1",
    "agent_pubkey": "<agent_pubkey_hex>",
    "channel_id": "52a85618-0f8f-4542-94ec-599e6e1c6f2e",
    "run_id": "run-1"
  },
  "seq": 7,
  "issued_at": 1777464042,
  "expires_at": 1777464102
}
```

## Reference Implementation

- Existing ephemeral routing: [block/sprout PR #421](https://github.com/block/sprout/pull/421)
- Frozen zero-I/O contract: `crates/buzz-core/src/agent_control.rs`
- Executable conformance matrix: `docs/nips/NIP-AO.fixtures.json`
