//! Strict, zero-I/O contracts for agent controls carried by NIP-AO.
//!
//! This module freezes the decrypted payloads and the state-machine seams used
//! by Slice 2. It deliberately does not send events, touch a queue, or persist
//! claims. Callers validate signed NIP-AO evidence here, perform the documented
//! durable transaction, then feed that transaction's authoritative result to
//! the classifiers below.

use nostr::{nips::nip44, Event, EventId, Keys, PublicKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::kind::KIND_AGENT_OBSERVER_FRAME;
use crate::observer::{
    content_looks_like_nip44, OBSERVER_AGENT_TAG, OBSERVER_FRAME_CONTROL, OBSERVER_FRAME_TAG,
    OBSERVER_FRAME_TELEMETRY,
};
use crate::{verify_event, CommunityId};

/// Wire discriminator for cancel and steer commands.
pub const COMMAND_FORMAT: &str = "buzz-agent-control-command";
/// Wire discriminator for cancel and steer acknowledgements.
pub const COMMAND_ACK_FORMAT: &str = "buzz-agent-control-ack";
/// Wire discriminator for pause-lease transitions.
pub const PAUSE_LEASE_FORMAT: &str = "buzz-agent-pause-lease";
/// Wire discriminator for pause-lease acknowledgements.
pub const PAUSE_LEASE_ACK_FORMAT: &str = "buzz-agent-pause-lease-ack";
/// Current structured-control contract version.
pub const VERSION: u32 = 1;
/// Default pause duration specified by the product contract.
pub const DEFAULT_PAUSE_LEASE_SECS: u64 = 300;
/// Maximum validity window for a delivered control mutation.
pub const MAX_TRANSITION_TTL_SECS: u64 = 300;
/// Hard maximum pause lease duration.
pub const MAX_PAUSE_LEASE_SECS: u64 = 3_600;
/// Maximum decrypted JSON accepted by the strict structured-control parser.
pub const MAX_CONTROL_PLAINTEXT_BYTES: usize = 16_384;
/// Maximum UTF-8 byte length of a display-only acknowledgement excerpt.
pub const MAX_ACK_EXCERPT_BYTES: usize = 512;
/// Maximum UTF-8 byte length of one readable Live Activity output excerpt.
pub const MAX_ACTIVITY_EXCERPT_BYTES: usize = 4_096;
/// Maximum UTF-8 byte length of an opaque computer or run identifier.
pub const MAX_OPAQUE_ID_BYTES: usize = 128;

const COMMAND_HASH_DOMAIN: &[u8] = b"buzz-agent-control/command/v1\0";
const LEASE_HASH_DOMAIN: &[u8] = b"buzz-agent-control/pause-lease/v1\0";

/// Exact destination and execution context bound into every control payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlTarget {
    /// Stable identifier of the computer hosting the agent.
    pub computer_id: String,
    /// Hex Nostr public key of the controlled agent.
    pub agent_pubkey: String,
    /// Buzz channel whose active work the operator selected.
    pub channel_id: Uuid,
    /// Opaque harness run identifier visible when the control was issued.
    pub run_id: String,
}

impl ControlTarget {
    fn validate(&self) -> Result<(), AgentControlError> {
        validate_opaque_id("computer_id", &self.computer_id)?;
        parse_pubkey("agent_pubkey", &self.agent_pubkey)?;
        if self.channel_id.is_nil() {
            return Err(AgentControlError::InvalidField("channel_id"));
        }
        validate_opaque_id("run_id", &self.run_id)
    }
}

/// One-shot operations. Neither variant creates renewable authority or state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OneShotControlKind {
    /// Stop the selected in-flight turn, if one still exists.
    Cancel,
    /// Deliver an already-durable operator message through ACP steering/fallback.
    Steer,
}

/// Strict decrypted NIP-AO payload for a cancel or steer command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OneShotControlCommand {
    /// Always [`COMMAND_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Caller-selected replay key, unique within the server-resolved tenant.
    pub command_id: Uuid,
    /// Cancel or steer.
    pub control: OneShotControlKind,
    /// Operator identity duplicated inside the ciphertext and matched to signer.
    pub operator_pubkey: String,
    /// Exact agent/computer/channel/run binding.
    pub target: ControlTarget,
    /// Non-zero monotonic operator-to-agent sequence for gap diagnostics.
    pub seq: u64,
    /// Signed issue time in Unix seconds; equals the outer event timestamp.
    pub issued_at: u64,
    /// Command deadline. `now >= expires_at` is expired.
    pub expires_at: u64,
    /// Durable operator-message event for steer; absent for cancel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_message_event_id: Option<String>,
}

impl OneShotControlCommand {
    /// Validate closed-schema fields without trusting any wire identity.
    pub fn validate_shape(&self) -> Result<(), AgentControlError> {
        validate_discriminator(&self.format, COMMAND_FORMAT, self.version)?;
        if self.command_id.is_nil() {
            return Err(AgentControlError::InvalidField("command_id"));
        }
        parse_pubkey("operator_pubkey", &self.operator_pubkey)?;
        self.target.validate()?;
        if self.seq == 0 {
            return Err(AgentControlError::InvalidField("seq"));
        }
        validate_transition_times(self.issued_at, self.expires_at)?;
        match (self.control, self.steer_message_event_id.as_deref()) {
            (OneShotControlKind::Cancel, None) => Ok(()),
            (OneShotControlKind::Steer, Some(event_id)) => {
                parse_event_id("steer_message_event_id", event_id)?;
                Ok(())
            }
            (OneShotControlKind::Cancel, Some(_)) => {
                Err(AgentControlError::InvalidField("steer_message_event_id"))
            }
            (OneShotControlKind::Steer, None) => Err(AgentControlError::MissingSteerMessage),
        }
    }
}

/// Trusted projection of the durable operator message referenced by a steer.
///
/// The runtime constructs this only after loading the event from the current
/// tenant. No message body is carried in the control payload or this projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSteerMessage {
    /// Server-resolved tenant from which the event was loaded.
    pub community_id: CommunityId,
    /// Loaded event id.
    pub event_id: String,
    /// Verified event author.
    pub operator_pubkey: String,
    /// Server-resolved channel containing the message.
    pub channel_id: Uuid,
    /// Signed event timestamp.
    pub created_at: u64,
}

/// Trusted runtime facts used to bind a decrypted control payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedControlFacts {
    /// Server-resolved tenant; never accepted from payload JSON.
    pub community_id: CommunityId,
    /// Transaction-independent validation time in Unix seconds.
    pub now: u64,
    /// Currently resolved owner/operator of the target agent.
    pub operator_pubkey: String,
    /// Monotonic revision of the target agent's owner/visibility binding.
    /// Every transition, including A -> B -> A, must advance this value.
    pub agent_ownership_revision: u64,
    /// Current destination selected by the runtime.
    pub target: ControlTarget,
    /// Durable message projection required only by steer.
    pub steer_message: Option<ResolvedSteerMessage>,
}

/// Bounded, display-only detail. It is never command authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlAckExcerpt {
    /// Redacted text safe for operator display.
    pub text: String,
    /// Whether the source was truncated to fit the byte limit.
    pub truncated: bool,
}

impl ControlAckExcerpt {
    /// Validate byte bound and reject non-layout control characters.
    pub fn validate(&self) -> Result<(), AgentControlError> {
        if self.text.len() > MAX_ACK_EXCERPT_BYTES {
            return Err(AgentControlError::ExcerptTooLarge);
        }
        if self
            .text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            return Err(AgentControlError::InvalidField("detail"));
        }
        Ok(())
    }
}

/// Bounded output text admitted to the readable Live Activity timeline.
///
/// This is a display projection, not the raw ACP frame. `truncated=true`
/// directs the client to offer locally retained detail only when that optional
/// archive is enabled; it never authorizes a relay history fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveActivityExcerpt {
    /// Redacted UTF-8 text for immediate display.
    pub text: String,
    /// Whether additional source text was omitted.
    pub truncated: bool,
}

impl LiveActivityExcerpt {
    /// Validate the UTF-8 byte bound and safe layout characters.
    pub fn validate(&self) -> Result<(), AgentControlError> {
        if self.text.len() > MAX_ACTIVITY_EXCERPT_BYTES {
            return Err(AgentControlError::ActivityExcerptTooLarge);
        }
        if self
            .text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            return Err(AgentControlError::InvalidField("activity_excerpt"));
        }
        Ok(())
    }
}

/// Stable one-shot acknowledgement outcomes surfaced to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAckStatus {
    /// The selected turn was cancelled or the steer reached the live session.
    Applied,
    /// No matching in-flight turn remained when cancel was applied.
    NoActiveTurn,
    /// The durable steer message will enter the normal queue path.
    Queued,
    /// The agent refused the command without applying it.
    Rejected,
}

/// Machine-readable reasons for a rejected acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAckReason {
    /// Signed and current runtime bindings did not match.
    BindingMismatch,
    /// The selected adapter could not support the operation.
    Unsupported,
    /// The operation failed after validation.
    InternalError,
}

/// Agent-signed acknowledgement for a cancel or steer command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OneShotControlAck {
    /// Always [`COMMAND_ACK_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Unique acknowledgement id.
    pub ack_id: Uuid,
    /// Exact acknowledged command id.
    pub command_id: Uuid,
    /// Tenant-bound command fingerprint returned by validation.
    pub command_fingerprint: String,
    /// Exact acknowledged operation.
    pub control: OneShotControlKind,
    /// Exact operator identity from the command.
    pub operator_pubkey: String,
    /// Exact destination from the command.
    pub target: ControlTarget,
    /// Original operator sequence.
    pub command_seq: u64,
    /// Non-zero monotonic agent-to-owner acknowledgement sequence.
    pub seq: u64,
    /// Signed acknowledgement time; equals the outer event timestamp.
    pub acked_at: u64,
    /// Stable outcome for UI and audit.
    pub status: ControlAckStatus,
    /// Present exactly when `status` is `rejected`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ControlAckReason>,
    /// Optional bounded, redacted operator-facing detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ControlAckExcerpt>,
}

impl OneShotControlAck {
    fn validate_shape(&self) -> Result<(), AgentControlError> {
        validate_discriminator(&self.format, COMMAND_ACK_FORMAT, self.version)?;
        if self.ack_id.is_nil() || self.command_id.is_nil() {
            return Err(AgentControlError::InvalidField("ack_id"));
        }
        validate_hash("command_fingerprint", &self.command_fingerprint)?;
        parse_pubkey("operator_pubkey", &self.operator_pubkey)?;
        self.target.validate()?;
        if self.command_seq == 0 || self.seq == 0 || self.acked_at == 0 {
            return Err(AgentControlError::InvalidField("seq"));
        }
        if matches!(self.status, ControlAckStatus::Rejected) != self.reason.is_some() {
            return Err(AgentControlError::InvalidField("reason"));
        }
        if let Some(detail) = &self.detail {
            detail.validate()?;
        }
        Ok(())
    }
}

/// A pause-lease state transition. Resume is a release, never a second lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseLeaseTransitionKind {
    /// Acquire a new queue hold.
    Pause,
    /// Extend the expiry of an active queue hold.
    Renew,
    /// Release the named queue hold.
    Resume,
}

/// Strict decrypted payload for a persistent pause-lease mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseLeaseTransition {
    /// Always [`PAUSE_LEASE_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Unique id of this mutation attempt.
    pub transition_id: Uuid,
    /// Stable lease identity across pause, renew, and resume.
    pub lease_id: Uuid,
    /// Starts at one for pause and increments by one per state transition.
    pub generation: u64,
    /// Requested lease state transition.
    pub transition: PauseLeaseTransitionKind,
    /// Operator identity duplicated inside ciphertext and matched to signer.
    pub operator_pubkey: String,
    /// Exact destination and provenance of this mutation.
    pub target: ControlTarget,
    /// Non-zero monotonic operator-to-agent sequence.
    pub seq: u64,
    /// Signed issue time; equals the outer event timestamp.
    pub issued_at: u64,
    /// Delivery deadline for this transition, never the pause expiry.
    pub transition_expires_at: u64,
    /// Required for pause/renew and forbidden for resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<u64>,
}

/// Closed set of owner-to-agent structured-control payloads.
///
/// The distinct required fields and format discriminators make the untagged
/// representation unambiguous while preserving the flat wire contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OwnerControlPayload {
    /// A cancel or steer command.
    OneShot(OneShotControlCommand),
    /// A pause, renew, or resume lease transition.
    PauseLease(PauseLeaseTransition),
}

