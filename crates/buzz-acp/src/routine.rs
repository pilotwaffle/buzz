//! Routine (invoke_agent) handling for buzz-acp.
//!
//! Routines are workflow-driven agent invocations. The relay posts a kind:9 wake
//! event tagged `buzz:invoke-agent` with the agent's pubkey in a `p` tag and
//! the run id in a `buzz:workflow-run` tag. This module detects those events,
//! enforces token budgets per-run and per-day, and constructs the outcome event
//! that settles the dispatch.

/// Tags that identify a kind:9 event as a routine invocation.
pub const TAG_INVOKE_AGENT: &str = "buzz:invoke-agent";
/// Tag carrying the workflow run id.
pub const TAG_WORKFLOW_RUN: &str = "buzz:workflow-run";
/// Tag carrying the idempotency key.
pub const TAG_IDEMPOTENCY_KEY: &str = "buzz:idempotency-key";

/// Budgets extracted from the wake event by the relay, re-encoded as event tags
/// so the agent can enforce them without a direct DB connection.
pub const TAG_TOKEN_BUDGET_PER_RUN: &str = "buzz:token-budget-per-run";
/// Per-day token budget tag.
pub const TAG_TOKEN_BUDGET_PER_DAY: &str = "buzz:token-budget-per-day";
/// Outcome event tag for the run status.
pub const TAG_OUTCOME: &str = "buzz:routine-outcome";

/// Recognised outcome values posted by the agent.
pub const OUTCOME_SUCCEEDED: &str = "succeeded";
/// Budget exceeded — agent stopped early.
pub const OUTCOME_BUDGET_EXCEEDED_DAILY: &str = "budget_exceeded_daily";
/// Budget exceeded for this run only.
pub const OUTCOME_BUDGET_EXCEEDED_RUN: &str = "budget_exceeded_run";
/// Agent hit an unrecoverable error.
pub const OUTCOME_FAILED: &str = "failed";

/// Lightweight routine metadata stored on `FlushBatch` during admission.
///
/// Parsed from event tags after the inbound author gate passes.
#[derive(Debug, Clone)]
pub struct RoutineBinding {
    /// The workflow run id (UUID).
    pub run_id: String,
    /// The workflow/routine id (UUID).
    pub routine_id: String,
    /// Token budget for this single run.
    pub per_run: u64,
    /// Token budget for this calendar day (UTC).
    pub per_day: u64,
    /// The wake event id (used as the thread root for the outcome event).
    pub wake_event_id: String,
    /// The prompt to execute (event content).
    pub prompt: String,
    /// UUID of the result channel (from the `h` tag).
    pub result_channel: String,
}

/// Check whether a kind:9 event is an invoke_agent routine invocation.
///
/// Returns `true` when the event carries the `buzz:invoke-agent` tag with value
/// `"true"`. This is deliberately separate from `event_mentions_agent` — the `p`
/// tag gate for the agent's own pubkey is checked upstream.
pub fn is_routine_invocation(event: &nostr::Event) -> bool {
    event.tags.iter().any(|t| {
        let s = t.as_slice();
        s.first().map(|f| f.as_str()) == Some(TAG_INVOKE_AGENT)
            && s.get(1).map(|v| v.as_str()) == Some("true")
    })
}

/// Parse routine binding from an event that has already passed the author gate.
///
/// Only events carrying `buzz:invoke-agent=true` are parsed. Returns `None` for
/// non-routine events or when required tags are missing or malformed.
pub fn parse_routine_binding(event: &nostr::Event) -> Option<RoutineBinding> {
    if !is_routine_invocation(event) {
        return None;
    }

    let tag_value = |name: &str| -> Option<&str> {
        event.tags.iter().find_map(|t| {
            let s = t.as_slice();
            if s.first().map(|f| f.as_str()) == Some(name) {
                s.get(1).map(|v| v.as_str())
            } else {
                None
            }
        })
    };

    let run_id = tag_value("buzz:routine-run")?.to_owned();
    let routine_id = tag_value("buzz:routine")?.to_owned();
    let per_run = tag_value("buzz:routine-budget")
        .and_then(|v| v.split(',').next()?.parse().ok())
        .unwrap_or(0);
    let per_day = tag_value("buzz:routine-budget")
        .and_then(|v| v.split(',').nth(1)?.parse().ok())
        .unwrap_or(0);
    let wake_event_id = event.id.to_hex();
    let prompt = event.content.clone();
    let result_channel = tag_value("h")?.to_owned();

    Some(RoutineBinding {
        run_id,
        routine_id,
        per_run,
        per_day,
        wake_event_id,
        prompt,
        result_channel,
    })
}

