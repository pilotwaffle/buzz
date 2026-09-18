//! Action sink trait — interface for workflow side-effects.
//!
//! The relay implements [`ActionSink`] to provide direct DB access to the
//! executor, replacing the HTTP loopback pattern.

use std::future::Future;
use std::pin::Pin;

use buzz_core::tenant::CommunityId;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Errors from action sink operations.
#[derive(Debug, thiserror::Error)]
pub enum ActionSinkError {
    /// An input parameter is malformed (e.g. invalid UUID).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The target channel does not exist.
    #[error("channel not found: {0}")]
    ChannelNotFound(String),
    /// The target channel is archived.
    #[error("channel is archived: {0}")]
    ChannelArchived(String),
    /// Nostr event construction or signing failed.
    #[error("event construction failed: {0}")]
    EventBuild(String),
    /// A database operation failed.
    #[error("database error: {0}")]
    Database(String),
    /// Message content is empty or whitespace-only.
    #[error("empty message content")]
    EmptyContent,
    /// The routine's target agent is not owned by the workflow owner in the
    /// destination channel (or is not a member of it).
    #[error("agent not allowed: {0}")]
    AgentNotAllowed(String),
}

impl From<ActionSinkError> for crate::WorkflowError {
    fn from(e: ActionSinkError) -> Self {
        crate::WorkflowError::WebhookError(e.to_string())
    }
}

/// Interface for workflow actions that produce side effects.
///
/// Implemented by the relay to provide direct DB/event access to the executor.
/// This replaces the HTTP loopback where the executor POSTed to the relay's
/// REST API (which failed with 401 auth errors).
///
/// Returns `Pin<Box<dyn Future>>` for dyn-compatibility — required because
/// `WorkflowEngine` stores `Arc<dyn ActionSink>`.
pub trait ActionSink: Send + Sync {
    /// Post a message to a channel on behalf of a workflow owner.
    ///
    /// - `community_id`: the server-resolved community that owns the workflow
    ///   run driving this side effect. The relay-signed message is published
    ///   under *this* community, never the deployment/default tenant — the run
    ///   carries its owning community so a workflow in community B posts into B
    ///   even though the side effect has no inbound connection to bind.
    /// - `channel_id`: UUID string of the target channel
    /// - `text`: rendered message body (must not be empty/whitespace-only)
    /// - `authored_text`: the workflow owner's stored, unrendered step template;
    ///   consumers must use this rather than trigger-controlled rendered output
    ///   when attaching authority-bearing metadata
    /// - `author_pubkey`: hex-encoded pubkey of the workflow owner (used for
    ///   the `p` attribution tag; the relay keypair signs the event)
    /// - `reply_to`: when `Some(event_id_hex)`, the message is posted as a
    ///   threaded reply to that event (NIP-10 root/reply tags + real thread
    ///   metadata); when `None`, it is a top-level channel message.
    ///
    /// Returns the event ID hex string on success.
    fn send_message(
        &self,
        community_id: CommunityId,
        channel_id: &str,
        text: &str,
        authored_text: &str,
        author_pubkey: &str,
        reply_to: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, ActionSinkError>> + Send + '_>>;

    /// Dispatch an `invoke_agent` routine wake to its target agent.
    ///
    /// Idempotent on `(community_id, workflow_id, idempotency_key)`: a second
    /// call for an already-dispatched key returns `Deduplicated` without
    /// posting a second wake event.
    fn invoke_agent(
        &self,
        request: InvokeAgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<InvokeAgentOutcome, ActionSinkError>> + Send + '_>>;
}

/// Request to dispatch a routine's wake event to its target agent.
#[derive(Debug, Clone)]
pub struct InvokeAgentRequest {
    /// Server-resolved community that owns the workflow run.
    pub community_id: CommunityId,
    /// The workflow run this dispatch belongs to.
    pub run_id: Uuid,
    /// The workflow (routine) definition id.
    pub workflow_id: Uuid,
    /// Hex-encoded pubkey of the workflow owner.
    pub owner_pubkey_hex: String,
    /// Hex-encoded pubkey of the target agent.
    pub agent_pubkey_hex: String,
    /// Channel that will receive the wake and, later, the outcome event.
    pub result_channel: Uuid,
    /// Rendered prompt text for this run.
    pub prompt: String,
    /// Resolved idempotency key for this run.
    pub idempotency_key: String,
    /// Token budget for this single run.
    pub token_budget_per_run: u64,
    /// Token budget for this routine per UTC day.
    pub token_budget_per_day: u64,
    /// The schedule instant this run was fired for.
    pub fire_instant: DateTime<Utc>,
}

/// Result of an `invoke_agent` dispatch attempt.
#[derive(Debug, Clone)]
pub enum InvokeAgentOutcome {
    /// A new wake event was published.
    Dispatched {
        /// Hex event id of the published wake event.
        wake_event_id: String,
    },
    /// The idempotency key was already dispatched; no new wake was posted.
    Deduplicated,
    /// The workflow already has an open (unsettled) dispatch; this fire's
    /// claim was consumed without dispatching a new wake.
    SkippedBusy,
}
