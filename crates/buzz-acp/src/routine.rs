//! Routine (invoke_agent) handling for buzz-acp.
//!
//! Routines are workflow-driven agent invocations. The relay posts a
//! relay-signed kind:9 wake tagged `buzz:routine-run` (the workflow run id),
//! `buzz:routine` (the workflow id), `buzz:routine-idem`, and
//! `buzz:routine-budget` (per_run,per_day) — see `crates/buzz-relay/src/workflow_sink.rs`
//! `RelayActionSink::invoke_agent` for the wire producer. This module detects
//! those events after they have already passed the inbound author gate,
//! enforces the two token budgets, and builds the outcome event that settles
//! the dispatch.

/// Tag carrying the workflow run id (also present on the agent-signed outcome).
pub const TAG_ROUTINE_RUN: &str = "buzz:routine-run";
/// Tag carrying the workflow (routine) id.
pub const TAG_ROUTINE: &str = "buzz:routine";
/// Tag carrying the resolved idempotency key.
pub const TAG_ROUTINE_IDEM: &str = "buzz:routine-idem";
/// Tag carrying the two decimal budgets: per_run, per_day.
pub const TAG_ROUTINE_BUDGET: &str = "buzz:routine-budget";
/// Outcome event tag for the run status.
pub const TAG_OUTCOME: &str = "buzz:routine-outcome";

/// Recognised outcome values posted by the agent (frozen tag contract).
pub const OUTCOME_SUCCEEDED: &str = "succeeded";
/// Agent hit an unrecoverable error.
pub const OUTCOME_FAILED: &str = "failed";
/// Per-run token budget exceeded.
pub const OUTCOME_BUDGET_EXCEEDED_PER_RUN: &str = "budget_exceeded_per_run";
/// Daily token budget exceeded.
pub const OUTCOME_BUDGET_EXCEEDED_DAILY: &str = "budget_exceeded_daily";

/// Lightweight routine metadata stored on `FlushBatch` during admission.
///
/// Parsed from event tags only after the event has already passed the
/// inbound author gate (`require_mention`/`p` tag) — a routine tag on an
/// event that failed the gate is never parsed.
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
    /// The wake event id (hex) — the thread root for the outcome event.
    pub wake_event_id: String,
    /// The prompt to execute (event content, minus the trailing run-id line).
    pub prompt: String,
    /// UUID of the result channel (from the `h` tag).
    pub result_channel: String,
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

/// Find the `buzz:routine-budget` tag and return its two values (elements 1
/// and 2). The relay emits `["buzz:routine-budget", per_run, per_day]` as
/// two separate tag values, never a single comma-joined field — see
/// `RelayActionSink::invoke_agent` (`crates/buzz-relay/src/workflow_sink.rs`).
fn routine_budget_values<'a>(event: &'a nostr::Event) -> Option<(&'a str, &'a str)> {
    event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        if s.first().map(|f| f.as_str()) == Some(TAG_ROUTINE_BUDGET) {
            Some((s.get(1)?.as_str(), s.get(2)?.as_str()))
        } else {
            None
        }
    })
}