/// Budget tracker for a routine run.
#[derive(Debug, Clone, Default)]
pub struct RoutineBudget {
    /// Tokens consumed so far in this run.
    pub tokens_used: u64,
    /// Hard cap for this run (0 = unlimited).
    pub per_run_cap: u64,
    /// Hard cap for today (0 = unlimited).
    pub per_day_cap: u64,
    /// Tokens already consumed today before this run started.
    pub prior_day_usage: u64,
}

impl RoutineBudget {
    /// Create a new budget tracker from a routine binding and prior day usage.
    pub fn new(binding: &RoutineBinding, prior_day_usage: u64) -> Self {
        Self {
            tokens_used: 0,
            per_run_cap: binding.per_run,
            per_day_cap: binding.per_day,
            prior_day_usage,
        }
    }

    /// Check whether another `delta` tokens can be consumed.
    ///
    /// Returns `None` when both budgets permit the spend. Returns `Some(outcome)`
    /// when a budget would be exceeded — the caller should stop and post the
    /// returned outcome.
    pub fn check(&self, delta: u64) -> Option<&'static str> {
        let projected = self.tokens_used.saturating_add(delta);

        if self.per_run_cap > 0 && projected > self.per_run_cap {
            return Some(OUTCOME_BUDGET_EXCEEDED_RUN);
        }
        if self.per_day_cap > 0 {
            let day_projected = self.prior_day_usage.saturating_add(projected);
            if day_projected > self.per_day_cap {
                return Some(OUTCOME_BUDGET_EXCEEDED_DAILY);
            }
        }
        None
    }

    /// Record `delta` tokens consumed. Caller should have called `check` first.
    pub fn record(&mut self, delta: u64) {
        self.tokens_used = self.tokens_used.saturating_add(delta);
    }
}