impl PauseLeaseTransition {
    /// Validate the closed-schema transition independently of stored lease state.
    pub fn validate_shape(&self) -> Result<(), AgentControlError> {
        validate_discriminator(&self.format, PAUSE_LEASE_FORMAT, self.version)?;
        if self.transition_id.is_nil() || self.lease_id.is_nil() {
            return Err(AgentControlError::InvalidField("lease_id"));
        }
        parse_pubkey("operator_pubkey", &self.operator_pubkey)?;
        self.target.validate()?;
        if self.seq == 0 {
            return Err(AgentControlError::InvalidField("seq"));
        }
        validate_transition_times(self.issued_at, self.transition_expires_at)?;
        match (self.transition, self.generation, self.lease_expires_at) {
            (PauseLeaseTransitionKind::Pause, 1, Some(lease_expires_at)) => {
                validate_lease_expiry(self.issued_at, lease_expires_at)
            }
            (PauseLeaseTransitionKind::Renew, generation, Some(lease_expires_at))
                if generation > 1 =>
            {
                validate_lease_expiry(self.issued_at, lease_expires_at)
            }
            (PauseLeaseTransitionKind::Resume, generation, None) if generation > 1 => Ok(()),
            _ => Err(AgentControlError::InvalidLeaseShape),
        }
    }
}

/// Durable lease projection loaded from the current tenant's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPauseLease {
    /// Server-resolved tenant of the row.
    pub community_id: CommunityId,
    /// Stable lease id.
    pub lease_id: Uuid,
    /// Current owner/operator binding.
    pub operator_pubkey: String,
    /// Non-zero owner/visibility revision captured when this lease state was applied.
    pub agent_ownership_revision: u64,
    /// Complete original computer/agent/channel/run mutation binding.
    pub target: ControlTarget,
    /// Last durably applied generation.
    pub generation: u64,
    /// Whether the durable state currently says the queue is held.
    pub active: bool,
    /// Signed lease deadline retained even after release.
    pub lease_expires_at: u64,
    /// Id of the last durably applied transition.
    pub last_transition_id: Uuid,
    /// Tenant-bound fingerprint of the last applied transition.
    pub last_transition_fingerprint: String,
}

impl ResolvedPauseLease {
    fn validate(&self) -> Result<(), AgentControlError> {
        if self.lease_id.is_nil()
            || self.last_transition_id.is_nil()
            || self.generation == 0
            || self.agent_ownership_revision == 0
            || self.lease_expires_at == 0
        {
            return Err(AgentControlError::LeaseConflict);
        }
        parse_pubkey("operator_pubkey", &self.operator_pubkey)?;
        self.target.validate()?;
        validate_hash(
            "last_transition_fingerprint",
            &self.last_transition_fingerprint,
        )
    }
}

/// Effective queue state reported in a pause-lease acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueHoldState {
    /// Queue dispatch is held at safe boundaries.
    Paused,
    /// Queue dispatch is allowed.
    Running,
}

/// Stable pause-lease acknowledgement outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseLeaseAckStatus {
    /// The requested state transition was durably applied.
    Applied,
    /// An exact retry observed the already-applied durable transition.
    AlreadyApplied,
    /// The mutation was rejected without changing durable state.
    Rejected,
}

/// Agent-signed acknowledgement for a pause, renew, or resume transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseLeaseAck {
    /// Always [`PAUSE_LEASE_ACK_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Unique acknowledgement id.
    pub ack_id: Uuid,
    /// Exact transition id being acknowledged.
    pub transition_id: Uuid,
    /// Tenant-bound transition fingerprint.
    pub transition_fingerprint: String,
    /// Exact lease identity.
    pub lease_id: Uuid,
    /// Exact requested generation.
    pub generation: u64,
    /// Exact requested mutation.
    pub transition: PauseLeaseTransitionKind,
    /// Exact operator identity from the request.
    pub operator_pubkey: String,
    /// Exact target from the request.
    pub target: ControlTarget,
    /// Original operator sequence.
    pub transition_seq: u64,
    /// Non-zero monotonic agent-to-owner sequence.
    pub seq: u64,
    /// Signed acknowledgement timestamp.
    pub acked_at: u64,
    /// Mutation result.
    pub status: PauseLeaseAckStatus,
    /// Effective durable queue state after classification.
    pub queue_state: QueueHoldState,
    /// Optional bounded, redacted detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ControlAckExcerpt>,
}

impl PauseLeaseAck {
    fn validate_shape(&self) -> Result<(), AgentControlError> {
        validate_discriminator(&self.format, PAUSE_LEASE_ACK_FORMAT, self.version)?;
        if self.ack_id.is_nil()
            || self.transition_id.is_nil()
            || self.lease_id.is_nil()
            || self.generation == 0
            || self.transition_seq == 0
            || self.seq == 0
            || self.acked_at == 0
        {
            return Err(AgentControlError::InvalidField("ack"));
        }
        validate_hash("transition_fingerprint", &self.transition_fingerprint)?;
        parse_pubkey("operator_pubkey", &self.operator_pubkey)?;
        self.target.validate()?;
        let expected_state = match self.transition {
            PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => {
                QueueHoldState::Paused
            }
            PauseLeaseTransitionKind::Resume => QueueHoldState::Running,
        };
        if !matches!(self.status, PauseLeaseAckStatus::Rejected)
            && self.queue_state != expected_state
        {
            return Err(AgentControlError::AckBindingMismatch);
        }
        if let Some(detail) = &self.detail {
            detail.validate()?;
        }
        Ok(())
    }
}

/// Stable failures returned by structured-control parsing and validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentControlError {
    /// JSON did not match the closed schema.
    #[error("invalid agent-control schema: {0}")]
    InvalidSchema(String),
    /// Decrypted JSON exceeded the stricter structured-control cap.
    #[error("agent-control plaintext exceeds {MAX_CONTROL_PLAINTEXT_BYTES} bytes")]
    PlaintextTooLarge,
    /// Wire discriminator was not recognized.
    #[error("invalid agent-control format")]
    InvalidFormat,
    /// Wire version is unsupported.
    #[error("unsupported agent-control version")]
    UnsupportedVersion,
    /// A named field was malformed.
    #[error("invalid agent-control field: {0}")]
    InvalidField(&'static str),
    /// Timestamps were reversed, zero, or exceeded the control TTL.
    #[error("invalid agent-control timestamp")]
    InvalidTimestamp,
    /// The transition reached or passed its signed deadline.
    #[error("agent-control transition expired")]
    Expired,
    /// Outer Nostr event id/signature verification failed.
    #[error("invalid agent-control event signature")]
    InvalidEventSignature,
    /// Outer NIP-AO kind, tag, direction, or ciphertext shape was invalid.
    #[error("invalid agent-control event envelope: {0}")]
    InvalidEventEnvelope(String),
    /// NIP-44 decryption failed before any command was considered.
    #[error("agent-control payload decryption failed")]
    DecryptionFailed,
    /// Payload/event/facts operator identities disagreed.
    #[error("agent-control operator binding mismatch")]
    OperatorMismatch,
    /// Owner/visibility authority changed between validation and the effect.
    #[error("agent-control authority revision changed")]
    AuthorityConflict,
    /// Payload/event/facts target agents disagreed.
    #[error("agent-control agent binding mismatch")]
    AgentMismatch,
    /// The selected computer did not match runtime facts.
    #[error("agent-control computer binding mismatch")]
    ComputerMismatch,
    /// The selected channel did not match runtime facts.
    #[error("agent-control channel binding mismatch")]
    ChannelMismatch,
    /// The selected run did not match runtime facts.
    #[error("agent-control run binding mismatch")]
    RunMismatch,
    /// A steer omitted the durable message evidence.
    #[error("steer requires a durable operator message")]
    MissingSteerMessage,
    /// Durable steer-message evidence did not match the command.
    #[error("steer message binding mismatch")]
    SteerMessageMismatch,
    /// A spent command id was reused with conflicting binding data.
    #[error("agent-control command replay")]
    CommandReplay,
    /// Durable command/lease state could not be proven.
    #[error("agent-control state store unavailable")]
    StoreUnavailable,
    /// A signed acknowledgement did not bind to its request.
    #[error("agent-control acknowledgement binding mismatch")]
    AckBindingMismatch,
    /// An acknowledgement claims to have applied an already-expired request.
    #[error("agent-control acknowledgement is too late")]
    AckTooLate,
    /// Pause, renew, and resume fields formed an invalid combination.
    #[error("invalid pause-lease shape")]
    InvalidLeaseShape,
    /// Requested lease duration exceeded the hard bound.
    #[error("pause lease duration exceeds limit")]
    LeaseDurationExceeded,
    /// No matching durable lease exists.
    #[error("pause lease not found")]
    LeaseMissing,
    /// Durable lease identity or scope conflicted.
    #[error("pause lease conflict")]
    LeaseConflict,
    /// Requested generation was stale or skipped a generation.
    #[error("pause lease generation mismatch")]
    LeaseGenerationMismatch,
    /// An active lease was already expired at validation/transaction time.
    #[error("pause lease expired")]
    LeaseExpired,
    /// Display-only detail exceeded its explicit byte cap.
    #[error("agent-control acknowledgement excerpt too large")]
    ExcerptTooLarge,
    /// A readable Live Activity excerpt exceeded its explicit byte cap.
    #[error("Live Activity excerpt too large")]
    ActivityExcerptTooLarge,
}

impl AgentControlError {
    /// Stable code for audit records, fixtures, and UI mapping.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSchema(_) => "invalid_schema",
            Self::PlaintextTooLarge => "plaintext_too_large",
            Self::InvalidFormat => "invalid_format",
            Self::UnsupportedVersion => "unsupported_version",
            Self::InvalidField(_) => "invalid_field",
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::Expired => "expired",
            Self::InvalidEventSignature => "invalid_event_signature",
            Self::InvalidEventEnvelope(_) => "invalid_event_envelope",
            Self::DecryptionFailed => "decryption_failed",
            Self::OperatorMismatch => "operator_mismatch",
            Self::AuthorityConflict => "authority_conflict",
            Self::AgentMismatch => "agent_mismatch",
            Self::ComputerMismatch => "computer_mismatch",
            Self::ChannelMismatch => "channel_mismatch",
            Self::RunMismatch => "run_mismatch",
            Self::MissingSteerMessage => "missing_steer_message",
            Self::SteerMessageMismatch => "steer_message_mismatch",
            Self::CommandReplay => "command_replay",
            Self::StoreUnavailable => "store_unavailable",
            Self::AckBindingMismatch => "ack_binding_mismatch",
            Self::AckTooLate => "ack_too_late",
            Self::InvalidLeaseShape => "invalid_lease_shape",
            Self::LeaseDurationExceeded => "lease_duration_exceeded",
            Self::LeaseMissing => "lease_missing",
            Self::LeaseConflict => "lease_conflict",
            Self::LeaseGenerationMismatch => "lease_generation_mismatch",
            Self::LeaseExpired => "lease_expired",
            Self::ExcerptTooLarge => "excerpt_too_large",
            Self::ActivityExcerptTooLarge => "activity_excerpt_too_large",
        }
    }

    /// Enumeration-safe message suitable for an external refusal.
    pub const fn public_message(&self) -> &'static str {
        "agent control refused"
    }
}

/// Opaque result of stateless one-shot validation.
///
/// Holding this value is not proof that `command_id` was durably claimed. The
/// runtime must still execute an atomic claim+enqueue transaction through the
/// sealed store adapter introduced by the runtime slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedOneShotControl {
    command: OneShotControlCommand,
    community_id: CommunityId,
    agent_ownership_revision: u64,
    fingerprint: String,
}

impl ValidatedOneShotControl {
    /// Validated command payload.
    pub fn command(&self) -> &OneShotControlCommand {
        &self.command
    }

    /// Server-resolved tenant bound into the command fingerprint and claim key.
    pub const fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Tenant-bound command fingerprint used for exact-retry classification.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Build the private-field claim projection for the durable store adapter.
    pub fn claim(&self) -> OneShotCommandClaim {
        OneShotCommandClaim {
            community_id: self.community_id,
            command_id: self.command.command_id,
            fingerprint: self.fingerprint.clone(),
            operator_pubkey: self.command.operator_pubkey.clone(),
            agent_pubkey: self.command.target.agent_pubkey.clone(),
            agent_ownership_revision: self.agent_ownership_revision,
            expires_at: self.command.expires_at,
        }
    }

