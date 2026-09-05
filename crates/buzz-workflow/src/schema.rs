//! YAML/JSON workflow definition types.
//!
//! Workflow definitions are authored in YAML and stored as canonical JSON.
//! All types must round-trip through both formats without loss.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, NaiveDate};
use cron::TimeUnitSpec;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::error::WorkflowError;

/// Top-level workflow definition, authored in YAML and stored as canonical JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    /// Human-readable workflow name (must be non-empty).
    pub name: String,
    /// Optional description shown in the UI.
    #[serde(default)]
    pub description: Option<String>,
    /// The event trigger that starts this workflow.
    pub trigger: TriggerDef,
    /// Ordered list of steps to execute when triggered.
    pub steps: Vec<Step>,
    /// Whether this workflow is active. Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Trigger definition. The `on` field is the tag.
///
/// Serde internally-tagged: `on: message_posted`, `on: reaction_added`, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum TriggerDef {
    /// Fires when any message is posted in the workflow's channel.
    MessagePosted {
        /// Optional evalexpr filter (flat var names, e.g. `trigger_text`).
        #[serde(default)]
        filter: Option<String>,
    },
    /// Fires when an emoji reaction is added to a message.
    ReactionAdded {
        /// Optional: only fire for this specific emoji.
        #[serde(default)]
        emoji: Option<String>,
    },
    /// Fires when a diff message (kind:40008) is posted in the workflow's channel.
    DiffPosted {
        /// Optional evalexpr filter expression (same variables as MessagePosted).
        #[serde(default)]
        filter: Option<String>,
    },
    /// Fires on a cron schedule.
    Schedule {
        /// Cron expression (UTC). Mutually exclusive with `interval`.
        #[serde(default)]
        cron: Option<String>,
        /// Simple interval string (e.g. "1h", "30m"). Mutually exclusive with `cron`.
        #[serde(default)]
        interval: Option<String>,
    },
    /// Fires when HTTP POST arrives at `/hooks/{id}`.
    Webhook,
}

/// A single step in a workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Unique step identifier within this workflow.
    pub id: String,
    /// Optional human-readable step name.
    #[serde(default)]
    pub name: Option<String>,
    /// evalexpr condition. Step is skipped (not failed) if false.
    #[serde(rename = "if", default)]
    pub if_expr: Option<String>,
    /// Maximum seconds this step may run before timing out.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// The action to perform when this step executes.
    #[serde(flatten)]
    pub action: ActionDef,
}

/// Action definition. The `action` field is the tag.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ActionDef {
    /// Post a message to the workflow's channel (or an override channel).
    SendMessage {
        /// Message text (supports template variables).
        text: String,
        /// Optional channel UUID override. Must be a valid UUID string.
        #[serde(default)]
        channel: Option<String>,
    },
    /// Send a direct message to a user.
    SendDm {
        /// Recipient — pubkey hex or `{{trigger.author}}`.
        to: String,
        /// Message text (supports template variables).
        text: String,
    },
    /// Update the channel topic.
    SetChannelTopic {
        /// New topic string.
        topic: String,
    },
    /// Add an emoji reaction to the triggering message.
    AddReaction {
        /// Emoji name (e.g. `"thumbsup"`).
        emoji: String,
    },
    /// HTTP POST to an external URL.
    CallWebhook {
        /// Target URL (must be a public HTTPS endpoint).
        url: String,
        /// HTTP method override (default: `"POST"`).
        #[serde(default)]
        method: Option<String>,
        /// Additional request headers.
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
        /// Request body template.
        #[serde(default)]
        body: Option<String>,
    },
    /// Suspend execution and request approval.
    RequestApproval {
        /// User mention or role (e.g. `"@release-manager"`).
        from: String,
        /// Message shown to the approver.
        message: String,
        /// Duration string (e.g. `"24h"`). Defaults to 24h.
        #[serde(default)]
        timeout: Option<String>,
    },
    /// Pause execution for a duration (e.g. `"5m"`, `"1h"`).
    Delay {
        /// Duration string (e.g. `"5m"`, `"1h"`).
        duration: String,
    },
    /// Invoke a managed agent and post its result to a fixed channel.
    ///
    /// Slice 0 freezes this serialized contract only. Runtime dispatch remains
    /// disabled until Slice 3 wires durable claims and authorization.
    InvokeAgent {
        /// Target managed-agent Nostr public key (64-character hex).
        agent_pubkey: String,
        /// Agent task text (supports template variables).
        prompt: String,
        /// Fixed UUID of the channel that receives the result.
        result_channel: String,
        /// Caller-supplied duplicate-suppression key (supports templates).
        idempotency_key: String,
    },
}

// Keep legacy variants' permissive decoding unchanged while making the new
// Slice-0 contract exact. Applying `deny_unknown_fields` to `ActionDef` itself
// would be a breaking change for every existing workflow action.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ActionDefWire {
    SendMessage {
        text: String,
        #[serde(default)]
        channel: Option<String>,
    },
    SendDm {
        to: String,
        text: String,
    },
    SetChannelTopic {
        topic: String,
    },
    AddReaction {
        emoji: String,
    },
    CallWebhook {
        url: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
        #[serde(default)]
        body: Option<String>,
    },
    RequestApproval {
        from: String,
        message: String,
        #[serde(default)]
        timeout: Option<String>,
    },
    Delay {
        duration: String,
    },
    InvokeAgent(InvokeAgentWire),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeAgentWire {
    agent_pubkey: String,
    prompt: String,
    result_channel: String,
    idempotency_key: String,
}