/// Build a kind:9 outcome event to post back to the result channel.
///
/// The event carries the run id and outcome so the relay can settle the
/// `routine_dispatches` row. Signed by the agent key (not the relay key)
/// per the spec contract.
pub fn build_outcome_event(
    agent_keys: &nostr::Keys,
    binding: &RoutineBinding,
    outcome: &str,
    _budget: &RoutineBudget,
    content: &str,
) -> nostr::Event {
    let kind = nostr::Kind::Custom(9);
    let tags = vec![
        nostr::Tag::parse(["h", &binding.result_channel]).expect("h tag"),
        nostr::Tag::parse(["buzz:routine-run", &binding.run_id]).expect("run tag"),
        nostr::Tag::parse([TAG_OUTCOME, outcome]).expect("outcome tag"),
    ];
    nostr::EventBuilder::new(kind, content)
        .tags(tags)
        .sign_with_keys(agent_keys)
        .expect("sign outcome event")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    fn make_routine_event(
        agent_pubkey: &str,
        run_id: &str,
        routine_id: &str,
        channel: &str,
        prompt: &str,
    ) -> nostr::Event {
        let keys = Keys::generate();
        EventBuilder::new(Kind::Custom(9), prompt)
            .tags([
                Tag::parse(["h", channel]).unwrap(),
                Tag::parse(["p", agent_pubkey]).unwrap(),
                Tag::parse(["buzz:invoke-agent", "true"]).unwrap(),
                Tag::parse(["buzz:routine-run", run_id]).unwrap(),
                Tag::parse(["buzz:routine", routine_id]).unwrap(),
                Tag::parse(["buzz:routine-budget", "50000,200000"]).unwrap(),
            ])
            .sign_with_keys(&keys)
            .unwrap()
    }

    fn make_regular_mention_event(agent_pubkey: &str) -> nostr::Event {
        let keys = Keys::generate();
        EventBuilder::new(Kind::Custom(9), "hey @agent")
            .tags([Tag::parse(["p", agent_pubkey]).unwrap()])
            .sign_with_keys(&keys)
            .unwrap()
    }

    #[test]
    fn detects_routine_invocation() {
        let event = make_routine_event(
            "c".repeat(64).as_str(),
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003",
            "00000000-0000-0000-0000-000000000002",
            "do the thing",
        );
        assert!(is_routine_invocation(&event));
    }

    #[test]
    fn ignores_regular_mention() {
        let event = make_regular_mention_event("c".repeat(64).as_str());
        assert!(!is_routine_invocation(&event));
    }

    #[test]
    fn parses_routine_binding() {
        let event = make_routine_event(
            "c".repeat(64).as_str(),
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003",
            "00000000-0000-0000-0000-000000000002",
            "do the thing",
        );
        let binding = parse_routine_binding(&event).expect("should parse");
        assert_eq!(binding.run_id, "00000000-0000-0000-0000-000000000001");
        assert_eq!(binding.routine_id, "00000000-0000-0000-0000-000000000003");
        assert_eq!(binding.per_run, 50000);
        assert_eq!(binding.per_day, 200000);
        assert_eq!(binding.prompt, "do the thing");
        assert_eq!(
            binding.result_channel,
            "00000000-0000-0000-0000-000000000002"
        );
        assert_eq!(binding.wake_event_id, event.id.to_hex());
    }

    #[test]
    fn returns_none_for_non_routine_event() {
        let event = make_regular_mention_event("c".repeat(64).as_str());
        assert!(parse_routine_binding(&event).is_none());
    }

    #[test]
    fn returns_none_when_missing_required_tags() {
        let keys = Keys::generate();
        // Missing buzz:routine-run and buzz:routine tags
        let event = EventBuilder::new(Kind::Custom(9), "prompt")
            .tags([
                Tag::parse(["h", "00000000-0000-0000-0000-000000000002"]).unwrap(),
                Tag::parse(["buzz:invoke-agent", "true"]).unwrap(),
            ])
            .sign_with_keys(&keys)
            .unwrap();
        assert!(parse_routine_binding(&event).is_none());
    }

    #[test]
    fn budget_check_allows_when_under_cap() {
        let budget = RoutineBudget {
            tokens_used: 50,
            per_run_cap: 100,
            per_day_cap: 200,
            prior_day_usage: 50,
        };
        assert_eq!(budget.check(25), None);
    }

    #[test]
    fn budget_check_blocks_run_exceeded() {
        let budget = RoutineBudget {
            tokens_used: 90,
            per_run_cap: 100,
            per_day_cap: 0,
            prior_day_usage: 0,
        };
        assert_eq!(budget.check(20), Some(OUTCOME_BUDGET_EXCEEDED_RUN));
    }

    #[test]
    fn budget_check_blocks_daily_exceeded() {
        let budget = RoutineBudget {
            tokens_used: 10,
            per_run_cap: 0,
            per_day_cap: 100,
            prior_day_usage: 95,
        };
        assert_eq!(budget.check(10), Some(OUTCOME_BUDGET_EXCEEDED_DAILY));
    }

    #[test]
    fn budget_blocks_when_run_ok_but_day_over() {
        let budget = RoutineBudget {
            tokens_used: 10,
            per_run_cap: 200,
            per_day_cap: 100,
            prior_day_usage: 95,
        };
        assert_eq!(budget.check(10), Some(OUTCOME_BUDGET_EXCEEDED_DAILY));
    }

    #[test]
    fn budget_zero_caps_are_unlimited() {
        let budget = RoutineBudget::default();
        assert_eq!(budget.check(1_000_000), None);
    }

    #[test]
    fn budget_record_updates_count() {
        let mut budget = RoutineBudget::default();
        budget.record(100);
        assert_eq!(budget.tokens_used, 100);
        budget.record(50);
        assert_eq!(budget.tokens_used, 150);
    }

    #[test]
    fn outcome_event_carries_required_tags() {
        let keys = Keys::generate();
        let binding = RoutineBinding {
            run_id: "00000000-0000-0000-0000-000000000001".into(),
            routine_id: "00000000-0000-0000-0000-000000000003".into(),
            per_run: 100,
            per_day: 500,
            wake_event_id: "wake-event-id-hex".into(),
            prompt: "do the thing".into(),
            result_channel: "00000000-0000-0000-0000-000000000002".into(),
        };
        let budget = RoutineBudget {
            tokens_used: 42,
            per_run_cap: 100,
            per_day_cap: 500,
            prior_day_usage: 200,
        };
        let event = build_outcome_event(
            &keys,
            &binding,
            OUTCOME_SUCCEEDED,
            &budget,
            "routine run 00000000-0000-0000-0000-000000000001 completed",
        );
        let has_outcome = event.tags.iter().any(|t| {
            let s = t.as_slice();
            s.first().map(|f| f.as_str()) == Some(TAG_OUTCOME)
                && s.get(1).map(|v| v.as_str()) == Some(OUTCOME_SUCCEEDED)
        });
        assert!(has_outcome);
        let has_channel = event.tags.iter().any(|t| {
            let s = t.as_slice();
            s.first().map(|f| f.as_str()) == Some("h")
                && s.get(1).map(|v| v.as_str()) == Some(binding.result_channel.as_str())
        });
        assert!(has_channel);
    }
}