    /// Recheck freshness using the durable transaction's own clock.
    pub fn ensure_fresh_at(&self, transaction_now: u64) -> Result<(), AgentControlError> {
        if transaction_now >= self.command.expires_at {
            return Err(AgentControlError::Expired);
        }
        Ok(())
    }
}

/// Private-field projection supplied to the atomic spent-command transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneShotCommandClaim {
    community_id: CommunityId,
    command_id: Uuid,
    fingerprint: String,
    operator_pubkey: String,
    agent_pubkey: String,
    agent_ownership_revision: u64,
    expires_at: u64,
}

impl OneShotCommandClaim {
    /// Tenant namespace of the claim.
    pub const fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Unique command key within the tenant.
    pub const fn command_id(&self) -> Uuid {
        self.command_id
    }

    /// Exact immutable binding for retry/conflict comparison.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Verified operator identity.
    pub fn operator_pubkey(&self) -> &str {
        &self.operator_pubkey
    }

    /// Verified target agent identity.
    pub fn agent_pubkey(&self) -> &str {
        &self.agent_pubkey
    }

    /// Monotonic owner/visibility revision validated for the target agent.
    pub const fn agent_ownership_revision(&self) -> u64 {
        self.agent_ownership_revision
    }

    /// Recheck current authority while the owner row is locked and immediately
    /// before the recorded command causes an external effect.
    pub fn ensure_current_authority(
        &self,
        operator_pubkey: &str,
        agent_ownership_revision: u64,
    ) -> Result<(), AgentControlError> {
        if operator_pubkey != self.operator_pubkey
            || agent_ownership_revision != self.agent_ownership_revision
        {
            return Err(AgentControlError::AuthorityConflict);
        }
        Ok(())
    }

    /// Signed command deadline.
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

/// Result returned by the authoritative atomic spent-command store operation.
///
/// Implementations compare both `(community_id, command_id)` and the complete
/// fingerprint. The fresh path must persist the claim and enqueue execution in
/// one transaction. This crate-private enum exists only for the reference
/// classifier and tests; constructing it is not proof that I/O occurred. The
/// runtime slice must co-locate a sealed store adapter and private execution
/// permit with the transaction/outbox rows that establish success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum SpentCommandStoreOutcome {
    /// Claim and execution/outbox row were committed atomically.
    AcquiredAndEnqueued,
    /// Exact same command is already durably pending.
    ExactDuplicatePending,
    /// Exact same command has a durable terminal acknowledgement.
    ExactDuplicateCompleted,
    /// The id exists with different fingerprint or binding data.
    CommandIdConflict,
    /// Locked owner/visibility state no longer matched the claim revision.
    AuthorityConflict,
    /// Durable state could not be read or committed.
    StoreUnavailable,
}

/// Crate-private reference result after spent-command classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum OneShotCommandDisposition {
    /// Execute only the transaction's newly enqueued work row.
    DurablyEnqueued,
    /// Do not execute; wait for the original work row's acknowledgement.
    AwaitOriginalAcknowledgement,
    /// Do not execute; return the exact stored terminal acknowledgement.
    ReturnStoredAcknowledgement,
}

/// Opaque result of a validated pause-lease transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPauseLeaseTransition {
    transition: PauseLeaseTransition,
    community_id: CommunityId,
    agent_ownership_revision: u64,
    fingerprint: String,
    retry: bool,
}

impl ValidatedPauseLeaseTransition {
    /// Validated transition payload.
    pub fn transition(&self) -> &PauseLeaseTransition {
        &self.transition
    }

    /// Server-resolved tenant.
    pub const fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Tenant-bound immutable transition fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Whether validation recognized an exact retry of the current row.
    pub const fn is_exact_retry(&self) -> bool {
        self.retry
    }

    /// Build the private-field input for the atomic transition-id claim and lease CAS.
    pub fn claim(&self) -> PauseLeaseTransitionClaim {
        PauseLeaseTransitionClaim {
            community_id: self.community_id,
            transition_id: self.transition.transition_id,
            lease_id: self.transition.lease_id,
            fingerprint: self.fingerprint.clone(),
            operator_pubkey: self.transition.operator_pubkey.clone(),
            target: self.transition.target.clone(),
            agent_ownership_revision: self.agent_ownership_revision,
            transition_expires_at: self.transition.transition_expires_at,
        }
    }

    /// Recheck transition and lease deadlines inside the durable transaction.
    pub fn ensure_fresh_at(&self, transaction_now: u64) -> Result<(), AgentControlError> {
        if transaction_now >= self.transition.transition_expires_at {
            return Err(AgentControlError::Expired);
        }
        if matches!(
            self.transition.transition,
            PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew
        ) && self
            .transition
            .lease_expires_at
            .is_none_or(|expires_at| transaction_now >= expires_at)
        {
            return Err(AgentControlError::LeaseExpired);
        }
        Ok(())
    }
}

/// Private-field input to the durable transition-id claim and current-lease CAS.
///
/// The store must retain `(community_id, transition_id)` with the complete
/// fingerprint through the signed transition deadline plus its accepted
/// transport-skew window. The transition claim and any current-lease mutation
/// commit atomically so replacing the current row cannot make an older signed
/// transition executable again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseLeaseTransitionClaim {
    community_id: CommunityId,
    transition_id: Uuid,
    lease_id: Uuid,
    fingerprint: String,
    operator_pubkey: String,
    target: ControlTarget,
    agent_ownership_revision: u64,
    transition_expires_at: u64,
}

impl PauseLeaseTransitionClaim {
    /// Tenant namespace of the replay claim.
    pub const fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Globally claimed transition id within the tenant.
    pub const fn transition_id(&self) -> Uuid {
        self.transition_id
    }

    /// Lease named by the transition.
    pub const fn lease_id(&self) -> Uuid {
        self.lease_id
    }

    /// Exact tenant-bound transition binding used for duplicate classification.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Verified operator identity.
    pub fn operator_pubkey(&self) -> &str {
        &self.operator_pubkey
    }

    /// Complete computer/agent/channel/run target.
    pub fn target(&self) -> &ControlTarget {
        &self.target
    }

    /// Monotonic owner/visibility revision validated for the target agent.
    pub const fn agent_ownership_revision(&self) -> u64 {
        self.agent_ownership_revision
    }

    /// Recheck current authority while the owner row is locked and immediately
    /// before a retained pause state affects queue dispatch.
    pub fn ensure_current_authority(
        &self,
        operator_pubkey: &str,
        agent_ownership_revision: u64,
    ) -> Result<(), AgentControlError> {
        if operator_pubkey != self.operator_pubkey
            || agent_ownership_revision != self.agent_ownership_revision
        {
            return Err(AgentControlError::AuthorityConflict);
        }
        Ok(())
    }

    /// Signed deadline through which the replay claim must remain available.
    pub const fn transition_expires_at(&self) -> u64 {
        self.transition_expires_at
    }
}

/// Reference result for the authoritative lease compare-and-swap transaction.
///
/// This crate-private enum is executable contract scaffolding, not a runtime
/// store API. The runtime slice must mint any effect permit only from its sealed
/// adapter after the lease state and audit/outbox row commit together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum PauseLeaseStoreOutcome {
    /// Transition id, requested generation, resulting queue state, and audit row
    /// were committed atomically.
    Applied,
    /// The durable transition-id ledger proved the exact fingerprint was
    /// previously committed. The current lease row is not changed, even if a
    /// later lease has replaced the row.
    ExactDuplicate,
    /// Transition id, lease id, scope, generation, or fingerprint conflicted.
    LeaseConflict,
    /// Locked owner/visibility state no longer matched the claim revision.
    AuthorityConflict,
    /// Durable lease state was unavailable.
    StoreUnavailable,
}

/// Crate-private reference result of pause-lease store classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum PauseLeaseDisposition {
    /// State changed once and the matching audit/outbox row is durable.
    StateChanged,
    /// Exact retry was suppressed. Current queue state remains authoritative;
    /// any reused historical acknowledgement must not replace that projection.
    DuplicateSuppressed,
}

/// Queue behavior derived from durable pause state at an authoritative time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectivePauseState {
    /// Hold new dispatch at the next safe boundary.
    HoldQueue,
    /// Durable state says the queue is already running.
    Running,
    /// Lease lapsed; atomically release the hold and emit `pause_lease_expired`.
    ExpiredMustRelease,
    /// Owner/visibility authority changed; atomically release the hold and
    /// audit the authority-driven release before reconsidering dispatch.
    AuthorityChangedMustRelease,
}

/// Opaque validated owner-to-agent control selected after decryption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatedOwnerControl {
    /// Validated cancel or steer, still requiring a durable command claim.
    OneShot(ValidatedOneShotControl),
    /// Validated lease mutation, still requiring the durable lease CAS.
    PauseLease(ValidatedPauseLeaseTransition),
}

/// Parse a strict, size-bounded one-shot command JSON document.
pub fn parse_one_shot_command_json(
    bytes: &[u8],
) -> Result<OneShotControlCommand, AgentControlError> {
    check_plaintext_size(bytes)?;
    let command: OneShotControlCommand = serde_json::from_slice(bytes)
        .map_err(|error| AgentControlError::InvalidSchema(error.to_string()))?;
    command.validate_shape()?;
    Ok(command)
}

/// Parse a strict, size-bounded one-shot acknowledgement JSON document.
pub fn parse_one_shot_ack_json(bytes: &[u8]) -> Result<OneShotControlAck, AgentControlError> {
    check_plaintext_size(bytes)?;
    let ack: OneShotControlAck = serde_json::from_slice(bytes)
        .map_err(|error| AgentControlError::InvalidSchema(error.to_string()))?;
    ack.validate_shape()?;
    Ok(ack)
}

/// Parse and validate one bounded readable Live Activity excerpt.
pub fn parse_live_activity_excerpt_json(
    bytes: &[u8],
) -> Result<LiveActivityExcerpt, AgentControlError> {
    check_plaintext_size(bytes)?;
    let excerpt: LiveActivityExcerpt = serde_json::from_slice(bytes)
        .map_err(|error| AgentControlError::InvalidSchema(error.to_string()))?;
    excerpt.validate()?;
    Ok(excerpt)
}

/// Parse a strict, size-bounded pause-lease transition JSON document.
pub fn parse_pause_lease_json(bytes: &[u8]) -> Result<PauseLeaseTransition, AgentControlError> {
    check_plaintext_size(bytes)?;
    let transition: PauseLeaseTransition = serde_json::from_slice(bytes)
        .map_err(|error| AgentControlError::InvalidSchema(error.to_string()))?;
    transition.validate_shape()?;
    Ok(transition)
}

/// Parse a strict, size-bounded pause-lease acknowledgement JSON document.
pub fn parse_pause_lease_ack_json(bytes: &[u8]) -> Result<PauseLeaseAck, AgentControlError> {
    check_plaintext_size(bytes)?;
    let ack: PauseLeaseAck = serde_json::from_slice(bytes)
        .map_err(|error| AgentControlError::InvalidSchema(error.to_string()))?;
    ack.validate_shape()?;
    Ok(ack)
}

/// Verify, decrypt, dispatch by the closed payload shape, and validate control.
///
/// Runtime integrations should prefer this entry point when consuming the
/// shared NIP-AO control subscription.
pub fn decrypt_and_validate_owner_control(
    event: &Event,
    agent_keys: &Keys,
    facts: &ResolvedControlFacts,
    current_lease: Option<&ResolvedPauseLease>,
) -> Result<ValidatedOwnerControl, AgentControlError> {
    validate_control_event(
        event,
        ObserverDirection::OwnerToAgent,
        &facts.operator_pubkey,
        &facts.target.agent_pubkey,
        event.created_at.as_secs(),
        Some(facts.target.channel_id),
    )?;
    let payload: OwnerControlPayload = decrypt_control_payload(event, agent_keys)?;
    match payload {
        OwnerControlPayload::OneShot(command) => {
            validate_one_shot_control(&command, event, facts).map(ValidatedOwnerControl::OneShot)
        }
        OwnerControlPayload::PauseLease(transition) => {
            validate_pause_lease_transition(&transition, event, facts, current_lease)
                .map(ValidatedOwnerControl::PauseLease)
        }
    }
}