impl<'de> Deserialize<'de> for ActionDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match ActionDefWire::deserialize(deserializer)? {
            ActionDefWire::SendMessage { text, channel } => Self::SendMessage { text, channel },
            ActionDefWire::SendDm { to, text } => Self::SendDm { to, text },
            ActionDefWire::SetChannelTopic { topic } => Self::SetChannelTopic { topic },
            ActionDefWire::AddReaction { emoji } => Self::AddReaction { emoji },
            ActionDefWire::CallWebhook {
                url,
                method,
                headers,
                body,
            } => Self::CallWebhook {
                url,
                method,
                headers,
                body,
            },
            ActionDefWire::RequestApproval {
                from,
                message,
                timeout,
            } => Self::RequestApproval {
                from,
                message,
                timeout,
            },
            ActionDefWire::Delay { duration } => Self::Delay { duration },
            ActionDefWire::InvokeAgent(fields) => Self::InvokeAgent {
                agent_pubkey: fields.agent_pubkey,
                prompt: fields.prompt,
                result_channel: fields.result_channel,
                idempotency_key: fields.idempotency_key,
            },
        })
    }
}

const MAX_INVOKE_AGENT_PROMPT_BYTES: usize = 64 * 1024;
const MAX_INVOKE_AGENT_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MIN_INVOKE_AGENT_CADENCE_SECS: u32 = 15 * 60;

impl ActionDef {
    /// Validate invariants that belong to an individual action definition.
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        let Self::InvokeAgent {
            agent_pubkey,
            prompt,
            result_channel,
            idempotency_key,
        } = self
        else {
            return Ok(());
        };

        if agent_pubkey.len() != 64
            || !agent_pubkey
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            return Err(WorkflowError::InvalidDefinition(
                "invoke_agent agent_pubkey must be a canonical lowercase 64-character hex public key"
                    .into(),
            ));
        }
        agent_pubkey.parse::<nostr::PublicKey>().map_err(|_| {
            WorkflowError::InvalidDefinition(
                "invoke_agent agent_pubkey must be a valid Nostr public key".into(),
            )
        })?;

        if prompt.trim().is_empty() {
            return Err(WorkflowError::InvalidDefinition(
                "invoke_agent prompt is required and must not be empty".into(),
            ));
        }
        if prompt.len() > MAX_INVOKE_AGENT_PROMPT_BYTES {
            return Err(WorkflowError::InvalidDefinition(format!(
                "invoke_agent prompt exceeds the {MAX_INVOKE_AGENT_PROMPT_BYTES}-byte limit"
            )));
        }

        let result_channel = result_channel.parse::<Uuid>().map_err(|_| {
            WorkflowError::InvalidDefinition("invoke_agent result_channel must be a UUID".into())
        })?;
        if result_channel.is_nil() {
            return Err(WorkflowError::InvalidDefinition(
                "invoke_agent result_channel must be a non-nil UUID".into(),
            ));
        }

        if idempotency_key.trim().is_empty() {
            return Err(WorkflowError::InvalidDefinition(
                "invoke_agent idempotency_key is required and must not be empty".into(),
            ));
        }
        if idempotency_key.len() > MAX_INVOKE_AGENT_IDEMPOTENCY_KEY_BYTES {
            return Err(WorkflowError::InvalidDefinition(format!(
                "invoke_agent idempotency_key exceeds the {MAX_INVOKE_AGENT_IDEMPOTENCY_KEY_BYTES}-byte limit"
            )));
        }
        if idempotency_key.chars().any(char::is_control) {
            return Err(WorkflowError::InvalidDefinition(
                "invoke_agent idempotency_key must not contain control characters".into(),
            ));
        }

        Ok(())
    }
}

