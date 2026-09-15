//! Delegation (kind 43007, `BUZZ_DELEGATION`) handling for buzz-acp.
//!
//! The relay posts a relay-signed kind:9 wake tagged `buzz:delegation-run`
//! (the run id), `buzz:delegation` (the delegation id), `buzz:delegation-context`
//! (a compact `DelegationExecutionContext` JSON), `buzz:delegation-budget`
//! (decimal token budget remaining), and, for a continuation wake,
//! `buzz:delegation-child-answer` — see `crates/buzz-relay/src/delegation/dispatch.rs`
//! for the wire producer. This module detects those wakes after they have
//! already passed the inbound author gate's dedicated delegation admission
//! check (`lib.rs`'s `mod inbound_author_gate`, spec 4.2), and builds the
//! outcome event that settles the dispatched action.

use buzz_core::delegation::DelegationExecutionContext;

/// Tag carrying the delegation run id (also present on the agent-signed outcome).
pub const TAG_DELEGATION_RUN: &str = "buzz:delegation-run";
/// Tag carrying the delegation id.
pub const TAG_DELEGATION: &str = "buzz:delegation";
/// Tag carrying the compact `DelegationExecutionContext` JSON.
pub const TAG_CONTEXT: &str = "buzz:delegation-context";
/// Tag carrying the decimal token budget remaining.
pub const TAG_BUDGET: &str = "buzz:delegation-budget";
/// Tag carrying the child delegation's agent-signed outcome event id, present
/// only on a continuation wake.
pub const TAG_CHILD_ANSWER: &str = "buzz:delegation-child-answer";
/// Outcome event tag for the delegation outcome word.
pub const TAG_OUTCOME: &str = "buzz:delegation-outcome";
/// Outcome event tag for the decimal tokens used this turn.
pub const TAG_TOKENS: &str = "buzz:delegation-tokens";

/// The agent delivered the answer directly; the delegation is done.
pub const OUTCOME_DELIVERED: &str = "delivered";
/// The agent delegated the task onward; the parent stays open awaiting the
/// child's own outcome.
pub const OUTCOME_DELEGATED: &str = "delegated";
/// The turn failed (cancelled, or any other non-`Ok` prompt outcome).
pub const OUTCOME_FAILED: &str = "failed";
/// The turn's token usage exceeded the remaining budget.
pub const OUTCOME_BUDGET_EXCEEDED: &str = "budget_exceeded";

/// Delegation binding parsed from a relay-signed wake's tags, after the
/// event has already passed the inbound author gate's delegation admission
/// check.
#[derive(Debug, Clone)]
pub struct DelegationBinding {
    /// The durable run id this delegation was claimed under.
    pub run_id: String,
    /// The delegation id.
    pub delegation_id: String,
    /// The execution context carried on the wake.
    pub context: DelegationExecutionContext,
    /// Decimal token budget remaining, from the wake's own budget tag (not
    /// `context.remaining_turns`, which is a turn count, not a token count).
    pub budget_remaining: u64,
    /// The child delegation's agent-signed outcome event id (hex), present
    /// only on a continuation wake.
    pub child_answer_event_id: Option<String>,
    /// The wake event's own id (hex).
    pub wake_event_id: String,
    /// Origin channel (UUID) the delegation was drafted in, from the wake's
    /// `h` tag.
    pub origin_channel: String,
    /// Encrypted originating message event id (hex), from
    /// `context.request.origin_event_id`.
    pub origin_event_id: String,
}

fn tag_value<'a>(event: &'a nostr::Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        if s.first().map(|f| f.as_str()) == Some(name) {
            s.get(1).map(|v| v.as_str())
        } else {
            None
        }
    })
}

/// Whether any `buzz:delegation-*` tag is present on `event`. Used to
/// distinguish "not a delegation event" ([`None`] from
/// [`parse_delegation_binding`]) from "a malformed delegation event" ([`Err`]).
fn has_any_delegation_tag(event: &nostr::Event) -> bool {
    event.tags.iter().any(|t| {
        t.as_slice()
            .first()
            .map(|f| f.as_str().starts_with("buzz:delegation"))
            .unwrap_or(false)
    })
}