/// Parse routine binding from an event that has already passed the author
/// gate. Returns `None` for non-routine events or when required tags are
/// missing or malformed — a malformed budget never falls back to unlimited.
pub fn parse_routine_binding(event: &nostr::Event) -> Option<RoutineBinding> {
    let run_id = tag_value(event, TAG_ROUTINE_RUN)?.to_owned();
    let routine_id = tag_value(event, TAG_ROUTINE)?.to_owned();
    let (per_run_str, per_day_str) = routine_budget_values(event)?;
    let per_run: u64 = per_run_str.parse().ok()?;
    let per_day: u64 = per_day_str.parse().ok()?;
    let result_channel = tag_value(event, "h")?.to_owned();
    let wake_event_id = event.id.to_hex();
    let prompt = event.content.clone();

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
            return Some(OUTCOME_BUDGET_EXCEEDED_PER_RUN);
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

/// Fixed outcome content strings (5.4). Never the prompt, the reply, a
/// count, or a cost — `detail` is a fixed one-word failure reason
/// (`store_unavailable`, `cancelled`), never free text.
pub fn outcome_content(run_id: &str, outcome: &str, detail: Option<&str>) -> String {
    match outcome {
        OUTCOME_SUCCEEDED => format!("routine run {run_id} completed"),
        OUTCOME_BUDGET_EXCEEDED_PER_RUN => {
            format!("routine run {run_id} exceeded its per-run token budget")
        }
        OUTCOME_BUDGET_EXCEEDED_DAILY => {
            format!("routine run {run_id} reached the routine's daily token budget")
        }
        _ => match detail {
            Some(d) => format!("routine run {run_id} failed: {d}"),
            None => format!("routine run {run_id} failed"),
        },
    }
}

/// Build a kind:9 outcome event to post back to the result channel, threaded
/// under the wake event. Signed by the agent key (not the relay key) per the
/// spec contract; the tags carry exactly one `buzz:routine-run` and one
/// `buzz:routine-outcome`.
pub fn build_outcome_event(
    agent_keys: &nostr::Keys,
    binding: &RoutineBinding,
    outcome: &str,
    content: &str,
) -> Result<nostr::Event, String> {
    let wake_id = nostr::EventId::from_hex(&binding.wake_event_id)
        .map_err(|e| format!("wake_event_id: {e}"))?;
    let channel_id: uuid::Uuid = binding
        .result_channel
        .parse()
        .map_err(|e| format!("result_channel: {e}"))?;
    let thread_ref = buzz_sdk::ThreadRef {
        root_event_id: wake_id,
        parent_event_id: wake_id,
    };
    let builder = buzz_sdk::build_message(channel_id, content, Some(&thread_ref), &[], false, &[], &[])
        .map_err(|e| format!("build_message: {e}"))?
        .tags([
            nostr::Tag::parse([TAG_ROUTINE_RUN, &binding.run_id])
                .map_err(|e| format!("routine-run tag: {e}"))?,
            nostr::Tag::parse([TAG_OUTCOME, outcome]).map_err(|e| format!("outcome tag: {e}"))?,
        ]);
    builder
        .sign_with_keys(agent_keys)
        .map_err(|e| format!("sign outcome event: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    /// The shared tag-shape fixture (`test-fixtures/routine-budget-tag.json`)
    /// also loaded by the buzz-relay e2e test, so the two sides cannot drift.
    #[derive(serde::Deserialize)]
    struct RoutineBudgetFixture {
        #[serde(rename = "tagName")]
        tag_name: String,
        #[serde(rename = "perRun")]
        per_run: u64,
        #[serde(rename = "perDay")]
        per_day: u64,
    }

    fn routine_budget_fixture() -> RoutineBudgetFixture {
        serde_json::from_str(include_str!("../../../test-fixtures/routine-budget-tag.json"))
            .expect("valid routine-budget-tag fixture")
    }

    fn make_routine_event(
        agent_pubkey: &str,
        run_id: &str,
        routine_id: &str,
        channel: &str,
        prompt: &str,
    ) -> nostr::Event {
        let fixture = routine_budget_fixture();
        let keys = Keys::generate();
        EventBuilder::new(Kind::Custom(9), prompt)
            .tags([
                Tag::parse(["h", channel]).unwrap(),
                Tag::parse(["p", agent_pubkey]).unwrap(),
                Tag::parse(["buzz:workflow-mention", agent_pubkey]).unwrap(),
                Tag::parse([TAG_ROUTINE_RUN, run_id]).unwrap(),
                Tag::parse([TAG_ROUTINE, routine_id]).unwrap(),
                Tag::parse([
                    fixture.tag_name.as_str(),
                    &fixture.per_run.to_string(),
                    &fixture.per_day.to_string(),
                ])
                .unwrap(),
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
    fn parses_routine_binding() {
        let event = make_routine_event(
            "c".repeat(64).as_str(),
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003",
            "00000000-0000-0000-0000-000000000002",
            "do the thing",
        );
        let binding = parse_routine_binding(&event).expect("should parse");
        let fixture = routine_budget_fixture();
        assert_eq!(binding.run_id, "00000000-0000-0000-0000-000000000001");
        assert_eq!(binding.routine_id, "00000000-0000-0000-0000-000000000003");
        assert_eq!(binding.per_run, fixture.per_run);
        assert_eq!(binding.per_day, fixture.per_day);
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
        // Missing buzz:routine and buzz:routine-budget tags.
        let event = EventBuilder::new(Kind::Custom(9), "prompt")
            .tags([
                Tag::parse(["h", "00000000-0000-0000-0000-000000000002"]).unwrap(),
                Tag::parse([TAG_ROUTINE_RUN, "00000000-0000-0000-0000-000000000001"]).unwrap(),
            ])
            .sign_with_keys(&keys)
            .unwrap();
        assert!(parse_routine_binding(&event).is_none());
    }

    #[test]
    fn returns_none_for_malformed_budget_never_falls_back_to_unlimited() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "prompt")
            .tags([
                Tag::parse(["h", "00000000-0000-0000-0000-000000000002"]).unwrap(),
                Tag::parse([TAG_ROUTINE_RUN, "00000000-0000-0000-0000-000000000001"]).unwrap(),
                Tag::parse([TAG_ROUTINE, "00000000-0000-0000-0000-000000000003"]).unwrap(),
                Tag::parse([TAG_ROUTINE_BUDGET, "not-a-number", "200000"]).unwrap(),
            ])
            .sign_with_keys(&keys)
            .unwrap();
        assert!(parse_routine_binding(&event).is_none());
    }

    #[test]
    fn returns_none_when_budget_tag_has_only_one_value() {
        // Regression for S3-2: a comma-joined single value must never parse
        // as if it were the two-value wire shape the relay actually emits.
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "prompt")
            .tags([
                Tag::parse(["h", "00000000-0000-0000-0000-000000000002"]).unwrap(),
                Tag::parse([TAG_ROUTINE_RUN, "00000000-0000-0000-0000-000000000001"]).unwrap(),
                Tag::parse([TAG_ROUTINE, "00000000-0000-0000-0000-000000000003"]).unwrap(),
                Tag::parse([TAG_ROUTINE_BUDGET, "50000,200000"]).unwrap(),
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
        assert_eq!(budget.check(20), Some(OUTCOME_BUDGET_EXCEEDED_PER_RUN));
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
    fn outcome_content_matches_fixed_strings() {
        assert_eq!(
            outcome_content("run-1", OUTCOME_SUCCEEDED, None),
            "routine run run-1 completed"
        );
        assert_eq!(
            outcome_content("run-1", OUTCOME_BUDGET_EXCEEDED_PER_RUN, None),
            "routine run run-1 exceeded its per-run token budget"
        );
        assert_eq!(
            outcome_content("run-1", OUTCOME_BUDGET_EXCEEDED_DAILY, None),
            "routine run run-1 reached the routine's daily token budget"
        );
        assert_eq!(
            outcome_content("run-1", OUTCOME_FAILED, Some("store_unavailable")),
            "routine run run-1 failed: store_unavailable"
        );
    }

    #[test]
    fn outcome_event_is_threaded_under_the_wake_with_exact_tags() {
        let keys = Keys::generate();
        let wake = make_routine_event(
            &keys.public_key().to_hex(),
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003",
            "00000000-0000-0000-0000-000000000002",
            "do the thing",
        );
        let binding = parse_routine_binding(&wake).expect("parse");
        let content = outcome_content(&binding.run_id, OUTCOME_SUCCEEDED, None);
        let event =
            build_outcome_event(&keys, &binding, OUTCOME_SUCCEEDED, &content).expect("build");

        assert_eq!(event.pubkey, keys.public_key());
        assert_eq!(event.kind, Kind::Custom(9));

        let e_tags: Vec<&nostr::Tag> = event
            .tags
            .iter()
            .filter(|t| t.as_slice().first().map(|f| f.as_str()) == Some("e"))
            .collect();
        assert_eq!(e_tags.len(), 1, "direct reply must emit exactly one e tag");
        assert_eq!(e_tags[0].as_slice().get(1), Some(&wake.id.to_hex()));
        assert_eq!(e_tags[0].as_slice().get(3).map(String::as_str), Some("reply"));

        let run_tags: Vec<&nostr::Tag> = event
            .tags
            .iter()
            .filter(|t| t.as_slice().first().map(|f| f.as_str()) == Some(TAG_ROUTINE_RUN))
            .collect();
        assert_eq!(run_tags.len(), 1);
        assert_eq!(run_tags[0].as_slice().get(1), Some(&binding.run_id));

        let outcome_tags: Vec<&nostr::Tag> = event
            .tags
            .iter()
            .filter(|t| t.as_slice().first().map(|f| f.as_str()) == Some(TAG_OUTCOME))
            .collect();
        assert_eq!(outcome_tags.len(), 1);
        assert_eq!(
            outcome_tags[0].as_slice().get(1).map(String::as_str),
            Some(OUTCOME_SUCCEEDED)
        );
    }
}