impl WorkflowDef {
    /// True when any step performs an action that can exfiltrate channel data
    /// to an arbitrary external destination (`call_webhook`).
    ///
    /// Definitions with such steps require elevated (owner/admin) channel
    /// authority both to save and to run — plain membership is not enough,
    /// because a workflow forwards channel content with the *owner's* standing
    /// authority long after the save (SEC-006).
    pub fn requires_elevated_authority(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.action, ActionDef::CallWebhook { .. }))
    }

    /// Validate the workflow definition. Returns `Err` with a descriptive message
    /// if any invariant is violated.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.name.trim().is_empty() {
            return Err(WorkflowError::InvalidDefinition(
                "name is required and must not be empty".into(),
            ));
        }

        if self.steps.is_empty() {
            return Err(WorkflowError::InvalidDefinition(
                "at least one step is required".into(),
            ));
        }

        // Validate step IDs are safe for use in evalexpr variable names.
        // Step IDs become variable names like `steps_{id}_output_{field}`,
        // so they must only contain alphanumeric chars and underscores.
        let valid_step_id = |id: &str| -> bool {
            !id.is_empty()
                && id.len() <= 64
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };

        let mut seen_ids: HashSet<&str> = HashSet::new();
        for step in &self.steps {
            if step.id.trim().is_empty() {
                return Err(WorkflowError::InvalidDefinition(
                    "step id must not be empty".into(),
                ));
            }
            if !valid_step_id(&step.id) {
                return Err(WorkflowError::InvalidDefinition(format!(
                    "step id '{}' is invalid: must contain only alphanumeric characters and underscores",
                    step.id
                )));
            }
            if !seen_ids.insert(step.id.as_str()) {
                return Err(WorkflowError::InvalidDefinition(format!(
                    "duplicate step id: {}",
                    step.id
                )));
            }
            step.action.validate()?;
        }

        if let TriggerDef::Schedule { cron, interval } = &self.trigger {
            let invokes_agent = self
                .steps
                .iter()
                .any(|step| matches!(step.action, ActionDef::InvokeAgent { .. }));
            if cron.is_none() && interval.is_none() {
                return Err(WorkflowError::InvalidDefinition(
                    "schedule trigger requires either 'cron' or 'interval'".into(),
                ));
            }

            if cron.is_some() && interval.is_some() {
                return Err(WorkflowError::InvalidDefinition(
                    "schedule trigger cannot specify both 'cron' and 'interval'; use one or the other".into(),
                ));
            }

            if let Some(expr) = cron {
                let schedule = validate_cron(expr)?;
                if invokes_agent {
                    validate_invoke_agent_cron_cadence(&schedule)?;
                }
            }

            if let Some(dur) = interval {
                let secs = crate::executor::parse_duration_secs(dur).map_err(|_| {
                    WorkflowError::InvalidDefinition(format!(
                        "invalid interval '{dur}': expected a duration like '30m', '1h', or '60s'"
                    ))
                })?;
                // Fix 4: the cron loop ticks every 60s, so sub-minute intervals
                // can never fire correctly. Reject them at definition time.
                if secs < 60 {
                    return Err(WorkflowError::InvalidDefinition(
                        "interval must be at least 60s (cron loop ticks every 60 seconds)".into(),
                    ));
                }
                if secs < u64::from(MIN_INVOKE_AGENT_CADENCE_SECS) && invokes_agent {
                    return Err(WorkflowError::InvalidDefinition(
                        "invoke_agent schedule interval must be at least 15m".into(),
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Validate a cron expression using the `cron` crate.
///
/// The `cron` crate requires 7 fields: `sec min hour dom month dow year`.
/// Standard 5-field cron (`min hour dom month dow`) is normalized by prepending
/// `0` (seconds) and appending `*` (any year).
fn validate_cron(expr: &str) -> Result<cron::Schedule, WorkflowError> {
    let normalized = normalize_cron(expr);
    normalized.parse::<cron::Schedule>().map_err(|e| {
        WorkflowError::InvalidDefinition(format!("invalid cron expression '{expr}': {e}"))
    })
}

/// Prove that a parsed UTC cron never fires twice within the routine minimum.
///
/// `cron` 0.16 exposes the complete finite domain it supports (years
/// 1970..=2100) through [`TimeUnitSpec`]. Checking parsed ordinals rather than
/// sampling upcoming occurrences avoids horizon gaps and works for aliases and
/// all expression spellings accepted by the parser. There are at most 86,400
/// time-of-day combinations and 47,848 dates in that domain.
fn validate_invoke_agent_cron_cadence(schedule: &cron::Schedule) -> Result<(), WorkflowError> {
    let mut first_second_of_day = None;
    let mut previous_second_of_day = None;
    let mut last_second_of_day = None;
    let mut has_too_close_same_day_pair = false;

    for hour in schedule.hours().iter() {
        for minute in schedule.minutes().iter() {
            for second in schedule.seconds().iter() {
                let current = hour * 3_600 + minute * 60 + second;
                first_second_of_day.get_or_insert(current);
                if previous_second_of_day
                    .is_some_and(|previous| current - previous < MIN_INVOKE_AGENT_CADENCE_SECS)
                {
                    has_too_close_same_day_pair = true;
                }
                previous_second_of_day = Some(current);
                last_second_of_day = Some(current);
            }
        }
    }

    let (Some(first_second_of_day), Some(last_second_of_day)) =
        (first_second_of_day, last_second_of_day)
    else {
        return Ok(());
    };
    let midnight_gap = 86_400 - last_second_of_day + first_second_of_day;

    let Some(first_year) = schedule.years().iter().next() else {
        return Ok(());
    };
    let Some(last_year) = schedule.years().iter().next_back() else {
        return Ok(());
    };
    let Some(mut date) = NaiveDate::from_ymd_opt(first_year as i32, 1, 1) else {
        return Err(WorkflowError::InvalidDefinition(
            "invoke_agent cron has an unsupported start year".into(),
        ));
    };
    let Some(last_date) = NaiveDate::from_ymd_opt(last_year as i32, 12, 31) else {
        return Err(WorkflowError::InvalidDefinition(
            "invoke_agent cron has an unsupported end year".into(),
        ));
    };

    let mut previous_date_matched = false;
    loop {
        let date_matches = schedule.years().includes(date.year() as u32)
            && schedule.months().includes(date.month())
            && schedule.days_of_month().includes(date.day())
            && schedule
                .days_of_week()
                .includes(date.weekday().number_from_sunday());

        if date_matches
            && (has_too_close_same_day_pair
                || (previous_date_matched && midnight_gap < MIN_INVOKE_AGENT_CADENCE_SECS))
        {
            return Err(WorkflowError::InvalidDefinition(
                "invoke_agent cron cadence must be at least 15m".into(),
            ));
        }

        if date == last_date {
            break;
        }
        previous_date_matched = date_matches;
        date = date.succ_opt().ok_or_else(|| {
            WorkflowError::InvalidDefinition(
                "invoke_agent cron date domain overflowed during cadence validation".into(),
            )
        })?;
    }

    Ok(())
}

/// Normalize a cron expression to the 7-field format required by the `cron` crate.
///
/// - 5 fields (`min hour dom month dow`) → prepend `0` (sec), append `*` (year)
/// - 6 fields → append `*` (year)
/// - 7 fields → unchanged
pub(crate) fn normalize_cron(expr: &str) -> String {
    let field_count = expr.split_whitespace().count();
    match field_count {
        5 => format!("0 {expr} *"),
        6 => format!("{expr} *"),
        _ => expr.to_owned(),
    }
}

/// Parse a YAML workflow definition, validate it, and return the canonical JSON.
///
/// Returns `(WorkflowDef, canonical_json)` on success.
pub fn parse_yaml(yaml: &str) -> Result<(WorkflowDef, String), WorkflowError> {
    let def: WorkflowDef = serde_yaml::from_str(yaml)?;
    def.validate()?;
    let json =
        serde_json::to_string(&def).map_err(|e| WorkflowError::InvalidDefinition(e.to_string()))?;
    Ok((def, json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_message_posted_workflow() {
        // Use single-quoted YAML strings to avoid raw-string delimiter conflicts.
        let yaml = "name: 'Incident Alert'\ndescription: 'Alert on P1 messages'\ntrigger:\n  on: message_posted\n  filter: 'str_contains(trigger_text, \"P1\")'\nsteps:\n  - id: notify\n    action: send_message\n    text: 'P1 alert'\n";
        let (def, json) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.name, "Incident Alert");
        assert!(def.enabled); // default true
        assert_eq!(def.steps.len(), 1);
        assert_eq!(def.steps[0].id, "notify");

        let reparsed: WorkflowDef = serde_json::from_str(&json).expect("json round-trip");
        assert_eq!(reparsed.name, def.name);
    }

    #[test]
    fn parse_reaction_added_trigger() {
        let yaml = "name: Triage\ntrigger:\n  on: reaction_added\n  emoji: clipboard\nsteps:\n  - id: ack\n    action: add_reaction\n    emoji: eyes\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.trigger {
            TriggerDef::ReactionAdded { emoji } => {
                assert_eq!(emoji.as_deref(), Some("clipboard"));
            }
            other => panic!("unexpected trigger: {other:?}"),
        }
    }

    #[test]
    fn parse_schedule_trigger() {
        let yaml = "name: Daily Standup\ntrigger:\n  on: schedule\n  cron: '0 9 * * 1-5'\nsteps:\n  - id: prompt\n    action: send_message\n    text: Standup time\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.trigger {
            TriggerDef::Schedule { cron, .. } => {
                assert_eq!(cron.as_deref(), Some("0 9 * * 1-5"));
            }
            other => panic!("unexpected trigger: {other:?}"),
        }
    }

    #[test]
    fn parse_workflow_with_conditions() {
        // Use single-quoted YAML strings; evalexpr expressions use double quotes inside.
        let yaml = concat!(
            "name: Conditional Workflow\n",
            "trigger:\n  on: message_posted\n",
            "steps:\n",
            "  - id: escalate\n",
            "    if: 'str_contains(trigger_text, \"P1\") || str_contains(trigger_text, \"SEV1\")'\n",
            "    action: send_message\n",
            "    text: P1 escalation\n",
            "  - id: normal\n",
            "    if: '!str_contains(trigger_text, \"P1\")'\n",
            "    action: send_message\n",
            "    text: Normal message\n",
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.steps.len(), 2);
        assert!(def.steps[0].if_expr.is_some());
        assert!(def.steps[1].if_expr.is_some());
    }

    #[test]
    fn parse_all_action_types() {
        // Avoid "# in YAML values (would close r# raw strings).
        // Use unquoted or single-quoted YAML values throughout.
        let yaml = concat!(
            "name: All Actions\n",
            "trigger:\n  on: webhook\n",
            "steps:\n",
            "  - id: msg\n    action: send_message\n    text: Hello\n    channel: general\n",
            "  - id: dm\n    action: send_dm\n    to: '{{trigger.author}}'\n    text: You triggered this\n",
            "  - id: topic\n    action: set_channel_topic\n    topic: Status active\n",
            "  - id: react\n    action: add_reaction\n    emoji: white_check_mark\n",
            "  - id: hook\n    action: call_webhook\n    url: https://hooks.example.com/notify\n    method: POST\n",
            "  - id: approve\n    action: request_approval\n    from: '@manager'\n    message: Approve?\n    timeout: 4h\n",
            "  - id: wait\n    action: delay\n    duration: 5m\n",
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.steps.len(), 7);

        assert!(matches!(
            &def.steps[0].action,
            ActionDef::SendMessage { .. }
        ));
        assert!(matches!(&def.steps[1].action, ActionDef::SendDm { .. }));
        assert!(matches!(
            &def.steps[2].action,
            ActionDef::SetChannelTopic { .. }
        ));
        assert!(matches!(
            &def.steps[3].action,
            ActionDef::AddReaction { .. }
        ));
        assert!(matches!(
            &def.steps[4].action,
            ActionDef::CallWebhook { .. }
        ));
        assert!(matches!(
            &def.steps[5].action,
            ActionDef::RequestApproval { .. }
        ));
        assert!(matches!(&def.steps[6].action, ActionDef::Delay { .. }));
    }

    #[test]
    fn parse_approval_gate_example() {
        let yaml = concat!(
            "name: Deploy Approval\n",
            "trigger:\n  on: webhook\n",
            "steps:\n",
            "  - id: request\n    action: request_approval\n    from: '@engineering-lead'\n",
            "    message: Approve deploy?\n    timeout: 4h\n",
            "  - id: notify_approved\n    if: 'steps_request_output_approved == true'\n",
            "    action: send_message\n    text: Deploy approved\n",
            "  - id: notify_denied\n    if: 'steps_request_output_approved == false'\n",
            "    action: send_message\n    text: Deploy denied\n",
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.steps.len(), 3);
    }

    #[test]
    fn validate_rejects_empty_name() {
        let yaml =
            "name: ''\ntrigger:\n  on: message_posted\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidDefinition(_)),
            "expected InvalidDefinition, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_empty_steps() {
        let yaml = "name: No Steps\ntrigger:\n  on: message_posted\nsteps: []\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    }

    #[test]
    fn validate_rejects_duplicate_step_ids() {
        let yaml = concat!(
            "name: Duplicate IDs\n",
            "trigger:\n  on: message_posted\n",
            "steps:\n",
            "  - id: step1\n    action: send_message\n    text: first\n",
            "  - id: step1\n    action: send_message\n    text: second\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        match &err {
            WorkflowError::InvalidDefinition(msg) => {
                assert!(msg.contains("duplicate"), "expected 'duplicate' in: {msg}");
            }
            other => panic!("expected InvalidDefinition, got: {other}"),
        }
    }

    #[test]
    fn validate_rejects_invalid_cron() {
        let yaml = "name: Bad Cron\ntrigger:\n  on: schedule\n  cron: not-a-cron\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    }

    #[test]
    fn validate_rejects_schedule_without_cron_or_interval() {
        let yaml = "name: Empty Schedule\ntrigger:\n  on: schedule\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    }

    #[test]
    fn enabled_defaults_to_true() {
        let yaml = "name: Test\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 1m\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert!(def.enabled);
    }

    #[test]
    fn enabled_can_be_set_false() {
        let yaml = "name: Disabled\nenabled: false\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 1m\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert!(!def.enabled);
    }

    #[test]
    fn parse_missing_optional_description_defaults_to_none() {
        let yaml = "name: No Desc\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 1m\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert!(def.description.is_none());
    }

    #[test]
    fn parse_explicit_description_is_present() {
        let yaml = "name: With Desc\ndescription: 'A helpful description'\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 1m\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.description.as_deref(), Some("A helpful description"));
    }

    #[test]
    fn parse_reaction_added_without_emoji_defaults_to_none() {
        // emoji is optional on ReactionAdded — omitting it means match any emoji.
        let yaml = "name: Any Reaction\ntrigger:\n  on: reaction_added\nsteps:\n  - id: s1\n    action: add_reaction\n    emoji: eyes\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.trigger {
            TriggerDef::ReactionAdded { emoji } => {
                assert!(emoji.is_none(), "emoji should default to None");
            }
            other => panic!("unexpected trigger: {other:?}"),
        }
    }

    #[test]
    fn parse_message_posted_without_filter_defaults_to_none() {
        let yaml = "name: All Messages\ntrigger:\n  on: message_posted\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.trigger {
            TriggerDef::MessagePosted { filter } => {
                assert!(filter.is_none(), "filter should default to None");
            }
            other => panic!("unexpected trigger: {other:?}"),
        }
    }

    #[test]
    fn parse_schedule_with_interval_instead_of_cron() {
        let yaml = "name: Interval Schedule\ntrigger:\n  on: schedule\n  interval: 30m\nsteps:\n  - id: s1\n    action: send_message\n    text: tick\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.trigger {
            TriggerDef::Schedule { cron, interval } => {
                assert!(cron.is_none());
                assert_eq!(interval.as_deref(), Some("30m"));
            }
            other => panic!("unexpected trigger: {other:?}"),
        }
    }

    #[test]
    fn parse_step_without_optional_name_defaults_to_none() {
        let yaml = "name: Test\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 5s\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert!(def.steps[0].name.is_none());
    }

    #[test]
    fn parse_step_with_optional_name() {
        let yaml = concat!(
            "name: Test\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: s1\n    name: 'Wait a bit'\n    action: delay\n    duration: 5s\n"
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.steps[0].name.as_deref(), Some("Wait a bit"));
    }

    #[test]
    fn parse_step_without_if_expr_defaults_to_none() {
        let yaml = "name: Test\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 5s\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert!(def.steps[0].if_expr.is_none());
    }

    #[test]
    fn parse_step_without_timeout_defaults_to_none() {
        let yaml = "name: Test\ntrigger:\n  on: webhook\nsteps:\n  - id: s1\n    action: delay\n    duration: 5s\n";
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert!(def.steps[0].timeout_secs.is_none());
    }

    #[test]
    fn parse_step_with_timeout_secs() {
        let yaml = concat!(
            "name: Test\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: s1\n    timeout_secs: 120\n    action: delay\n    duration: 5s\n"
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.steps[0].timeout_secs, Some(120));
    }

    #[test]
    fn parse_call_webhook_with_all_optional_fields() {
        let yaml = concat!(
            "name: Full Webhook\ntrigger:\n  on: webhook\n",
            "steps:\n",
            "  - id: call\n    action: call_webhook\n",
            "    url: https://example.com/hook\n",
            "    method: PUT\n",
            "    headers:\n      Authorization: 'Bearer token123'\n      Content-Type: application/json\n",
            "    body: '{\"key\": \"value\"}'\n",
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.steps[0].action {
            ActionDef::CallWebhook {
                url,
                method,
                headers,
                body,
            } => {
                assert_eq!(url, "https://example.com/hook");
                assert_eq!(method.as_deref(), Some("PUT"));
                let hdrs = headers.as_ref().expect("headers should be present");
                assert_eq!(
                    hdrs.get("Authorization").map(|s| s.as_str()),
                    Some("Bearer token123")
                );
                assert!(body.is_some());
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn parse_call_webhook_minimal_only_url() {
        let yaml = concat!(
            "name: Min Webhook\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: call\n    action: call_webhook\n    url: https://example.com/hook\n",
        );
        let (def, _) = parse_yaml(yaml).expect("parse failed");
        match &def.steps[0].action {
            ActionDef::CallWebhook {
                url,
                method,
                headers,
                body,
            } => {
                assert_eq!(url, "https://example.com/hook");
                assert!(method.is_none());
                assert!(headers.is_none());
                assert!(body.is_none());
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_yaml_returns_error() {
        let yaml = "name: [unclosed bracket\ntrigger:\n  on: message_posted\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidYaml(_)),
            "expected InvalidYaml, got: {err}"
        );
    }

    #[test]
    fn parse_yaml_with_unknown_trigger_type_returns_error() {
        // Unknown trigger `on:` value should fail deserialization.
        let yaml = "name: Bad Trigger\ntrigger:\n  on: unknown_trigger_type\nsteps:\n  - id: s1\n    action: delay\n    duration: 1m\n";
        let err = parse_yaml(yaml).unwrap_err();
        // serde_yaml will return an InvalidYaml error for unknown enum variant.
        assert!(
            matches!(
                err,
                WorkflowError::InvalidYaml(_) | WorkflowError::InvalidDefinition(_)
            ),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn parse_yaml_with_unknown_action_type_returns_error() {
        let yaml = concat!(
            "name: Bad Action\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: s1\n    action: fly_to_moon\n    destination: moon\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        assert!(
            matches!(
                err,
                WorkflowError::InvalidYaml(_) | WorkflowError::InvalidDefinition(_)
            ),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn canonical_json_round_trips_all_fields() {
        let yaml = concat!(
            "name: 'Full Round Trip'\n",
            "description: 'Tests all fields'\n",
            "enabled: true\n",
            "trigger:\n  on: message_posted\n  filter: 'str_contains(trigger_text, \"alert\")'\n",
            "steps:\n",
            "  - id: notify\n    name: 'Send Alert'\n    timeout_secs: 60\n",
            "    if: 'str_len(trigger_text) > 5'\n",
            "    action: send_message\n    text: 'Alert: {{trigger.text}}'\n",
        );
        let (def, json) = parse_yaml(yaml).expect("parse failed");

        let reparsed: WorkflowDef = serde_json::from_str(&json).expect("json round-trip");

        assert_eq!(reparsed.name, def.name);
        assert_eq!(reparsed.description, def.description);
        assert_eq!(reparsed.enabled, def.enabled);
        assert_eq!(reparsed.steps.len(), def.steps.len());
        assert_eq!(reparsed.steps[0].id, def.steps[0].id);
        assert_eq!(reparsed.steps[0].name, def.steps[0].name);
        assert_eq!(reparsed.steps[0].timeout_secs, def.steps[0].timeout_secs);
        assert_eq!(reparsed.steps[0].if_expr, def.steps[0].if_expr);
    }

    #[test]
    fn validate_rejects_whitespace_only_name() {
        let yaml =
            "name: '   '\ntrigger:\n  on: message_posted\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidDefinition(_)),
            "expected InvalidDefinition for whitespace-only name, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_empty_step_id() {
        let yaml = concat!(
            "name: Empty Step ID\ntrigger:\n  on: message_posted\n",
            "steps:\n  - id: ''\n    action: send_message\n    text: hi\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    }

    #[test]
    fn validate_rejects_whitespace_only_step_id() {
        let yaml = concat!(
            "name: Whitespace Step ID\ntrigger:\n  on: message_posted\n",
            "steps:\n  - id: '  '\n    action: send_message\n    text: hi\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    }

    #[test]
    fn validate_accepts_valid_5_field_cron() {
        // Standard 5-field cron: min hour dom month dow
        let yaml = "name: Cron5\ntrigger:\n  on: schedule\n  cron: '0 9 * * 1-5'\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        assert!(parse_yaml(yaml).is_ok(), "5-field cron should be valid");
    }

    #[test]
    fn validate_accepts_valid_6_field_cron() {
        // 6-field cron: sec min hour dom month dow
        let yaml = "name: Cron6\ntrigger:\n  on: schedule\n  cron: '0 0 9 * * 1-5'\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        assert!(parse_yaml(yaml).is_ok(), "6-field cron should be valid");
    }

    #[test]
    fn validate_accepts_valid_7_field_cron() {
        // 7-field cron: sec min hour dom month dow year
        let yaml = "name: Cron7\ntrigger:\n  on: schedule\n  cron: '0 0 9 * * 1-5 *'\nsteps:\n  - id: s1\n    action: send_message\n    text: hi\n";
        assert!(parse_yaml(yaml).is_ok(), "7-field cron should be valid");
    }

    #[test]
    fn validate_rejects_three_duplicate_step_ids() {
        let yaml = concat!(
            "name: Triple Duplicate\ntrigger:\n  on: message_posted\n",
            "steps:\n",
            "  - id: step1\n    action: send_message\n    text: first\n",
            "  - id: step1\n    action: send_message\n    text: second\n",
            "  - id: step1\n    action: send_message\n    text: third\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        match &err {
            WorkflowError::InvalidDefinition(msg) => {
                assert!(msg.contains("duplicate"), "expected 'duplicate' in: {msg}");
            }
            other => panic!("expected InvalidDefinition, got: {other}"),
        }
    }

    #[test]
    fn validate_accepts_multiple_steps_with_unique_ids() {
        let yaml = concat!(
            "name: Multi Step\ntrigger:\n  on: message_posted\n",
            "steps:\n",
            "  - id: step1\n    action: send_message\n    text: first\n",
            "  - id: step2\n    action: send_message\n    text: second\n",
            "  - id: step3\n    action: send_message\n    text: third\n",
        );
        let (def, _) = parse_yaml(yaml).expect("unique step IDs should be valid");
        assert_eq!(def.steps.len(), 3);
    }

    #[test]
    fn step_id_validation_rejects_dashes() {
        // Step ID with dash would cause evalexpr to interpret as subtraction:
        // `steps_my-step_output_field` → `steps_my` minus `step_output_field`
        let yaml = concat!(
            "name: Dash Step\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: my-step\n    action: send_message\n    text: hi\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        match &err {
            WorkflowError::InvalidDefinition(msg) => {
                assert!(
                    msg.contains("my-step"),
                    "error message should mention the invalid id, got: {msg}"
                );
            }
            other => panic!("expected InvalidDefinition, got: {other}"),
        }
    }

    #[test]
    fn step_id_validation_accepts_underscores() {
        // Underscores are safe in evalexpr variable names.
        let yaml = concat!(
            "name: Underscore Step\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: my_step\n    action: send_message\n    text: hi\n",
        );
        let (def, _) = parse_yaml(yaml).expect("underscore step id should be valid");
        assert_eq!(def.steps[0].id, "my_step");
    }

    #[test]
    fn step_id_validation_rejects_special_chars() {
        // Special characters (semicolons, spaces, etc.) must be rejected.
        let yaml = concat!(
            "name: Special Chars\ntrigger:\n  on: webhook\n",
            "steps:\n  - id: 'step;drop table'\n    action: send_message\n    text: hi\n",
        );
        let err = parse_yaml(yaml).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidDefinition(_)),
            "expected InvalidDefinition for step id with special chars, got: {err}"
        );
    }

    #[test]
    fn normalize_cron_5_fields_prepends_sec_appends_year() {
        let result = normalize_cron("0 9 * * 1-5");
        assert_eq!(result, "0 0 9 * * 1-5 *");
    }

    #[test]
    fn normalize_cron_6_fields_appends_year() {
        let result = normalize_cron("0 0 9 * * 1-5");
        assert_eq!(result, "0 0 9 * * 1-5 *");
    }

    #[test]
    fn normalize_cron_7_fields_unchanged() {
        let result = normalize_cron("0 0 9 * * 1-5 *");
        assert_eq!(result, "0 0 9 * * 1-5 *");
    }

    #[test]
    fn normalize_cron_every_minute_5_fields() {
        let result = normalize_cron("* * * * *");
        assert_eq!(result, "0 * * * * * *");
    }

    #[test]
    fn validate_rejects_sub_minute_interval() {
        let yaml = "name: Too Fast\ntrigger:\n  on: schedule\n  interval: 30s\nsteps:\n  - id: s1\n    action: send_message\n    text: tick\n";
        let err = parse_yaml(yaml).unwrap_err();
        match &err {
            WorkflowError::InvalidDefinition(msg) => {
                assert!(
                    msg.contains("60s") || msg.contains("60 seconds"),
                    "error should mention 60s minimum, got: {msg}"
                );
            }
            other => panic!("expected InvalidDefinition, got: {other}"),
        }
    }

    #[test]
    fn validate_rejects_sub_minute_interval_59s() {
        let yaml = "name: Too Fast\ntrigger:\n  on: schedule\n  interval: 59s\nsteps:\n  - id: s1\n    action: send_message\n    text: tick\n";
        let err = parse_yaml(yaml).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDefinition(_)));
    }

    #[test]
    fn validate_accepts_exactly_60s_interval() {
        let yaml = "name: Exactly 60s\ntrigger:\n  on: schedule\n  interval: 60s\nsteps:\n  - id: s1\n    action: send_message\n    text: tick\n";
        assert!(parse_yaml(yaml).is_ok(), "60s interval should be valid");
    }

    #[test]
    fn validate_accepts_interval_above_60s() {
        // 30m = 1800s, well above the 60s minimum.
        let yaml = "name: Interval Schedule\ntrigger:\n  on: schedule\n  interval: 30m\nsteps:\n  - id: s1\n    action: send_message\n    text: tick\n";
        assert!(parse_yaml(yaml).is_ok(), "30m interval should be valid");
    }

    #[test]
    fn diff_posted_trigger_roundtrips_yaml() {
        let yaml = "on: diff_posted\n";
        let trigger: TriggerDef = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(trigger, TriggerDef::DiffPosted { filter: None }));
        let back = serde_yaml::to_string(&trigger).unwrap();
        assert!(back.contains("diff_posted"));
    }

    #[test]
    fn diff_posted_trigger_with_filter_roundtrips_yaml() {
        let yaml = "on: diff_posted\nfilter: 'str_contains(trigger_text, \"src/\")'\n";
        let trigger: TriggerDef = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            trigger,
            TriggerDef::DiffPosted { filter: Some(_) }
        ));
    }

    const VALID_AGENT_PUBKEY: &str =
        "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
    const VALID_RESULT_CHANNEL: &str = "018f47a2-4b52-7de0-8c5b-92e5860f4851";

    fn invoke_agent_yaml(agent_pubkey: &str, prompt: &str, idempotency_key: &str) -> String {
        format!(
            "name: Agent Routine\ntrigger:\n  on: schedule\n  interval: 15m\nsteps:\n  - id: invoke\n    action: invoke_agent\n    agent_pubkey: '{agent_pubkey}'\n    prompt: '{prompt}'\n    result_channel: '{VALID_RESULT_CHANNEL}'\n    idempotency_key: '{idempotency_key}'\n"
        )
    }

    fn invoke_agent_cron_yaml(expr: &str) -> String {
        invoke_agent_yaml(VALID_AGENT_PUBKEY, "do work", "run-1")
            .replace("  interval: 15m", &format!("  cron: '{expr}'"))
    }

    #[test]
    fn invoke_agent_action_round_trips_exact_fields() {
        let yaml = invoke_agent_yaml(
            VALID_AGENT_PUBKEY,
            "Summarize {{trigger.timestamp}}",
            "daily-{{trigger.timestamp}}",
        );
        let (def, canonical_json) = parse_yaml(&yaml).expect("invoke_agent should parse");

        match &def.steps[0].action {
            ActionDef::InvokeAgent {
                agent_pubkey,
                prompt,
                result_channel,
                idempotency_key,
            } => {
                assert_eq!(agent_pubkey, VALID_AGENT_PUBKEY);
                assert_eq!(prompt, "Summarize {{trigger.timestamp}}");
                assert_eq!(result_channel, VALID_RESULT_CHANNEL);
                assert_eq!(idempotency_key, "daily-{{trigger.timestamp}}");
            }
            other => panic!("unexpected action: {other:?}"),
        }

        let value: serde_json::Value =
            serde_json::from_str(&canonical_json).expect("canonical JSON");
        let action = &value["steps"][0];
        assert_eq!(action["action"], "invoke_agent");
        for field in [
            "agent_pubkey",
            "prompt",
            "result_channel",
            "idempotency_key",
        ] {
            assert!(action.get(field).is_some(), "missing {field}");
        }

        let reparsed: WorkflowDef = serde_json::from_str(&canonical_json).expect("JSON round-trip");
        reparsed.validate().expect("round-trip should validate");
    }

    #[test]
    fn invoke_agent_requires_all_contract_fields() {
        let yaml = format!(
            "name: Missing Key\ntrigger:\n  on: schedule\n  interval: 15m\nsteps:\n  - id: invoke\n    action: invoke_agent\n    agent_pubkey: '{VALID_AGENT_PUBKEY}'\n    prompt: do work\n    result_channel: '{VALID_RESULT_CHANNEL}'\n"
        );
        assert!(matches!(
            parse_yaml(&yaml),
            Err(WorkflowError::InvalidYaml(_))
        ));
    }

    #[test]
    fn invoke_agent_rejects_unknown_fields_without_changing_legacy_decoding() {
        let invoke = invoke_agent_yaml(VALID_AGENT_PUBKEY, "do work", "run-1")
            .replace("    prompt:", "    authority: owner\n    prompt:");
        let err = parse_yaml(&invoke).expect_err("unknown invoke_agent field must fail");
        assert!(matches!(err, WorkflowError::InvalidYaml(_)));
        assert!(err.to_string().contains("authority"));

        let legacy = "name: Legacy\ntrigger:\n  on: webhook\nsteps:\n  - id: notify\n    action: send_message\n    text: hi\n    legacy_metadata: retained-by-author\n";
        parse_yaml(legacy).expect("legacy action unknown-field behavior must remain unchanged");
    }

    #[test]
    fn invoke_agent_rejects_invalid_target_and_result_channel() {
        let bad_pubkey = invoke_agent_yaml("not-a-pubkey", "do work", "run-1");
        let err = parse_yaml(&bad_pubkey).expect_err("invalid pubkey must fail");
        assert!(err.to_string().contains("agent_pubkey"));

        let uppercase_pubkey =
            invoke_agent_yaml(&VALID_AGENT_PUBKEY.to_ascii_uppercase(), "do work", "run-1");
        let err = parse_yaml(&uppercase_pubkey).expect_err("uppercase pubkey must fail");
        assert!(err.to_string().contains("canonical lowercase"));

        let bad_channel = invoke_agent_yaml(VALID_AGENT_PUBKEY, "do work", "run-1")
            .replace(VALID_RESULT_CHANNEL, "general");
        let err = parse_yaml(&bad_channel).expect_err("invalid result channel must fail");
        assert!(err.to_string().contains("result_channel"));

        let nil_channel = invoke_agent_yaml(VALID_AGENT_PUBKEY, "do work", "run-1")
            .replace(VALID_RESULT_CHANNEL, "00000000-0000-0000-0000-000000000000");
        let err = parse_yaml(&nil_channel).expect_err("nil result channel must fail");
        assert!(err.to_string().contains("non-nil"));
    }

    #[test]
    fn invoke_agent_rejects_empty_and_oversized_payload_fields() {
        for (prompt, key, expected_field) in [
            ("   ".to_owned(), "run-1".to_owned(), "prompt"),
            ("do work".to_owned(), "   ".to_owned(), "idempotency_key"),
            (
                "x".repeat(MAX_INVOKE_AGENT_PROMPT_BYTES + 1),
                "run-1".to_owned(),
                "prompt",
            ),
            (
                "do work".to_owned(),
                "x".repeat(MAX_INVOKE_AGENT_IDEMPOTENCY_KEY_BYTES + 1),
                "idempotency_key",
            ),
        ] {
            let yaml = invoke_agent_yaml(VALID_AGENT_PUBKEY, &prompt, &key);
            let err = parse_yaml(&yaml).expect_err("invalid field must fail");
            assert!(
                err.to_string().contains(expected_field),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn invoke_agent_requires_fifteen_minute_interval() {
        let too_frequent =
            invoke_agent_yaml(VALID_AGENT_PUBKEY, "do work", "run-1").replace("15m", "14m");
        let err = parse_yaml(&too_frequent).expect_err("14m invoke interval must fail");
        assert!(err.to_string().contains("at least 15m"));

        let exact = invoke_agent_yaml(VALID_AGENT_PUBKEY, "do work", "run-1");
        parse_yaml(&exact).expect("15m invoke interval should pass");

        let existing_action = "name: Existing\ntrigger:\n  on: schedule\n  interval: 1m\nsteps:\n  - id: notify\n    action: send_message\n    text: tick\n";
        parse_yaml(existing_action).expect("existing action interval semantics must not change");
    }

    #[test]
    fn invoke_agent_rejects_sub_fifteen_minute_cron_cadence() {
        for expr in ["* * * * *", "*/10 * * * *", "* * * * * *"] {
            let yaml = invoke_agent_cron_yaml(expr);
            let err = parse_yaml(&yaml).expect_err("fast invoke_agent cron must fail");
            assert!(
                err.to_string().contains("cadence must be at least 15m"),
                "unexpected error for {expr}: {err}"
            );
        }
    }

    #[test]
    fn invoke_agent_accepts_fifteen_minute_or_slower_cron_cadence() {
        for expr in ["*/15 * * * *", "0 * * * *", "@hourly"] {
            let yaml = invoke_agent_cron_yaml(expr);
            parse_yaml(&yaml).unwrap_or_else(|err| panic!("{expr} should be accepted: {err}"));
        }
    }

    #[test]
    fn invoke_agent_cron_checks_midnight_boundary_on_actual_dates() {
        // Cross-product produces 00:00, 00:55, 23:00, and 23:55. Same-day
        // gaps are safe, but consecutive all-days executions cross midnight
        // only five minutes apart.
        let every_day = invoke_agent_cron_yaml("0 0,55 0,23 * * * *");
        let err = parse_yaml(&every_day).expect_err("five-minute midnight gap must fail");
        assert!(err.to_string().contains("cadence must be at least 15m"));

        // Restricting the same times to Mondays removes the adjacent-date
        // pair, so its real consecutive occurrences are safely separated.
        let mondays = invoke_agent_cron_yaml("0 0,55 0,23 * * Mon *");
        parse_yaml(&mondays).expect("non-consecutive valid dates should not false-reject");
    }

    #[test]
    fn legacy_actions_keep_existing_fast_cron_behavior() {
        let yaml = "name: Existing\ntrigger:\n  on: schedule\n  cron: '* * * * *'\nsteps:\n  - id: notify\n    action: send_message\n    text: tick\n";
        parse_yaml(yaml).expect("invoke cadence gate must not affect existing actions");
    }
}