/// Parse a delegation binding from an event that has already passed the
/// inbound author gate's delegation admission check.
///
/// `Ok(None)` when no `buzz:delegation-*` tag is present at all — not a
/// delegation event. `Err(reason)` when any such tag is present but the
/// wake is malformed: `run_id` is not a UUID, the context is missing or
/// fails to parse (`parse_context_json`), `context.request.target_agent`
/// does not match `own_pubkey_hex`, or the budget tag is not a decimal.
/// A malformed budget never falls back to unlimited.
pub fn parse_delegation_binding(
    event: &nostr::Event,
    own_pubkey_hex: &str,
) -> Result<Option<DelegationBinding>, &'static str> {
    if !has_any_delegation_tag(event) {
        return Ok(None);
    }
    let run_id = tag_value(event, TAG_DELEGATION_RUN).ok_or("missing delegation-run tag")?;
    uuid::Uuid::parse_str(run_id).map_err(|_| "delegation-run tag is not a UUID")?;
    let delegation_id = tag_value(event, TAG_DELEGATION).ok_or("missing delegation tag")?;

    let context_json =
        tag_value(event, TAG_CONTEXT).ok_or("missing delegation-context tag")?;
    let context = buzz_core::delegation::parse_context_json(context_json.as_bytes())
        .map_err(|_| "delegation-context tag failed to parse")?;
    if context.request.target_agent != own_pubkey_hex {
        return Err("delegation context target_agent does not match this agent");
    }

    let budget_str = tag_value(event, TAG_BUDGET).ok_or("missing delegation-budget tag")?;
    let budget_remaining: u64 = budget_str
        .parse()
        .map_err(|_| "delegation-budget tag is not a decimal")?;

    let origin_channel = tag_value(event, "h").ok_or("missing h tag")?.to_owned();
    let origin_event_id = context.request.origin_event_id.clone();
    let child_answer_event_id = tag_value(event, TAG_CHILD_ANSWER).map(str::to_owned);
    let wake_event_id = event.id.to_hex();

    Ok(Some(DelegationBinding {
        run_id: run_id.to_owned(),
        delegation_id: delegation_id.to_owned(),
        context,
        budget_remaining,
        child_answer_event_id,
        wake_event_id,
        origin_channel,
        origin_event_id,
    }))
}

/// Fixed outcome content strings (spec 4.4). Never the prompt, the reply, a
/// count beyond the run id, or a cost — `detail` is a fixed one-word failure
/// reason (`cancelled`), never free text.
pub fn outcome_content(run_id: &str, outcome: &str, detail: Option<&str>) -> String {
    match outcome {
        OUTCOME_DELIVERED => format!("delegation run {run_id} delivered"),
        OUTCOME_DELEGATED => format!("delegation run {run_id} delegated"),
        OUTCOME_BUDGET_EXCEEDED => format!("delegation run {run_id} exceeded its token budget"),
        _ => match detail {
            Some(d) => format!("delegation run {run_id} failed: {d}"),
            None => format!("delegation run {run_id} failed"),
        },
    }
}

/// Build a kind:9 outcome event to post back into the origin thread,
/// threaded under the **origin** event (root = origin, parent = wake) —
/// unlike a routine outcome, which threads under the wake itself. Signed by
/// the agent key. Tags carry exactly one `buzz:delegation-run`, one
/// `buzz:delegation-outcome`, and one `buzz:delegation-tokens` (decimal; the
/// tag set is frozen, no extra marker tag).
pub fn build_outcome_event(
    agent_keys: &nostr::Keys,
    binding: &DelegationBinding,
    outcome: &str,
    turn_tokens: u64,
    content: &str,
) -> Result<nostr::Event, String> {
    let origin_id = nostr::EventId::from_hex(&binding.origin_event_id)
        .map_err(|e| format!("origin_event_id: {e}"))?;
    let wake_id = nostr::EventId::from_hex(&binding.wake_event_id)
        .map_err(|e| format!("wake_event_id: {e}"))?;
    let channel_id: uuid::Uuid = binding
        .origin_channel
        .parse()
        .map_err(|e| format!("origin_channel: {e}"))?;
    let thread_ref = buzz_sdk::ThreadRef {
        root_event_id: origin_id,
        parent_event_id: wake_id,
    };
    let builder =
        buzz_sdk::build_message(channel_id, content, Some(&thread_ref), &[], false, &[], &[])
            .map_err(|e| format!("build_message: {e}"))?
            .tags([
                nostr::Tag::parse([TAG_DELEGATION_RUN, &binding.run_id])
                    .map_err(|e| format!("delegation-run tag: {e}"))?,
                nostr::Tag::parse([TAG_OUTCOME, outcome])
                    .map_err(|e| format!("delegation-outcome tag: {e}"))?,
                nostr::Tag::parse([TAG_TOKENS, &turn_tokens.to_string()])
                    .map_err(|e| format!("delegation-tokens tag: {e}"))?,
            ]);
    builder
        .sign_with_keys(agent_keys)
        .map_err(|e| format!("sign outcome event: {e}"))
}

/// Detect the literal `delegation-outcome: delegated` line (spec 4.3) in the
/// agent's final message chunk, case-sensitive, trimmed. Its absence means
/// `delivered`.
pub fn delegated_outcome_from_last_line(reply_text: &str) -> bool {
    reply_text
        .lines()
        .next_back()
        .map(|line| line.trim() == "delegation-outcome: delegated")
        .unwrap_or(false)
}