/// Verify, decrypt, strictly parse, and validate a cancel/steer event.
///
/// This is the preferred runtime entry point. It prevents a caller from pairing
/// plaintext parsed from one event with the signed envelope of another.
pub fn decrypt_and_validate_one_shot_control(
    event: &Event,
    agent_keys: &Keys,
    facts: &ResolvedControlFacts,
) -> Result<ValidatedOneShotControl, AgentControlError> {
    validate_control_event(
        event,
        ObserverDirection::OwnerToAgent,
        &facts.operator_pubkey,
        &facts.target.agent_pubkey,
        event.created_at.as_secs(),
        Some(facts.target.channel_id),
    )?;
    let command: OneShotControlCommand = decrypt_control_payload(event, agent_keys)?;
    validate_one_shot_control(&command, event, facts)
}

/// Verify, decrypt, strictly parse, and validate a pause-lease mutation.
pub fn decrypt_and_validate_pause_lease_transition(
    event: &Event,
    agent_keys: &Keys,
    facts: &ResolvedControlFacts,
    current: Option<&ResolvedPauseLease>,
) -> Result<ValidatedPauseLeaseTransition, AgentControlError> {
    validate_control_event(
        event,
        ObserverDirection::OwnerToAgent,
        &facts.operator_pubkey,
        &facts.target.agent_pubkey,
        event.created_at.as_secs(),
        Some(facts.target.channel_id),
    )?;
    let transition: PauseLeaseTransition = decrypt_control_payload(event, agent_keys)?;
    validate_pause_lease_transition(&transition, event, facts, current)
}

/// Verify, decrypt, strictly parse, and validate a one-shot acknowledgement.
pub fn decrypt_and_validate_one_shot_ack(
    validated: &ValidatedOneShotControl,
    event: &Event,
    operator_keys: &Keys,
) -> Result<OneShotControlAck, AgentControlError> {
    let command = validated.command();
    validate_control_event(
        event,
        ObserverDirection::AgentToOwner,
        &command.operator_pubkey,
        &command.target.agent_pubkey,
        event.created_at.as_secs(),
        Some(command.target.channel_id),
    )?;
    let ack: OneShotControlAck = decrypt_control_payload(event, operator_keys)?;
    validate_one_shot_ack(validated, &ack, event)?;
    Ok(ack)
}

/// Verify, decrypt, strictly parse, and validate a pause-lease acknowledgement.
pub fn decrypt_and_validate_pause_lease_ack(
    validated: &ValidatedPauseLeaseTransition,
    event: &Event,
    operator_keys: &Keys,
) -> Result<PauseLeaseAck, AgentControlError> {
    let transition = validated.transition();
    validate_control_event(
        event,
        ObserverDirection::AgentToOwner,
        &transition.operator_pubkey,
        &transition.target.agent_pubkey,
        event.created_at.as_secs(),
        Some(transition.target.channel_id),
    )?;
    let ack: PauseLeaseAck = decrypt_control_payload(event, operator_keys)?;
    validate_pause_lease_ack(validated, &ack, event)?;
    Ok(ack)
}

/// Validate a signed, decrypted cancel/steer command against trusted facts.
///
/// This performs CPU-bound Schnorr verification. Async handlers must use their
/// blocking-work facility. On success, claim the command durably before any
/// ACP or queue side effect.
fn validate_one_shot_control(
    command: &OneShotControlCommand,
    event: &Event,
    facts: &ResolvedControlFacts,
) -> Result<ValidatedOneShotControl, AgentControlError> {
    command.validate_shape()?;
    validate_facts(facts)?;
    if command.issued_at > facts.now {
        return Err(AgentControlError::InvalidTimestamp);
    }
    if facts.now >= command.expires_at {
        return Err(AgentControlError::Expired);
    }
    validate_control_event(
        event,
        ObserverDirection::OwnerToAgent,
        &facts.operator_pubkey,
        &facts.target.agent_pubkey,
        command.issued_at,
        Some(command.target.channel_id),
    )?;
    validate_target_bindings(
        &command.operator_pubkey,
        &command.target,
        &facts.operator_pubkey,
        &facts.target,
    )?;
    validate_steer_binding(command, facts)?;
    let fingerprint = one_shot_command_fingerprint(facts.community_id, command)?;
    Ok(ValidatedOneShotControl {
        command: command.clone(),
        community_id: facts.community_id,
        agent_ownership_revision: facts.agent_ownership_revision,
        fingerprint,
    })
}

/// Classify a reference atomic spent-command result for contract tests.
///
/// Exact duplicates never execute twice. Conflicting reuse of a spent id is a
/// replay rejection. Freshness is rechecked using the transaction's own clock.
/// Runtime code must use a sealed durable adapter rather than exposing or
/// accepting this caller-constructible reference outcome.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn classify_spent_command(
    validated: &ValidatedOneShotControl,
    transaction_now: u64,
    outcome: SpentCommandStoreOutcome,
) -> Result<OneShotCommandDisposition, AgentControlError> {
    validated.ensure_fresh_at(transaction_now)?;
    match outcome {
        SpentCommandStoreOutcome::AcquiredAndEnqueued => {
            Ok(OneShotCommandDisposition::DurablyEnqueued)
        }
        SpentCommandStoreOutcome::ExactDuplicatePending => {
            Ok(OneShotCommandDisposition::AwaitOriginalAcknowledgement)
        }
        SpentCommandStoreOutcome::ExactDuplicateCompleted => {
            Ok(OneShotCommandDisposition::ReturnStoredAcknowledgement)
        }
        SpentCommandStoreOutcome::CommandIdConflict => Err(AgentControlError::CommandReplay),
        SpentCommandStoreOutcome::AuthorityConflict => Err(AgentControlError::AuthorityConflict),
        SpentCommandStoreOutcome::StoreUnavailable => Err(AgentControlError::StoreUnavailable),
    }
}

/// Validate an agent-signed acknowledgement against a validated command.
fn validate_one_shot_ack(
    validated: &ValidatedOneShotControl,
    ack: &OneShotControlAck,
    event: &Event,
) -> Result<(), AgentControlError> {
    ack.validate_shape()?;
    let command = validated.command();
    validate_control_event(
        event,
        ObserverDirection::AgentToOwner,
        &command.operator_pubkey,
        &command.target.agent_pubkey,
        ack.acked_at,
        Some(command.target.channel_id),
    )?;
    if ack.command_id != command.command_id
        || ack.command_fingerprint != validated.fingerprint
        || ack.control != command.control
        || ack.operator_pubkey != command.operator_pubkey
        || ack.target != command.target
        || ack.command_seq != command.seq
    {
        return Err(AgentControlError::AckBindingMismatch);
    }
    if ack.acked_at < command.issued_at || ack.acked_at >= command.expires_at {
        return Err(AgentControlError::AckTooLate);
    }
    match (command.control, ack.status) {
        (OneShotControlKind::Cancel, ControlAckStatus::Queued)
        | (OneShotControlKind::Steer, ControlAckStatus::NoActiveTurn) => {
            Err(AgentControlError::AckBindingMismatch)
        }
        _ => Ok(()),
    }
}

/// Validate a pause/renew/resume request against signed evidence and stored state.
fn validate_pause_lease_transition(
    transition: &PauseLeaseTransition,
    event: &Event,
    facts: &ResolvedControlFacts,
    current: Option<&ResolvedPauseLease>,
) -> Result<ValidatedPauseLeaseTransition, AgentControlError> {
    transition.validate_shape()?;
    validate_facts(facts)?;
    if transition.issued_at > facts.now {
        return Err(AgentControlError::InvalidTimestamp);
    }
    if facts.now >= transition.transition_expires_at {
        return Err(AgentControlError::Expired);
    }
    validate_control_event(
        event,
        ObserverDirection::OwnerToAgent,
        &facts.operator_pubkey,
        &facts.target.agent_pubkey,
        transition.issued_at,
        Some(transition.target.channel_id),
    )?;
    validate_target_bindings(
        &transition.operator_pubkey,
        &transition.target,
        &facts.operator_pubkey,
        &facts.target,
    )?;
    let fingerprint = pause_lease_fingerprint(facts.community_id, transition)?;
    let retry = validate_pause_state(
        transition,
        facts.community_id,
        facts.agent_ownership_revision,
        facts.now,
        &fingerprint,
        current,
    )?;
    Ok(ValidatedPauseLeaseTransition {
        transition: transition.clone(),
        community_id: facts.community_id,
        agent_ownership_revision: facts.agent_ownership_revision,
        fingerprint,
        retry,
    })
}

/// Classify a reference compare-and-swap result for lease contract tests.
///
/// Runtime code must use a sealed durable adapter rather than exposing or
/// accepting this caller-constructible reference outcome.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn classify_pause_lease_outcome(
    validated: &ValidatedPauseLeaseTransition,
    transaction_now: u64,
    outcome: PauseLeaseStoreOutcome,
) -> Result<PauseLeaseDisposition, AgentControlError> {
    validated.ensure_fresh_at(transaction_now)?;
    match outcome {
        PauseLeaseStoreOutcome::Applied if !validated.retry => {
            Ok(PauseLeaseDisposition::StateChanged)
        }
        PauseLeaseStoreOutcome::ExactDuplicate => Ok(PauseLeaseDisposition::DuplicateSuppressed),
        PauseLeaseStoreOutcome::Applied | PauseLeaseStoreOutcome::LeaseConflict => {
            Err(AgentControlError::LeaseConflict)
        }
        PauseLeaseStoreOutcome::AuthorityConflict => Err(AgentControlError::AuthorityConflict),
        PauseLeaseStoreOutcome::StoreUnavailable => Err(AgentControlError::StoreUnavailable),
    }
}

/// Validate an agent-signed pause-lease acknowledgement.
fn validate_pause_lease_ack(
    validated: &ValidatedPauseLeaseTransition,
    ack: &PauseLeaseAck,
    event: &Event,
) -> Result<(), AgentControlError> {
    ack.validate_shape()?;
    let transition = validated.transition();
    validate_control_event(
        event,
        ObserverDirection::AgentToOwner,
        &transition.operator_pubkey,
        &transition.target.agent_pubkey,
        ack.acked_at,
        Some(transition.target.channel_id),
    )?;
    if ack.transition_id != transition.transition_id
        || ack.transition_fingerprint != validated.fingerprint
        || ack.lease_id != transition.lease_id
        || ack.generation != transition.generation
        || ack.transition != transition.transition
        || ack.operator_pubkey != transition.operator_pubkey
        || ack.target != transition.target
        || ack.transition_seq != transition.seq
    {
        return Err(AgentControlError::AckBindingMismatch);
    }
    if ack.acked_at < transition.issued_at || ack.acked_at >= transition.transition_expires_at {
        return Err(AgentControlError::AckTooLate);
    }
    Ok(())
}

/// Derive queue behavior from one validated durable lease row.
pub fn effective_pause_state(
    lease: &ResolvedPauseLease,
    now: u64,
    current_operator_pubkey: &str,
    current_agent_ownership_revision: u64,
) -> Result<EffectivePauseState, AgentControlError> {
    lease.validate()?;
    if current_agent_ownership_revision == 0 {
        return Err(AgentControlError::AuthorityConflict);
    }
    parse_pubkey("current_operator_pubkey", current_operator_pubkey)?;
    if !lease.active {
        Ok(EffectivePauseState::Running)
    } else if lease.operator_pubkey != current_operator_pubkey
        || lease.agent_ownership_revision != current_agent_ownership_revision
    {
        Ok(EffectivePauseState::AuthorityChangedMustRelease)
    } else if now >= lease.lease_expires_at {
        Ok(EffectivePauseState::ExpiredMustRelease)
    } else {
        Ok(EffectivePauseState::HoldQueue)
    }
}

/// Compute the frozen tenant-bound command fingerprint.
pub fn one_shot_command_fingerprint(
    community_id: CommunityId,
    command: &OneShotControlCommand,
) -> Result<String, AgentControlError> {
    command.validate_shape()?;
    let mut hasher = Sha256::new();
    hasher.update(COMMAND_HASH_DOMAIN);
    hasher.update(community_id.as_uuid().as_bytes());
    hasher.update(command.command_id.as_bytes());
    hasher.update([match command.control {
        OneShotControlKind::Cancel => 1,
        OneShotControlKind::Steer => 2,
    }]);
    hash_string(&mut hasher, &command.operator_pubkey);
    hash_target(&mut hasher, &command.target);
    hasher.update(command.seq.to_be_bytes());
    hasher.update(command.issued_at.to_be_bytes());
    hasher.update(command.expires_at.to_be_bytes());
    match command.steer_message_event_id.as_deref() {
        Some(event_id) => {
            hasher.update([1]);
            hash_string(&mut hasher, event_id);
        }
        None => hasher.update([0]),
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Compute the frozen tenant-bound pause-transition fingerprint.
pub fn pause_lease_fingerprint(
    community_id: CommunityId,
    transition: &PauseLeaseTransition,
) -> Result<String, AgentControlError> {
    transition.validate_shape()?;
    let mut hasher = Sha256::new();
    hasher.update(LEASE_HASH_DOMAIN);
    hasher.update(community_id.as_uuid().as_bytes());
    hasher.update(transition.transition_id.as_bytes());
    hasher.update(transition.lease_id.as_bytes());
    hasher.update(transition.generation.to_be_bytes());
    hasher.update([match transition.transition {
        PauseLeaseTransitionKind::Pause => 1,
        PauseLeaseTransitionKind::Renew => 2,
        PauseLeaseTransitionKind::Resume => 3,
    }]);
    hash_string(&mut hasher, &transition.operator_pubkey);
    hash_target(&mut hasher, &transition.target);
    hasher.update(transition.seq.to_be_bytes());
    hasher.update(transition.issued_at.to_be_bytes());
    hasher.update(transition.transition_expires_at.to_be_bytes());
    match transition.lease_expires_at {
        Some(expires_at) => {
            hasher.update([1]);
            hasher.update(expires_at.to_be_bytes());
        }
        None => hasher.update([0]),
    }
    Ok(hex::encode(hasher.finalize()))
}

fn validate_pause_state(
    transition: &PauseLeaseTransition,
    community_id: CommunityId,
    agent_ownership_revision: u64,
    now: u64,
    fingerprint: &str,
    current: Option<&ResolvedPauseLease>,
) -> Result<bool, AgentControlError> {
    if let Some(lease) = current {
        lease.validate()?;
        if lease.community_id != community_id
            || lease.target.agent_pubkey != transition.target.agent_pubkey
            || lease.target.computer_id != transition.target.computer_id
        {
            return Err(AgentControlError::LeaseConflict);
        }
        let same_authority = lease.operator_pubkey == transition.operator_pubkey
            && lease.agent_ownership_revision == agent_ownership_revision;
        let exact_retry = same_authority
            && lease.target == transition.target
            && lease.lease_id == transition.lease_id
            && lease.generation == transition.generation
            && lease.last_transition_id == transition.transition_id
            && lease.last_transition_fingerprint == fingerprint;
        let retry_state_matches = match transition.transition {
            PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => {
                lease.active && now < lease.lease_expires_at
            }
            PauseLeaseTransitionKind::Resume => !lease.active,
        };
        if exact_retry && retry_state_matches {
            return Ok(true);
        }
        // A new owner/revision may replace only a row that the pre-dispatch
        // authority check has already released. An active stale-authority row
        // cannot be bypassed by presenting a new lease id.
        if !same_authority && lease.active {
            return Err(AgentControlError::LeaseConflict);
        }
    }

    match transition.transition {
        PauseLeaseTransitionKind::Pause => match current {
            None => Ok(false),
            Some(lease) if !lease.active || now >= lease.lease_expires_at => {
                if lease.lease_id == transition.lease_id {
                    Err(AgentControlError::LeaseConflict)
                } else {
                    Ok(false)
                }
            }
            Some(_) => Err(AgentControlError::LeaseConflict),
        },
        PauseLeaseTransitionKind::Renew => {
            let lease = current.ok_or(AgentControlError::LeaseMissing)?;
            if lease.operator_pubkey != transition.operator_pubkey
                || lease.agent_ownership_revision != agent_ownership_revision
                || lease.target != transition.target
                || lease.lease_id != transition.lease_id
            {
                return Err(AgentControlError::LeaseConflict);
            }
            if !lease.active || now >= lease.lease_expires_at {
                return Err(AgentControlError::LeaseExpired);
            }
            if transition.generation != lease.generation.saturating_add(1) {
                return Err(AgentControlError::LeaseGenerationMismatch);
            }
            if transition
                .lease_expires_at
                .is_none_or(|new_expiry| new_expiry <= lease.lease_expires_at)
            {
                return Err(AgentControlError::InvalidTimestamp);
            }
            Ok(false)
        }
        PauseLeaseTransitionKind::Resume => {
            let lease = current.ok_or(AgentControlError::LeaseMissing)?;
            if lease.operator_pubkey != transition.operator_pubkey
                || lease.agent_ownership_revision != agent_ownership_revision
                || lease.target != transition.target
                || lease.lease_id != transition.lease_id
            {
                return Err(AgentControlError::LeaseConflict);
            }
            if !lease.active {
                return Err(AgentControlError::LeaseGenerationMismatch);
            }
            if transition.generation != lease.generation.saturating_add(1) {
                return Err(AgentControlError::LeaseGenerationMismatch);
            }
            // An expired active row is still releasable: resume and the expiry
            // sweeper converge on the same Running state through the store CAS.
            Ok(false)
        }
    }
}

fn validate_steer_binding(
    command: &OneShotControlCommand,
    facts: &ResolvedControlFacts,
) -> Result<(), AgentControlError> {
    match command.control {
        OneShotControlKind::Cancel => {
            if facts.steer_message.is_some() {
                return Err(AgentControlError::SteerMessageMismatch);
            }
            Ok(())
        }
        OneShotControlKind::Steer => {
            let event_id = command
                .steer_message_event_id
                .as_deref()
                .ok_or(AgentControlError::MissingSteerMessage)?;
            let message = facts
                .steer_message
                .as_ref()
                .ok_or(AgentControlError::MissingSteerMessage)?;
            parse_event_id("resolved_steer_event_id", &message.event_id)?;
            parse_pubkey("resolved_steer_operator", &message.operator_pubkey)?;
            if message.community_id != facts.community_id
                || message.event_id != event_id
                || message.operator_pubkey != facts.operator_pubkey
                || message.channel_id != command.target.channel_id
                || message.created_at == 0
                || message.created_at > command.issued_at
            {
                return Err(AgentControlError::SteerMessageMismatch);
            }
            Ok(())
        }
    }
}

fn validate_facts(facts: &ResolvedControlFacts) -> Result<(), AgentControlError> {
    if facts.now == 0 {
        return Err(AgentControlError::InvalidTimestamp);
    }
    if facts.agent_ownership_revision == 0 {
        return Err(AgentControlError::AuthorityConflict);
    }
    parse_pubkey("resolved_operator", &facts.operator_pubkey)?;
    facts.target.validate()
}

fn validate_target_bindings(
    payload_operator: &str,
    payload_target: &ControlTarget,
    resolved_operator: &str,
    resolved_target: &ControlTarget,
) -> Result<(), AgentControlError> {
    if payload_operator != resolved_operator {
        return Err(AgentControlError::OperatorMismatch);
    }
    if payload_target.agent_pubkey != resolved_target.agent_pubkey {
        return Err(AgentControlError::AgentMismatch);
    }
    if payload_target.computer_id != resolved_target.computer_id {
        return Err(AgentControlError::ComputerMismatch);
    }
    if payload_target.channel_id != resolved_target.channel_id {
        return Err(AgentControlError::ChannelMismatch);
    }
    if payload_target.run_id != resolved_target.run_id {
        return Err(AgentControlError::RunMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObserverDirection {
    OwnerToAgent,
    AgentToOwner,
}

fn validate_control_event(
    event: &Event,
    direction: ObserverDirection,
    operator_pubkey: &str,
    agent_pubkey: &str,
    expected_created_at: u64,
    expected_channel: Option<Uuid>,
) -> Result<(), AgentControlError> {
    if event.kind.as_u16() as u32 != KIND_AGENT_OBSERVER_FRAME {
        return Err(AgentControlError::InvalidEventEnvelope("wrong kind".into()));
    }
    if !content_looks_like_nip44(&event.content) {
        return Err(AgentControlError::InvalidEventEnvelope(
            "content is not a plausible NIP-44 v2 ciphertext".into(),
        ));
    }
    verify_event(event).map_err(|_| AgentControlError::InvalidEventSignature)?;
    let operator = parse_pubkey("operator_pubkey", operator_pubkey)?;
    let agent = parse_pubkey("agent_pubkey", agent_pubkey)?;
    if event.created_at.as_secs() != expected_created_at {
        return Err(AgentControlError::InvalidTimestamp);
    }
    match direction {
        ObserverDirection::OwnerToAgent if event.pubkey != operator => {
            return Err(AgentControlError::OperatorMismatch);
        }
        ObserverDirection::AgentToOwner if event.pubkey != agent => {
            return Err(AgentControlError::AgentMismatch);
        }
        _ => {}
    }

    let mut recipient = None;
    let mut tagged_agent = None;
    let mut frame = None;
    let mut channel = None;
    for tag in event.tags.iter() {
        let parts = tag.as_slice();
        if parts.len() != 2 {
            return Err(AgentControlError::InvalidEventEnvelope(
                "every tag must contain exactly one value".into(),
            ));
        }
        let slot = match parts[0].as_str() {
            "p" => &mut recipient,
            OBSERVER_AGENT_TAG => &mut tagged_agent,
            OBSERVER_FRAME_TAG => &mut frame,
            "h" => &mut channel,
            name => {
                return Err(AgentControlError::InvalidEventEnvelope(format!(
                    "unexpected tag: {name}"
                )));
            }
        };
        if slot.replace(parts[1].clone()).is_some() {
            return Err(AgentControlError::InvalidEventEnvelope(format!(
                "duplicate {} tag",
                parts[0]
            )));
        }
    }
    let (expected_recipient, expected_frame) = match direction {
        ObserverDirection::OwnerToAgent => (agent.to_hex(), OBSERVER_FRAME_CONTROL),
        ObserverDirection::AgentToOwner => (operator.to_hex(), OBSERVER_FRAME_TELEMETRY),
    };
    if recipient.as_deref() != Some(expected_recipient.as_str())
        || tagged_agent.as_deref() != Some(agent.to_hex().as_str())
        || frame.as_deref() != Some(expected_frame)
    {
        return Err(AgentControlError::InvalidEventEnvelope(
            "direction tags do not match signed identities".into(),
        ));
    }
    if let Some(channel_value) = channel {
        let expected_channel = expected_channel
            .ok_or_else(|| AgentControlError::InvalidEventEnvelope("unexpected h tag".into()))?;
        if channel_value != expected_channel.to_string() {
            return Err(AgentControlError::ChannelMismatch);
        }
    }
    Ok(())
}

fn validate_discriminator(
    actual_format: &str,
    expected_format: &str,
    version: u32,
) -> Result<(), AgentControlError> {
    if actual_format != expected_format {
        return Err(AgentControlError::InvalidFormat);
    }
    if version != VERSION {
        return Err(AgentControlError::UnsupportedVersion);
    }
    Ok(())
}

fn validate_transition_times(issued_at: u64, expires_at: u64) -> Result<(), AgentControlError> {
    if issued_at == 0 || expires_at <= issued_at || expires_at - issued_at > MAX_TRANSITION_TTL_SECS
    {
        return Err(AgentControlError::InvalidTimestamp);
    }
    Ok(())
}

fn validate_lease_expiry(issued_at: u64, expires_at: u64) -> Result<(), AgentControlError> {
    if expires_at <= issued_at {
        return Err(AgentControlError::InvalidTimestamp);
    }
    if expires_at - issued_at > MAX_PAUSE_LEASE_SECS {
        return Err(AgentControlError::LeaseDurationExceeded);
    }
    Ok(())
}

fn validate_opaque_id(label: &'static str, value: &str) -> Result<(), AgentControlError> {
    if value.is_empty()
        || value.len() > MAX_OPAQUE_ID_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(AgentControlError::InvalidField(label));
    }
    Ok(())
}

fn validate_hash(label: &'static str, value: &str) -> Result<(), AgentControlError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(AgentControlError::InvalidField(label));
    }
    Ok(())
}

fn parse_pubkey(label: &'static str, value: &str) -> Result<PublicKey, AgentControlError> {
    validate_hash(label, value)?;
    let key = PublicKey::from_hex(value).map_err(|_| AgentControlError::InvalidField(label))?;
    key.xonly()
        .map_err(|_| AgentControlError::InvalidField(label))?;
    Ok(key)
}

fn parse_event_id(label: &'static str, value: &str) -> Result<EventId, AgentControlError> {
    validate_hash(label, value)?;
    EventId::from_hex(value).map_err(|_| AgentControlError::InvalidField(label))
}

fn check_plaintext_size(bytes: &[u8]) -> Result<(), AgentControlError> {
    if bytes.len() > MAX_CONTROL_PLAINTEXT_BYTES {
        Err(AgentControlError::PlaintextTooLarge)
    } else {
        Ok(())
    }
}

fn decrypt_control_payload<T: DeserializeOwned>(
    event: &Event,
    recipient_keys: &Keys,
) -> Result<T, AgentControlError> {
    let mut plaintext = nip44::decrypt(
        recipient_keys.secret_key(),
        &event.pubkey,
        event.content.as_str(),
    )
    .map_err(|_| AgentControlError::DecryptionFailed)?;
    if plaintext.len() > MAX_CONTROL_PLAINTEXT_BYTES {
        plaintext.zeroize();
        return Err(AgentControlError::PlaintextTooLarge);
    }
    let parsed = serde_json::from_str(&plaintext)
        .map_err(|error| AgentControlError::InvalidSchema(error.to_string()));
    plaintext.zeroize();
    parsed
}

fn hash_target(hasher: &mut Sha256, target: &ControlTarget) {
    hash_string(hasher, &target.computer_id);
    hash_string(hasher, &target.agent_pubkey);
    hasher.update(target.channel_id.as_bytes());
    hash_string(hasher, &target.run_id);
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use nostr::{EventBuilder, Kind, Tag};
    use serde::Deserialize;

    use super::*;
    use crate::observer::encrypt_observer_payload;

    const NOW: u64 = 1_800_000_000;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixtureManifest {
        format: String,
        version: u32,
        community_id: String,
        cases: Vec<FixtureCase>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixtureCase {
        name: String,
        expect: String,
        #[serde(default)]
        error_code: Option<String>,
    }

    struct OneShotFixture {
        community_id: CommunityId,
        owner: Keys,
        agent: Keys,
        other: Keys,
        command: OneShotControlCommand,
        facts: ResolvedControlFacts,
    }

    fn community_id() -> CommunityId {
        CommunityId::from_uuid(
            Uuid::parse_str("3580ca9b-47b4-4af9-b22a-1068778f26c6").expect("fixed community id"),
        )
    }

    fn channel_id() -> Uuid {
        Uuid::parse_str("52a85618-0f8f-4542-94ec-599e6e1c6f2e").expect("fixed channel id")
    }

    fn message_event_id() -> String {
        "11".repeat(32)
    }

    fn base_target(agent: &Keys) -> ControlTarget {
        ControlTarget {
            computer_id: "computer-1".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: channel_id(),
            run_id: "run-1".into(),
        }
    }

    fn one_shot_fixture() -> OneShotFixture {
        let owner = Keys::generate();
        let agent = Keys::generate();
        let other = Keys::generate();
        let target = base_target(&agent);
        let command = OneShotControlCommand {
            format: COMMAND_FORMAT.into(),
            version: VERSION,
            command_id: Uuid::new_v4(),
            control: OneShotControlKind::Cancel,
            operator_pubkey: owner.public_key().to_hex(),
            target: target.clone(),
            seq: 7,
            issued_at: NOW - 1,
            expires_at: NOW + 60,
            steer_message_event_id: None,
        };
        let facts = ResolvedControlFacts {
            community_id: community_id(),
            now: NOW,
            operator_pubkey: owner.public_key().to_hex(),
            agent_ownership_revision: 1,
            target,
            steer_message: None,
        };
        OneShotFixture {
            community_id: community_id(),
            owner,
            agent,
            other,
            command,
            facts,
        }
    }

    fn make_steer(fixture: &mut OneShotFixture) {
        let event_id = message_event_id();
        fixture.command.control = OneShotControlKind::Steer;
        fixture.command.steer_message_event_id = Some(event_id.clone());
        fixture.facts.steer_message = Some(ResolvedSteerMessage {
            community_id: fixture.community_id,
            event_id,
            operator_pubkey: fixture.owner.public_key().to_hex(),
            channel_id: fixture.command.target.channel_id,
            created_at: fixture.command.issued_at,
        });
    }

    fn signed_frame<T: Serialize>(
        sender: &Keys,
        recipient: &PublicKey,
        agent_pubkey: &str,
        frame: &str,
        created_at: u64,
        channel_id: Uuid,
        payload: &T,
    ) -> Event {
        let encrypted = encrypt_observer_payload(sender, recipient, payload).expect("encrypt");
        signed_ciphertext_frame(
            sender,
            recipient,
            agent_pubkey,
            frame,
            created_at,
            channel_id,
            encrypted,
        )
    }

    fn signed_raw_frame(
        sender: &Keys,
        recipient: &PublicKey,
        agent_pubkey: &str,
        frame: &str,
        created_at: u64,
        channel_id: Uuid,
        plaintext: &str,
    ) -> Event {
        let encrypted = nip44::encrypt(
            sender.secret_key(),
            recipient,
            plaintext,
            nip44::Version::V2,
        )
        .expect("encrypt raw payload");
        signed_ciphertext_frame(
            sender,
            recipient,
            agent_pubkey,
            frame,
            created_at,
            channel_id,
            encrypted,
        )
    }

    fn signed_ciphertext_frame(
        sender: &Keys,
        recipient: &PublicKey,
        agent_pubkey: &str,
        frame: &str,
        created_at: u64,
        channel_id: Uuid,
        encrypted: String,
    ) -> Event {
        EventBuilder::new(Kind::Custom(KIND_AGENT_OBSERVER_FRAME as u16), encrypted)
            .tags([
                Tag::parse(["p", recipient.to_hex().as_str()]).expect("p tag"),
                Tag::parse([OBSERVER_AGENT_TAG, agent_pubkey]).expect("agent tag"),
                Tag::parse([OBSERVER_FRAME_TAG, frame]).expect("frame tag"),
                Tag::parse(["h", channel_id.to_string().as_str()]).expect("h tag"),
            ])
            .custom_created_at(nostr::Timestamp::from(created_at))
            .sign_with_keys(sender)
            .expect("sign")
    }

    fn command_event(fixture: &OneShotFixture) -> Event {
        signed_frame(
            &fixture.owner,
            &fixture.agent.public_key(),
            &fixture.facts.target.agent_pubkey,
            OBSERVER_FRAME_CONTROL,
            fixture.command.issued_at,
            fixture.facts.target.channel_id,
            &fixture.command,
        )
    }

    fn validate_command(
        fixture: &OneShotFixture,
    ) -> Result<ValidatedOneShotControl, AgentControlError> {
        decrypt_and_validate_one_shot_control(
            &command_event(fixture),
            &fixture.agent,
            &fixture.facts,
        )
    }

    fn command_ack(validated: &ValidatedOneShotControl, acked_at: u64) -> OneShotControlAck {
        let command = validated.command();
        OneShotControlAck {
            format: COMMAND_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            command_id: command.command_id,
            command_fingerprint: validated.fingerprint().into(),
            control: command.control,
            operator_pubkey: command.operator_pubkey.clone(),
            target: command.target.clone(),
            command_seq: command.seq,
            seq: 10,
            acked_at,
            status: ControlAckStatus::Applied,
            reason: None,
            detail: None,
        }
    }

    fn command_ack_event(fixture: &OneShotFixture, ack: &OneShotControlAck) -> Event {
        signed_frame(
            &fixture.agent,
            &fixture.owner.public_key(),
            &fixture.facts.target.agent_pubkey,
            OBSERVER_FRAME_TELEMETRY,
            ack.acked_at,
            fixture.facts.target.channel_id,
            ack,
        )
    }

    fn base_pause(fixture: &OneShotFixture) -> PauseLeaseTransition {
        PauseLeaseTransition {
            format: PAUSE_LEASE_FORMAT.into(),
            version: VERSION,
            transition_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
            transition: PauseLeaseTransitionKind::Pause,
            operator_pubkey: fixture.owner.public_key().to_hex(),
            target: fixture.facts.target.clone(),
            seq: 20,
            issued_at: NOW - 1,
            transition_expires_at: NOW + 60,
            lease_expires_at: Some(NOW + DEFAULT_PAUSE_LEASE_SECS),
        }
    }

    fn pause_event(fixture: &OneShotFixture, transition: &PauseLeaseTransition) -> Event {
        signed_frame(
            &fixture.owner,
            &fixture.agent.public_key(),
            &fixture.facts.target.agent_pubkey,
            OBSERVER_FRAME_CONTROL,
            transition.issued_at,
            fixture.facts.target.channel_id,
            transition,
        )
    }

    fn validate_pause(
        fixture: &OneShotFixture,
        transition: &PauseLeaseTransition,
        current: Option<&ResolvedPauseLease>,
    ) -> Result<ValidatedPauseLeaseTransition, AgentControlError> {
        decrypt_and_validate_pause_lease_transition(
            &pause_event(fixture, transition),
            &fixture.agent,
            &fixture.facts,
            current,
        )
    }

    fn lease_from(validated: &ValidatedPauseLeaseTransition, active: bool) -> ResolvedPauseLease {
        let transition = validated.transition();
        ResolvedPauseLease {
            community_id: validated.community_id(),
            lease_id: transition.lease_id,
            operator_pubkey: transition.operator_pubkey.clone(),
            agent_ownership_revision: validated.claim().agent_ownership_revision(),
            target: transition.target.clone(),
            generation: transition.generation,
            active,
            lease_expires_at: transition.lease_expires_at.unwrap_or(NOW + 300),
            last_transition_id: transition.transition_id,
            last_transition_fingerprint: validated.fingerprint().into(),
        }
    }

    fn next_lease_transition(
        prior: &PauseLeaseTransition,
        kind: PauseLeaseTransitionKind,
    ) -> PauseLeaseTransition {
        PauseLeaseTransition {
            transition_id: Uuid::new_v4(),
            generation: prior.generation + 1,
            transition: kind,
            seq: prior.seq + 1,
            issued_at: NOW,
            transition_expires_at: NOW + 60,
            lease_expires_at: match kind {
                PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => {
                    Some(NOW + 600)
                }
                PauseLeaseTransitionKind::Resume => None,
            },
            ..prior.clone()
        }
    }

    fn pause_ack(validated: &ValidatedPauseLeaseTransition, acked_at: u64) -> PauseLeaseAck {
        let transition = validated.transition();
        PauseLeaseAck {
            format: PAUSE_LEASE_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            transition_id: transition.transition_id,
            transition_fingerprint: validated.fingerprint().into(),
            lease_id: transition.lease_id,
            generation: transition.generation,
            transition: transition.transition,
            operator_pubkey: transition.operator_pubkey.clone(),
            target: transition.target.clone(),
            transition_seq: transition.seq,
            seq: 30,
            acked_at,
            status: PauseLeaseAckStatus::Applied,
            queue_state: match transition.transition {
                PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => {
                    QueueHoldState::Paused
                }
                PauseLeaseTransitionKind::Resume => QueueHoldState::Running,
            },
            detail: None,
        }
    }

    fn pause_ack_event(fixture: &OneShotFixture, ack: &PauseLeaseAck) -> Event {
        signed_frame(
            &fixture.agent,
            &fixture.owner.public_key(),
            &fixture.facts.target.agent_pubkey,
            OBSERVER_FRAME_TELEMETRY,
            ack.acked_at,
            fixture.facts.target.channel_id,
            ack,
        )
    }

    fn observe(case: &str) -> Result<&'static str, AgentControlError> {
        let mut fixture = one_shot_fixture();
        match case {
            "valid_cancel" => {
                let validated = validate_command(&fixture)?;
                match classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::AcquiredAndEnqueued,
                )? {
                    OneShotCommandDisposition::DurablyEnqueued => Ok("durably_enqueued"),
                    _ => Err(AgentControlError::CommandReplay),
                }
            }
            "valid_steer" => {
                make_steer(&mut fixture);
                let validated = validate_command(&fixture)?;
                match classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::AcquiredAndEnqueued,
                )? {
                    OneShotCommandDisposition::DurablyEnqueued => Ok("durably_enqueued"),
                    _ => Err(AgentControlError::CommandReplay),
                }
            }
            "forged_control" => {
                let mut event = command_event(&fixture);
                event.content.push('A');
                decrypt_and_validate_one_shot_control(&event, &fixture.agent, &fixture.facts)?;
                Ok("unexpected")
            }
            "wrong_operator" => {
                fixture.command.operator_pubkey = fixture.other.public_key().to_hex();
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "wrong_agent" => {
                fixture.command.target.agent_pubkey = fixture.other.public_key().to_hex();
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "wrong_computer" => {
                fixture.command.target.computer_id = "computer-2".into();
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "wrong_channel" => {
                fixture.command.target.channel_id = Uuid::new_v4();
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "wrong_run" => {
                fixture.command.target.run_id = "run-2".into();
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "expired_command" => {
                fixture.facts.now = fixture.command.expires_at;
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "future_issued_command" => {
                fixture.command.issued_at = NOW + 1;
                fixture.command.expires_at = NOW + 2;
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "post_validation_command_expiry" => {
                let validated = validate_command(&fixture)?;
                classify_spent_command(
                    &validated,
                    fixture.command.expires_at,
                    SpentCommandStoreOutcome::AcquiredAndEnqueued,
                )?;
                Ok("unexpected")
            }
            "excessive_command_ttl" => {
                fixture.command.expires_at =
                    fixture.command.issued_at + MAX_TRANSITION_TTL_SECS + 1;
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "cancel_message_smuggle" => {
                fixture.command.steer_message_event_id = Some(message_event_id());
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "steer_missing_message_id" => {
                fixture.command.control = OneShotControlKind::Steer;
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "steer_message_not_durable" => {
                make_steer(&mut fixture);
                fixture.facts.steer_message = None;
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "steer_message_wrong_channel" => {
                make_steer(&mut fixture);
                fixture
                    .facts
                    .steer_message
                    .as_mut()
                    .expect("steer message")
                    .channel_id = Uuid::new_v4();
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "steer_message_cross_tenant" => {
                make_steer(&mut fixture);
                fixture
                    .facts
                    .steer_message
                    .as_mut()
                    .expect("steer message")
                    .community_id = CommunityId::from_uuid(Uuid::new_v4());
                validate_command(&fixture)?;
                Ok("unexpected")
            }
            "unknown_command_field" => {
                let mut value = serde_json::to_value(&fixture.command).expect("serialize");
                value
                    .as_object_mut()
                    .expect("object")
                    .insert("authority".into(), serde_json::json!("admin"));
                let raw = serde_json::to_string(&value).expect("encode");
                let event = signed_raw_frame(
                    &fixture.owner,
                    &fixture.agent.public_key(),
                    &fixture.facts.target.agent_pubkey,
                    OBSERVER_FRAME_CONTROL,
                    fixture.command.issued_at,
                    fixture.facts.target.channel_id,
                    &raw,
                );
                decrypt_and_validate_owner_control(&event, &fixture.agent, &fixture.facts, None)?;
                Ok("unexpected")
            }
            "duplicate_security_field" => {
                let json = serde_json::to_string(&fixture.command).expect("serialize");
                let needle = format!("\"command_id\":\"{}\"", fixture.command.command_id);
                let duplicate = format!("{needle},{needle}");
                let json = json.replacen(&needle, &duplicate, 1);
                let event = signed_raw_frame(
                    &fixture.owner,
                    &fixture.agent.public_key(),
                    &fixture.facts.target.agent_pubkey,
                    OBSERVER_FRAME_CONTROL,
                    fixture.command.issued_at,
                    fixture.facts.target.channel_id,
                    &json,
                );
                decrypt_and_validate_owner_control(&event, &fixture.agent, &fixture.facts, None)?;
                Ok("unexpected")
            }
            "replayed_spent_command" => {
                let validated = validate_command(&fixture)?;
                classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::CommandIdConflict,
                )?;
                Ok("unexpected")
            }
            "exact_duplicate_pending" => {
                let validated = validate_command(&fixture)?;
                match classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::ExactDuplicatePending,
                )? {
                    OneShotCommandDisposition::AwaitOriginalAcknowledgement => {
                        Ok("await_original_ack")
                    }
                    _ => Err(AgentControlError::CommandReplay),
                }
            }
            "exact_duplicate_completed" => {
                let validated = validate_command(&fixture)?;
                match classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::ExactDuplicateCompleted,
                )? {
                    OneShotCommandDisposition::ReturnStoredAcknowledgement => {
                        Ok("return_stored_ack")
                    }
                    _ => Err(AgentControlError::CommandReplay),
                }
            }
            "one_shot_authority_change_before_cas" => {
                let validated = validate_command(&fixture)?;
                let claim = validated.claim();
                assert_eq!(
                    claim.agent_ownership_revision(),
                    fixture.facts.agent_ownership_revision
                );
                assert_eq!(
                    claim.ensure_current_authority(
                        &fixture.facts.operator_pubkey,
                        fixture.facts.agent_ownership_revision + 1,
                    ),
                    Err(AgentControlError::AuthorityConflict)
                );
                classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::AuthorityConflict,
                )?;
                Ok("unexpected")
            }
            "one_shot_authority_change_before_effect" => {
                let validated = validate_command(&fixture)?;
                let claim = validated.claim();
                assert_eq!(
                    classify_spent_command(
                        &validated,
                        NOW,
                        SpentCommandStoreOutcome::AcquiredAndEnqueued,
                    )?,
                    OneShotCommandDisposition::DurablyEnqueued,
                );
                // Model an A -> B -> A ownership transition after the command
                // and outbox row committed but before the external effect.
                claim.ensure_current_authority(
                    &fixture.facts.operator_pubkey,
                    fixture.facts.agent_ownership_revision + 1,
                )?;
                Ok("unexpected")
            }
            "command_store_unavailable" => {
                let validated = validate_command(&fixture)?;
                classify_spent_command(
                    &validated,
                    NOW,
                    SpentCommandStoreOutcome::StoreUnavailable,
                )?;
                Ok("unexpected")
            }
            "valid_command_ack" => {
                let validated = validate_command(&fixture)?;
                let ack = command_ack(&validated, NOW);
                let event = command_ack_event(&fixture, &ack);
                decrypt_and_validate_one_shot_ack(&validated, &event, &fixture.owner)?;
                Ok("ack_valid")
            }
            "forged_command_ack" => {
                let validated = validate_command(&fixture)?;
                let ack = command_ack(&validated, NOW);
                let mut event = command_ack_event(&fixture, &ack);
                event.content.push('A');
                decrypt_and_validate_one_shot_ack(&validated, &event, &fixture.owner)?;
                Ok("unexpected")
            }
            "ack_wrong_command" => {
                let validated = validate_command(&fixture)?;
                let mut ack = command_ack(&validated, NOW);
                ack.command_id = Uuid::new_v4();
                let event = command_ack_event(&fixture, &ack);
                decrypt_and_validate_one_shot_ack(&validated, &event, &fixture.owner)?;
                Ok("unexpected")
            }
            "ack_after_command_expiry" => {
                let validated = validate_command(&fixture)?;
                let ack = command_ack(&validated, fixture.command.expires_at);
                let event = command_ack_event(&fixture, &ack);
                decrypt_and_validate_one_shot_ack(&validated, &event, &fixture.owner)?;
                Ok("unexpected")
            }
            "oversize_excerpt" => {
                let excerpt = LiveActivityExcerpt {
                    text: "x".repeat(MAX_ACTIVITY_EXCERPT_BYTES + 1),
                    truncated: true,
                };
                parse_live_activity_excerpt_json(
                    serde_json::to_vec(&excerpt).expect("encode").as_slice(),
                )?;
                Ok("unexpected")
            }
            "oversize_plaintext" => {
                let mut raw = serde_json::to_string(&fixture.command).expect("serialize");
                raw.push_str(
                    " ".repeat(MAX_CONTROL_PLAINTEXT_BYTES + 1 - raw.len())
                        .as_str(),
                );
                let event = signed_raw_frame(
                    &fixture.owner,
                    &fixture.agent.public_key(),
                    &fixture.facts.target.agent_pubkey,
                    OBSERVER_FRAME_CONTROL,
                    fixture.command.issued_at,
                    fixture.facts.target.channel_id,
                    &raw,
                );
                decrypt_and_validate_owner_control(&event, &fixture.agent, &fixture.facts, None)?;
                Ok("unexpected")
            }
            "valid_pause" => {
                let pause = base_pause(&fixture);
                let validated = validate_pause(&fixture, &pause, None)?;
                match classify_pause_lease_outcome(
                    &validated,
                    NOW,
                    PauseLeaseStoreOutcome::Applied,
                )? {
                    PauseLeaseDisposition::StateChanged => Ok("lease_state_changed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "valid_renew" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let lease = lease_from(&prior, true);
                let renew = next_lease_transition(&pause, PauseLeaseTransitionKind::Renew);
                let validated = validate_pause(&fixture, &renew, Some(&lease))?;
                match classify_pause_lease_outcome(
                    &validated,
                    NOW,
                    PauseLeaseStoreOutcome::Applied,
                )? {
                    PauseLeaseDisposition::StateChanged => Ok("lease_state_changed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "valid_resume" | "resume_after_expiry" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let mut lease = lease_from(&prior, true);
                if case == "resume_after_expiry" {
                    lease.lease_expires_at = NOW;
                }
                let resume = next_lease_transition(&pause, PauseLeaseTransitionKind::Resume);
                let validated = validate_pause(&fixture, &resume, Some(&lease))?;
                match classify_pause_lease_outcome(
                    &validated,
                    NOW,
                    PauseLeaseStoreOutcome::Applied,
                )? {
                    PauseLeaseDisposition::StateChanged => Ok("lease_state_changed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "new_pause_after_resume_changed_target" | "new_pause_after_expiry_changed_target" => {
                let old_pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &old_pause, None)?;
                let mut retained = if case == "new_pause_after_resume_changed_target" {
                    let active = lease_from(&prior, true);
                    let resume =
                        next_lease_transition(&old_pause, PauseLeaseTransitionKind::Resume);
                    let validated_resume = validate_pause(&fixture, &resume, Some(&active))?;
                    lease_from(&validated_resume, false)
                } else {
                    let mut expired = lease_from(&prior, true);
                    expired.lease_expires_at = NOW;
                    expired
                };
                retained.lease_expires_at = retained.lease_expires_at.min(NOW);

                fixture.facts.now = NOW + 1;
                fixture.facts.target.channel_id = Uuid::new_v4();
                fixture.facts.target.run_id = "run-new".into();
                let mut fresh_pause = base_pause(&fixture);
                fresh_pause.issued_at = NOW + 1;
                fresh_pause.transition_expires_at = NOW + 61;
                fresh_pause.lease_expires_at = Some(NOW + 301);
                let validated = validate_pause(&fixture, &fresh_pause, Some(&retained))?;
                assert!(!validated.is_exact_retry());
                match classify_pause_lease_outcome(
                    &validated,
                    NOW + 1,
                    PauseLeaseStoreOutcome::Applied,
                )? {
                    PauseLeaseDisposition::StateChanged => Ok("lease_state_changed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "stale_renew_generation" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let lease = lease_from(&prior, true);
                let mut renew = next_lease_transition(&pause, PauseLeaseTransitionKind::Renew);
                renew.generation += 1;
                validate_pause(&fixture, &renew, Some(&lease))?;
                Ok("unexpected")
            }
            "lease_scope_mismatch" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let mut lease = lease_from(&prior, true);
                lease.target.computer_id = "computer-2".into();
                let renew = next_lease_transition(&pause, PauseLeaseTransitionKind::Renew);
                validate_pause(&fixture, &renew, Some(&lease))?;
                Ok("unexpected")
            }
            "lease_channel_shift" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let mut lease = lease_from(&prior, true);
                lease.target.channel_id = Uuid::new_v4();
                let renew = next_lease_transition(&pause, PauseLeaseTransitionKind::Renew);
                validate_pause(&fixture, &renew, Some(&lease))?;
                Ok("unexpected")
            }
            "lease_run_shift" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let mut lease = lease_from(&prior, true);
                lease.target.run_id = "run-other".into();
                let renew = next_lease_transition(&pause, PauseLeaseTransitionKind::Renew);
                validate_pause(&fixture, &renew, Some(&lease))?;
                Ok("unexpected")
            }
            "overlong_pause_lease" => {
                let mut pause = base_pause(&fixture);
                pause.lease_expires_at = Some(pause.issued_at + MAX_PAUSE_LEASE_SECS + 1);
                validate_pause(&fixture, &pause, None)?;
                Ok("unexpected")
            }
            "resume_missing_lease" => {
                let pause = base_pause(&fixture);
                let resume = next_lease_transition(&pause, PauseLeaseTransitionKind::Resume);
                validate_pause(&fixture, &resume, None)?;
                Ok("unexpected")
            }
            "expired_lease_auto_resume" => {
                let pause = base_pause(&fixture);
                let prior = validate_pause(&fixture, &pause, None)?;
                let mut lease = lease_from(&prior, true);
                lease.lease_expires_at = NOW;
                match effective_pause_state(
                    &lease,
                    NOW,
                    &fixture.facts.operator_pubkey,
                    fixture.facts.agent_ownership_revision,
                )? {
                    EffectivePauseState::ExpiredMustRelease => Ok("expired_must_release"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "post_validation_lease_transition_expiry" => {
                let pause = base_pause(&fixture);
                let validated = validate_pause(&fixture, &pause, None)?;
                classify_pause_lease_outcome(
                    &validated,
                    pause.transition_expires_at,
                    PauseLeaseStoreOutcome::Applied,
                )?;
                Ok("unexpected")
            }
            "pause_exact_duplicate" => {
                let pause = base_pause(&fixture);
                let first = validate_pause(&fixture, &pause, None)?;
                let lease = lease_from(&first, true);
                let retry = validate_pause(&fixture, &pause, Some(&lease))?;
                match classify_pause_lease_outcome(
                    &retry,
                    NOW,
                    PauseLeaseStoreOutcome::ExactDuplicate,
                )? {
                    PauseLeaseDisposition::DuplicateSuppressed => Ok("duplicate_suppressed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "pause_concurrent_exact_duplicate" => {
                let pause = base_pause(&fixture);
                let validated = validate_pause(&fixture, &pause, None)?;
                assert!(!validated.is_exact_retry());
                match classify_pause_lease_outcome(
                    &validated,
                    NOW,
                    PauseLeaseStoreOutcome::ExactDuplicate,
                )? {
                    PauseLeaseDisposition::DuplicateSuppressed => Ok("duplicate_suppressed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "pause_replay_after_intervening_lease" => {
                // Lease A is paused and resumed, then replaced by a complete
                // pause/resume cycle for lease B on another run.
                let pause_a = base_pause(&fixture);
                let validated_pause_a = validate_pause(&fixture, &pause_a, None)?;
                let active_a = lease_from(&validated_pause_a, true);
                let resume_a = next_lease_transition(&pause_a, PauseLeaseTransitionKind::Resume);
                let validated_resume_a = validate_pause(&fixture, &resume_a, Some(&active_a))?;
                let inactive_a = lease_from(&validated_resume_a, false);

                fixture.facts.target.channel_id = Uuid::new_v4();
                fixture.facts.target.run_id = "run-b".into();
                let pause_b = base_pause(&fixture);
                let validated_pause_b = validate_pause(&fixture, &pause_b, Some(&inactive_a))?;
                let active_b = lease_from(&validated_pause_b, true);
                let resume_b = next_lease_transition(&pause_b, PauseLeaseTransitionKind::Resume);
                let validated_resume_b = validate_pause(&fixture, &resume_b, Some(&active_b))?;
                let inactive_b = lease_from(&validated_resume_b, false);

                // Stateless/current-row validation alone sees the still-fresh
                // replay of A as a possible new pause. The durable transition
                // tombstone must recognize it without changing lease B's row.
                fixture.facts.target = pause_a.target.clone();
                let replay = validate_pause(&fixture, &pause_a, Some(&inactive_b))?;
                assert!(!replay.is_exact_retry());
                let claim = replay.claim();
                assert_eq!(claim.community_id(), fixture.facts.community_id);
                assert_eq!(claim.transition_id(), pause_a.transition_id);
                assert_eq!(claim.lease_id(), pause_a.lease_id);
                assert_eq!(claim.fingerprint(), replay.fingerprint());
                assert_eq!(claim.operator_pubkey(), pause_a.operator_pubkey);
                assert_eq!(claim.target(), &pause_a.target);
                assert_eq!(claim.transition_expires_at(), pause_a.transition_expires_at);
                match classify_pause_lease_outcome(
                    &replay,
                    NOW,
                    PauseLeaseStoreOutcome::ExactDuplicate,
                )? {
                    PauseLeaseDisposition::DuplicateSuppressed => Ok("duplicate_suppressed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "pause_replay_after_expiry" => {
                let pause = base_pause(&fixture);
                let first = validate_pause(&fixture, &pause, None)?;
                let mut lease = lease_from(&first, false);
                lease.lease_expires_at = NOW;
                assert_eq!(
                    validate_pause(&fixture, &pause, Some(&lease)),
                    Err(AgentControlError::LeaseConflict)
                );
                classify_pause_lease_outcome(&first, NOW, PauseLeaseStoreOutcome::LeaseConflict)?;
                Ok("unexpected")
            }
            "pause_authority_change_release_then_new_owner_pause" => {
                let pause_a = base_pause(&fixture);
                let validated_a = validate_pause(&fixture, &pause_a, None)?;
                let mut active_a = lease_from(&validated_a, true);
                assert_eq!(
                    active_a.agent_ownership_revision,
                    fixture.facts.agent_ownership_revision
                );

                std::mem::swap(&mut fixture.owner, &mut fixture.other);
                fixture.facts.operator_pubkey = fixture.owner.public_key().to_hex();
                fixture.facts.agent_ownership_revision += 1;
                let pause_b = base_pause(&fixture);

                assert_eq!(
                    effective_pause_state(
                        &active_a,
                        NOW,
                        &fixture.facts.operator_pubkey,
                        fixture.facts.agent_ownership_revision,
                    )?,
                    EffectivePauseState::AuthorityChangedMustRelease
                );
                // New authority cannot replace an active stale-authority hold;
                // the required release CAS and its audit record come first.
                assert_eq!(
                    validate_pause(&fixture, &pause_b, Some(&active_a)),
                    Err(AgentControlError::LeaseConflict)
                );

                // Model the integration's atomic running-state + audit commit.
                active_a.active = false;
                assert_eq!(
                    effective_pause_state(
                        &active_a,
                        NOW,
                        &fixture.facts.operator_pubkey,
                        fixture.facts.agent_ownership_revision,
                    )?,
                    EffectivePauseState::Running
                );
                let validated_b = validate_pause(&fixture, &pause_b, Some(&active_a))?;
                assert!(!validated_b.is_exact_retry());
                assert_eq!(
                    validated_b.claim().agent_ownership_revision(),
                    fixture.facts.agent_ownership_revision
                );
                match classify_pause_lease_outcome(
                    &validated_b,
                    NOW,
                    PauseLeaseStoreOutcome::Applied,
                )? {
                    PauseLeaseDisposition::StateChanged => Ok("lease_state_changed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "pause_authority_aba_release_then_fresh_pause" => {
                let pause_a = base_pause(&fixture);
                let validated_a = validate_pause(&fixture, &pause_a, None)?;
                let mut active_a = lease_from(&validated_a, true);
                let original_operator = fixture.facts.operator_pubkey.clone();

                // Model A -> B -> A: the owner pubkey is unchanged at the
                // observation points, but the monotonic revision advanced.
                fixture.facts.agent_ownership_revision += 1;
                assert_eq!(fixture.facts.operator_pubkey, original_operator);
                let pause_b = base_pause(&fixture);
                assert_eq!(
                    effective_pause_state(
                        &active_a,
                        NOW,
                        &fixture.facts.operator_pubkey,
                        fixture.facts.agent_ownership_revision,
                    )?,
                    EffectivePauseState::AuthorityChangedMustRelease
                );
                assert_eq!(
                    validate_pause(&fixture, &pause_b, Some(&active_a)),
                    Err(AgentControlError::LeaseConflict),
                    "fresh pause cannot bypass the stale-authority release CAS"
                );

                // Model the integration's atomic running-state + audit commit,
                // then prove a fresh pause binds the new revision.
                active_a.active = false;
                assert_eq!(
                    effective_pause_state(
                        &active_a,
                        NOW,
                        &fixture.facts.operator_pubkey,
                        fixture.facts.agent_ownership_revision,
                    )?,
                    EffectivePauseState::Running
                );
                let validated_b = validate_pause(&fixture, &pause_b, Some(&active_a))?;
                assert_eq!(
                    validated_b.claim().agent_ownership_revision(),
                    fixture.facts.agent_ownership_revision
                );
                match classify_pause_lease_outcome(
                    &validated_b,
                    NOW,
                    PauseLeaseStoreOutcome::Applied,
                )? {
                    PauseLeaseDisposition::StateChanged => Ok("lease_state_changed"),
                    _ => Err(AgentControlError::LeaseConflict),
                }
            }
            "pause_authority_change_before_cas" => {
                let pause = base_pause(&fixture);
                let validated = validate_pause(&fixture, &pause, None)?;
                let claim = validated.claim();
                assert_eq!(
                    claim.agent_ownership_revision(),
                    fixture.facts.agent_ownership_revision
                );
                assert_eq!(
                    claim.ensure_current_authority(
                        &fixture.facts.operator_pubkey,
                        fixture.facts.agent_ownership_revision + 1,
                    ),
                    Err(AgentControlError::AuthorityConflict)
                );
                classify_pause_lease_outcome(
                    &validated,
                    NOW,
                    PauseLeaseStoreOutcome::AuthorityConflict,
                )?;
                Ok("unexpected")
            }
            "pause_store_unavailable" => {
                let pause = base_pause(&fixture);
                let validated = validate_pause(&fixture, &pause, None)?;
                classify_pause_lease_outcome(
                    &validated,
                    NOW,
                    PauseLeaseStoreOutcome::StoreUnavailable,
                )?;
                Ok("unexpected")
            }
            "valid_pause_ack" | "forged_pause_ack" => {
                let pause = base_pause(&fixture);
                let validated = validate_pause(&fixture, &pause, None)?;
                let ack = pause_ack(&validated, NOW);
                let mut event = pause_ack_event(&fixture, &ack);
                if case == "forged_pause_ack" {
                    event.content.push('A');
                }
                decrypt_and_validate_pause_lease_ack(&validated, &event, &fixture.owner)?;
                Ok("ack_valid")
            }
            name => panic!("fixture executor missing case: {name}"),
        }
    }

    #[test]
    fn shared_malicious_fixture_manifest_is_executable_and_complete() {
        let manifest: FixtureManifest =
            serde_json::from_str(include_str!("../../../docs/nips/NIP-AO.fixtures.json"))
                .expect("valid fixture manifest");
        assert_eq!(manifest.format, "buzz-nip-ao-fixtures");
        assert_eq!(manifest.version, VERSION);
        assert_eq!(manifest.community_id, community_id().to_string());
        assert_eq!(manifest.cases.len(), 55);

        let mut names = HashSet::new();
        for fixture in &manifest.cases {
            assert!(
                names.insert(fixture.name.as_str()),
                "duplicate fixture name"
            );
            match observe(&fixture.name) {
                Ok(actual) => {
                    assert_eq!(actual, fixture.expect, "fixture {}", fixture.name);
                    assert!(fixture.error_code.is_none(), "success fixture has error");
                }
                Err(error) => {
                    assert_eq!(fixture.expect, "reject", "fixture {}", fixture.name);
                    assert_eq!(
                        Some(error.code()),
                        fixture.error_code.as_deref(),
                        "fixture {}: {error}",
                        fixture.name
                    );
                }
            }
        }
    }

    #[test]
    fn command_fingerprint_is_tenant_bound() {
        let fixture = one_shot_fixture();
        let other_community = CommunityId::from_uuid(Uuid::new_v4());
        assert_ne!(
            one_shot_command_fingerprint(fixture.community_id, &fixture.command).unwrap(),
            one_shot_command_fingerprint(other_community, &fixture.command).unwrap()
        );
    }

    #[test]
    fn authoritative_exact_duplicate_closes_precommit_validation_race() {
        let fixture = one_shot_fixture();
        let pause = base_pause(&fixture);
        let validated = validate_pause(&fixture, &pause, None).unwrap();
        assert!(!validated.is_exact_retry());
        assert_eq!(
            classify_pause_lease_outcome(&validated, NOW, PauseLeaseStoreOutcome::ExactDuplicate),
            Ok(PauseLeaseDisposition::DuplicateSuppressed)
        );
    }
}
