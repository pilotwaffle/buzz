//! Versioned, fail-closed contracts for operator-approved agent delegation.
//!
//! This module is intentionally zero-I/O. It validates signed approval evidence
//! and returns an opaque statelessly validated context. Replay safety is a
//! separate, durable atomic-claim step represented by [`classify_claim_outcome`];
//! schema validation alone cannot make an at-least-once dispatch replay-safe.

use std::collections::HashSet;

use nostr::{Event, EventBuilder, EventId, Keys, Kind, PublicKey, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::kind::KIND_DELEGATION_APPROVAL;
use crate::verify_event;
use crate::CommunityId;

/// Wire discriminator for relay-persisted delegation records.
pub const RECORD_FORMAT: &str = "buzz-delegation-record";
/// Wire discriminator for execution contexts carried to a target agent.
pub const CONTEXT_FORMAT: &str = "buzz-delegation-context";
/// Wire discriminator for operator-signed delegation approvals.
pub const APPROVAL_FORMAT: &str = "buzz-delegation-approval";
/// Current delegation contract version.
pub const VERSION: u32 = 1;
/// Initial-release default hop budget.
pub const DEFAULT_HOP_BUDGET: u8 = 1;
/// Initial-release hard hop limit.
pub const MAX_HOP_BUDGET: u8 = 2;
/// Maximum UTF-8 byte length of an idempotency key.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

const REQUEST_HASH_DOMAIN: &[u8] = b"buzz-delegation/request/v2\0";

/// Immutable, task-body-free fields approved for a delegation.
///
/// The task body remains in the encrypted origin event. This request stores
/// only its event id and bounded routing/constraint metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationRequest {
    /// Stable delegation id.
    pub delegation_id: Uuid,
    /// Encrypted originating message event id.
    pub origin_event_id: String,
    /// Approval event for the currently executing parent delegation, if this
    /// is a nested hop. Direct/root delegations must use `None`.
    pub parent_approval_event_id: Option<String>,
    /// Agent asking another agent to take the work.
    pub source_agent: String,
    /// Agent that would receive the work.
    pub target_agent: String,
    /// Ordered, same-owner agent path, including source and target.
    ///
    /// A direct delegation is `[source, target]`; a second hop is
    /// `[root, source, target]`. Repeated entries are cycles and are rejected.
    pub agent_path: Vec<String>,
    /// Maximum number of delegation edges in `agent_path`.
    pub hop_budget: u8,
    /// Maximum target-agent turns approved for the request.
    pub max_turns: u32,
    /// Optional maximum cost in integer millionths of a US dollar.
    pub cost_cap_microusd: Option<u64>,
    /// Total token budget approved for this delegation's lifetime. Required;
    /// zero is rejected by [`DelegationRequest::validate`] \(Slice 4, v2\).
    pub token_budget: u64,
    /// Durable, caller-selected duplicate-suppression key.
    pub idempotency_key: String,
    /// Unix-seconds deadline; the request is expired when `now >= expires_at`.
    pub expires_at: u64,
}

/// Relay-persisted metadata for a delegation lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationRecord {
    /// Always [`RECORD_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Immutable request projection. It never contains a task body.
    pub request: DelegationRequest,
    /// Lowercase SHA-256 of the domain-separated immutable request projection.
    pub immutable_request_hash: String,
    /// Signed operator approval event id, once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_approval_event_id: Option<String>,
    /// Current lifecycle state.
    pub state: DelegationState,
    /// Attributed target answer event id, once delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_event_id: Option<String>,
    /// Relay creation time in Unix seconds.
    pub created_at: u64,
    /// Relay update time in Unix seconds.
    pub updated_at: u64,
}

impl DelegationRecord {
    /// Construct a validated offered record and compute its immutable hash.
    pub fn new_offered(
        community_id: CommunityId,
        request: DelegationRequest,
        created_at: u64,
    ) -> Result<Self, DelegationError> {
        let immutable_request_hash = immutable_request_hash(community_id, &request)?;
        let record = Self {
            format: RECORD_FORMAT.to_owned(),
            version: VERSION,
            request,
            immutable_request_hash,
            operator_approval_event_id: None,
            state: DelegationState::Offered,
            answer_event_id: None,
            created_at,
            updated_at: created_at,
        };
        record.validate(community_id)?;
        Ok(record)
    }

    /// Validate record format, immutable fields, hash, timestamps, and state.
    pub fn validate(&self, community_id: CommunityId) -> Result<(), DelegationError> {
        validate_discriminator(&self.format, RECORD_FORMAT, self.version)?;
        self.request.validate()?;
        validate_hash("immutable_request_hash", &self.immutable_request_hash)?;
        if immutable_request_hash(community_id, &self.request)? != self.immutable_request_hash {
            return Err(DelegationError::InvalidRequestHash);
        }
        if self.updated_at < self.created_at || self.request.expires_at <= self.created_at {
            return Err(DelegationError::InvalidTimestamp);
        }
        if let Some(event_id) = self.operator_approval_event_id.as_deref() {
            parse_event_id("operator_approval_event_id", event_id)?;
        }
        if let Some(event_id) = self.answer_event_id.as_deref() {
            parse_event_id("answer_event_id", event_id)?;
        }
        match self.state {
            DelegationState::Offered => {
                if self.operator_approval_event_id.is_some() || self.answer_event_id.is_some() {
                    return Err(DelegationError::InvalidState);
                }
            }
            DelegationState::Approved => {
                if self.operator_approval_event_id.is_none() || self.answer_event_id.is_some() {
                    return Err(DelegationError::InvalidState);
                }
            }
            DelegationState::Delivered => {
                if self.operator_approval_event_id.is_none() || self.answer_event_id.is_none() {
                    return Err(DelegationError::InvalidState);
                }
            }
            DelegationState::Refused | DelegationState::Failed | DelegationState::Expired => {
                if self.answer_event_id.is_some() {
                    return Err(DelegationError::InvalidState);
                }
            }
        }
        Ok(())
    }
}

/// Delegation lifecycle states persisted by the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationState {
    /// Awaiting operator approval.
    Offered,
    /// Approved and eligible for a durable claim.
    Approved,
    /// Refused before execution.
    Refused,
    /// Target answer relayed successfully.
    Delivered,
    /// Terminal execution or delivery failure.
    Failed,
    /// Approval/request deadline elapsed.
    Expired,
}

/// Constraint/provenance envelope that must accompany target execution.
///
/// This type contains no grants, scopes, tool permissions, or path authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationExecutionContext {
    /// Always [`CONTEXT_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Exact immutable request from the stored record.
    pub request: DelegationRequest,
    /// Exact immutable request hash from the stored record.
    pub immutable_request_hash: String,
    /// Exact operator approval event id from the stored record.
    pub operator_approval_event_id: String,
    /// Number of edges already traversed in `request.agent_path`.
    pub hop_count: u8,
    /// Remaining target turns. It may attenuate, never exceed, `max_turns`.
    pub remaining_turns: u32,
}

impl DelegationExecutionContext {
    /// Construct a context from an approved record.
    pub fn from_approved_record(
        community_id: CommunityId,
        record: &DelegationRecord,
        remaining_turns: u32,
    ) -> Result<Self, DelegationError> {
        record.validate(community_id)?;
        if record.state != DelegationState::Approved {
            return Err(DelegationError::InvalidState);
        }
        let approval_id = record
            .operator_approval_event_id
            .clone()
            .ok_or(DelegationError::MissingApproval)?;
        let hop_count = u8::try_from(record.request.agent_path.len() - 1)
            .map_err(|_| DelegationError::HopBudgetExceeded)?;
        let context = Self {
            format: CONTEXT_FORMAT.to_owned(),
            version: VERSION,
            request: record.request.clone(),
            immutable_request_hash: record.immutable_request_hash.clone(),
            operator_approval_event_id: approval_id,
            hop_count,
            remaining_turns,
        };
        context.validate_shape()?;
        Ok(context)
    }

    fn validate_shape(&self) -> Result<(), DelegationError> {
        validate_discriminator(&self.format, CONTEXT_FORMAT, self.version)?;
        self.request.validate()?;
        validate_hash("immutable_request_hash", &self.immutable_request_hash)?;
        parse_event_id(
            "operator_approval_event_id",
            &self.operator_approval_event_id,
        )?;
        let path_hops = u8::try_from(self.request.agent_path.len() - 1)
            .map_err(|_| DelegationError::HopBudgetExceeded)?;
        if self.hop_count != path_hops || self.hop_count > self.request.hop_budget {
            return Err(DelegationError::HopBudgetExceeded);
        }
        if self.remaining_turns == 0 || self.remaining_turns > self.request.max_turns {
            return Err(DelegationError::TurnLimitExceeded);
        }
        Ok(())
    }
}

/// Strict content of the operator-signed delegation approval event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationApproval {
    /// Always [`APPROVAL_FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// Approved delegation id.
    pub delegation_id: Uuid,
    /// Approved immutable request hash.
    pub immutable_request_hash: String,
    /// Signed request deadline, also included in the immutable hash.
    pub expires_at: u64,
}

impl DelegationApproval {
    /// Build an approval payload bound to an immutable request.
    pub fn for_request(
        community_id: CommunityId,
        request: &DelegationRequest,
    ) -> Result<Self, DelegationError> {
        Ok(Self {
            format: APPROVAL_FORMAT.to_owned(),
            version: VERSION,
            delegation_id: request.delegation_id,
            immutable_request_hash: immutable_request_hash(community_id, request)?,
            expires_at: request.expires_at,
        })
    }

    fn validate(&self) -> Result<(), DelegationError> {
        validate_discriminator(&self.format, APPROVAL_FORMAT, self.version)?;
        validate_hash("immutable_request_hash", &self.immutable_request_hash)?;
        Ok(())
    }
}

/// Server-resolved ownership for one agent at validation time.
///
/// This is deliberately not deserializable from the execution context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentOwner {
    /// Agent whose owner was resolved in the current tenant.
    pub agent_pubkey: String,
    /// Current owner, or `None` when absent/not visible/not owner-bound.
    pub owner_pubkey: Option<String>,
    /// Monotonic revision of this agent's ownership binding. It must advance on
    /// every owner or visibility transition, including an A -> B -> A change.
    pub ownership_revision: u64,
}

/// Trusted runtime facts resolved outside the wire context.
///
/// Callers must resolve every agent in `request.agent_path` from the current
/// tenant immediately before validation. Missing and invisible agents therefore
/// take the same denial path as wrong-owner agents.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedDelegationFacts {
    /// Server-resolved tenant. It never comes from the delegation wire payload.
    pub community_id: CommunityId,
    /// Current Unix time.
    pub now: u64,
    /// Current owner snapshots for all path agents.
    pub agent_owners: Vec<ResolvedAgentOwner>,
    /// Server-derived lineage state for the currently executing source.
    pub lineage: ResolvedDelegationLineage,
}

/// Opaque lineage state derived from the currently executing durable run.
///
/// Slice 0 intentionally exposes no constructor. Slice 4 may mint it only from
/// its private live execution permit, never from stateless validation or wire
/// data. Root, parent, and unavailable are distinct internal states; unavailable
/// always fails closed.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedDelegationLineage {
    kind: ResolvedDelegationLineageKind,
}

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum ResolvedDelegationLineageKind {
    Root(ResolvedDelegationRoot),
    Parent(ResolvedDelegationParent),
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
struct ResolvedDelegationRoot {
    community_id: CommunityId,
    run_id: Uuid,
    source_agent: String,
    operator_pubkey: String,
    expires_at: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct ResolvedDelegationParent {
    community_id: CommunityId,
    run_id: Uuid,
    delegation_id: Uuid,
    /// Parent approval event id.
    approval_event_id: String,
    immutable_request_hash: String,
    operator_pubkey: String,
    expires_at: u64,
    /// Parent's already-validated ordered agent path.
    agent_path: Vec<String>,
}

/// Current, authoritative facts required before every target action.
///
/// These values must come from durable runtime state, not from the agent-carried
/// context. The action-state update (turn decrement and cost increment) belongs
/// in the same transaction as recording/dispatching the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDelegationActionFacts {
    /// Fresh server-resolved tenant for this action.
    pub community_id: CommunityId,
    /// Current Unix time.
    pub now: u64,
    /// Fresh current-owner snapshots for every path agent.
    pub agent_owners: Vec<ResolvedAgentOwner>,
    /// Durable remaining-turn count before the action.
    pub remaining_turns: u32,
    /// Settled cost plus outstanding reservations before the action.
    pub cost_committed_microusd: Option<u64>,
    /// Conservative cost reservation for the next action.
    pub action_cost_reservation_microusd: Option<u64>,
}

/// A statelessly validated context, not yet authorized for execution.
///
/// The private fields prevent raw deserialized contexts from being mistaken for
/// validated evidence. A durable atomic claim is still required.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedDelegationContext {
    context: DelegationExecutionContext,
    community_id: CommunityId,
    operator_pubkey: String,
}

impl ValidatedDelegationContext {
    /// Return the validated wire context.
    pub fn context(&self) -> &DelegationExecutionContext {
        &self.context
    }

    /// Return the verified operator signer.
    pub fn operator_pubkey(&self) -> &str {
        &self.operator_pubkey
    }

    /// Build the three-key claim that a durable store must atomically acquire.
    pub fn claim(&self) -> DelegationClaim {
        DelegationClaim {
            community_id: self.community_id,
            operator_pubkey: self.operator_pubkey.clone(),
            source_agent: self.context.request.source_agent.clone(),
            delegation_id: self.context.request.delegation_id,
            approval_event_id: self.context.operator_approval_event_id.clone(),
            immutable_request_hash: self.context.immutable_request_hash.clone(),
            idempotency_key: self.context.request.idempotency_key.clone(),
            expires_at: self.context.request.expires_at,
        }
    }
}

/// Opaque proof that the current target action passed fresh runtime checks.
///
/// The action dispatcher should consume this value in a tenant-scoped CAS that
/// atomically decrements the durable turn counter, adds the conservative cost
/// reservation, and records the action before dispatch.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedDelegationAction {
    community_id: CommunityId,
    delegation_id: Uuid,
    approval_event_id: String,
    immutable_request_hash: String,
    expires_at: u64,
    owner_snapshot: Vec<ResolvedAgentOwner>,
    remaining_turns_before: u32,
    cost_committed_before_microusd: Option<u64>,
    cost_reservation_microusd: Option<u64>,
    projected_committed_cost_microusd: Option<u64>,
}

impl ValidatedDelegationAction {
    /// Server-resolved tenant that must scope the atomic action update.
    pub fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Delegation authorized for this action.
    pub fn delegation_id(&self) -> Uuid {
        self.delegation_id
    }

    /// Exact approval claim identity that the CAS must match.
    pub fn approval_event_id(&self) -> &str {
        &self.approval_event_id
    }

    /// Exact immutable request identity that the CAS must match.
    pub fn immutable_request_hash(&self) -> &str {
        &self.immutable_request_hash
    }

    /// Signed action deadline.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Recheck transaction time immediately before the CAS commits.
    pub fn ensure_fresh_at(&self, transaction_now: u64) -> Result<(), DelegationError> {
        if transaction_now >= self.expires_at {
            return Err(DelegationError::Expired);
        }
        Ok(())
    }

    /// Exact ordered ownership revisions that the action transaction and its
    /// outbox dispatcher must re-resolve before committing or causing effects.
    pub fn owner_snapshot(&self) -> &[ResolvedAgentOwner] {
        &self.owner_snapshot
    }

    /// Reject an ownership or visibility change, including an A -> B -> A ABA
    /// transition, against freshly resolved and locked ownership rows.
    ///
    /// The authoritative store must call this while obtaining the action CAS;
    /// the outbox dispatcher must repeat it immediately before external effects.
    pub fn ensure_owner_snapshot(
        &self,
        current: &[ResolvedAgentOwner],
    ) -> Result<(), DelegationError> {
        validate_owner_snapshot_unchanged(&self.owner_snapshot, current)
            .map_err(|_| DelegationError::ActionConflict)
    }

    /// Durable remaining-turn count that must be atomically decremented.
    pub fn remaining_turns_before(&self) -> u32 {
        self.remaining_turns_before
    }

    /// Durable committed cost expected by the compare-and-swap update.
    pub fn cost_committed_before_microusd(&self) -> Option<u64> {
        self.cost_committed_before_microusd
    }

    /// Conservative reservation to add before dispatch.
    pub fn cost_reservation_microusd(&self) -> Option<u64> {
        self.cost_reservation_microusd
    }

    /// Projected committed cost after the reservation.
    pub fn projected_committed_cost_microusd(&self) -> Option<u64> {
        self.projected_committed_cost_microusd
    }
}

/// Reference outcome for the authoritative pre-action compare-and-swap transaction.
///
/// The transaction must match the exact claim identity and expected turn/cost
/// state carried by [`ValidatedDelegationAction`], decrement one turn, reserve
/// bounded cost, and record the action atomically before reporting success.
/// This crate-private enum exists only for the reference classifier and tests;
/// constructing a value is not proof that I/O ran. Slice 4 must co-locate a
/// sealed store adapter and private execution permit with the transaction and
/// outbox rows that establish success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum DelegationActionStoreOutcome {
    /// Exact claim/state matched and the action row plus reservation were committed.
    AppliedAndRecorded,
    /// The durable claim was absent or its tenant/request/approval binding differed.
    ClaimConflict,
    /// The expected remaining-turn or committed-cost state was stale.
    StateConflict,
    /// Locked ownership rows no longer matched the action's revisions.
    AuthorityConflict,
    /// Durable action state could not be read or committed.
    StoreUnavailable,
}

/// Crate-private reference result after classifying a pre-action transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum DelegationActionDisposition {
    /// Dispatch only the action row that the transaction durably recorded.
    DurablyRecorded,
}

/// Durable three-key claim input for delegation, approval, and idempotency use.
///
/// A store must atomically reserve `(community_id, delegation_id)`,
/// `(community_id, approval_event_id)`, and `(community_id, operator_pubkey,
/// source_agent, idempotency_key)` together with durable enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationClaim {
    /// Server-resolved tenant namespace for both durable uniqueness keys.
    community_id: CommunityId,
    /// Verified approval signer namespace for caller-chosen idempotency keys.
    operator_pubkey: String,
    /// Verified source-agent namespace for caller-chosen idempotency keys.
    source_agent: String,
    /// Delegation id associated with the claim.
    delegation_id: Uuid,
    /// Approval event id; it may authorize only this immutable request.
    approval_event_id: String,
    /// Approved request hash.
    immutable_request_hash: String,
    /// At-least-once duplicate-suppression key.
    idempotency_key: String,
    /// Earliest instant at which claim retention may end.
    expires_at: u64,
}

impl DelegationClaim {
    /// Server-resolved tenant namespace.
    pub fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Verified operator signer.
    pub fn operator_pubkey(&self) -> &str {
        &self.operator_pubkey
    }

    /// Verified source agent that selected the idempotency key.
    pub fn source_agent(&self) -> &str {
        &self.source_agent
    }

    /// Delegation id associated with the claim.
    pub fn delegation_id(&self) -> Uuid {
        self.delegation_id
    }

    /// Approval event id used for approval uniqueness.
    pub fn approval_event_id(&self) -> &str {
        &self.approval_event_id
    }

    /// Immutable request hash bound by the approval.
    pub fn immutable_request_hash(&self) -> &str {
        &self.immutable_request_hash
    }

    /// Caller-selected key, scoped by tenant, operator, and source agent.
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// Claim rows must be retained through this Unix-seconds deadline.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Recheck transaction time before reserving keys and committing enqueue.
    pub fn ensure_fresh_at(&self, transaction_now: u64) -> Result<(), DelegationError> {
        if transaction_now >= self.expires_at {
            return Err(DelegationError::Expired);
        }
        Ok(())
    }
}

/// Reference outcome for the future durable, atomic claim transaction.
///
/// This crate-private type is executable contract scaffolding, never a
/// serialized field or proof that the future store transaction ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum AtomicClaimOutcome {
    /// All three keys were acquired and a durable work item/outbox row was
    /// committed in the same transaction.
    AcquiredAndEnqueued,
    /// The exact claim already has durable pending work; suppress this submit.
    ExactDuplicatePending,
    /// The exact claim already completed; suppress this submit.
    ExactDuplicateCompleted,
    /// The approval event was already bound to different claim data.
    ApprovalConflict,
    /// The delegation id was already bound to different claim data.
    DelegationConflict,
    /// The idempotency key was already bound to different claim data.
    IdempotencyConflict,
    /// The authoritative claim store could not prove/acquire the claim.
    StoreUnavailable,
}

/// Crate-private reference classification of an atomic-claim result.
///
/// This type is not an execution permit. Slice 4 must create its own private
/// permit only inside the durable claim transaction boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum DelegationClaimDisposition {
    /// Claim and durable work item were committed atomically.
    DurablyEnqueued,
    /// Exact at-least-once retry; do not execute again.
    DuplicateSuppressed(DelegationClaim),
}

/// Stable, fail-closed delegation validation errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DelegationError {
    /// JSON does not match the strict schema.
    #[error("invalid delegation schema: {0}")]
    InvalidSchema(String),
    /// Wire discriminator is not the expected format.
    #[error("invalid delegation format")]
    InvalidFormat,
    /// Wire version is not supported.
    #[error("unsupported delegation version")]
    UnsupportedVersion,
    /// A canonical id, pubkey, hash, path, or idempotency field is malformed.
    #[error("invalid delegation field: {0}")]
    InvalidField(&'static str),
    /// Record hash does not match the immutable request projection.
    #[error("delegation immutable request hash mismatch")]
    InvalidRequestHash,
    /// Lifecycle timestamps are inconsistent.
    #[error("invalid delegation timestamp")]
    InvalidTimestamp,
    /// Record lifecycle state is inconsistent with its fields.
    #[error("invalid delegation state")]
    InvalidState,
    /// Required execution context was absent.
    #[error("missing delegation execution context")]
    MissingContext,
    /// Required operator approval evidence was absent.
    #[error("missing delegation approval")]
    MissingApproval,
    /// Approval event id or Schnorr signature is invalid.
    #[error("invalid delegation approval signature")]
    InvalidApprovalSignature,
    /// Approval event kind/tags/content are malformed.
    #[error("invalid delegation approval envelope: {0}")]
    InvalidApprovalEnvelope(String),
    /// Approval is signed but does not bind the stored request/context.
    #[error("delegation approval binding mismatch")]
    ApprovalBindingMismatch,
    /// Context differs from the authoritative stored record.
    #[error("delegation context binding mismatch")]
    ContextBindingMismatch,
    /// Nested request does not extend the server-derived parent lineage.
    #[error("delegation parent binding mismatch")]
    ParentBindingMismatch,
    /// Action facts were resolved for a different server-trusted tenant.
    #[error("delegation tenant mismatch")]
    TenantMismatch,
    /// Source, target, or ancestry does not resolve to the approval signer.
    #[error("delegation owner mismatch")]
    OwnerMismatch,
    /// Request has reached or passed its signed deadline.
    #[error("delegation approval expired")]
    Expired,
    /// Agent ancestry contains a cycle.
    #[error("delegation cycle detected")]
    DelegationCycle,
    /// Hop count or budget is invalid/exhausted.
    #[error("delegation hop budget exceeded")]
    HopBudgetExceeded,
    /// Remaining turn limit is zero or escalates the approved maximum.
    #[error("delegation turn limit exceeded")]
    TurnLimitExceeded,
    /// A cost cap exists but a conservative action reservation is unknown.
    #[error("delegation action cost is unknown")]
    CostUnknown,
    /// The reservation overflows or exceeds the approved cap.
    #[error("delegation cost limit exceeded")]
    CostLimitExceeded,
    /// Approval or idempotency evidence conflicts with a durable prior claim.
    #[error("delegation approval replayed with conflicting claim data")]
    ApprovalReplay,
    /// The pre-action transaction did not match the exact durable claim and state.
    #[error("delegation action state conflict")]
    ActionConflict,
    /// The delegation's remaining token budget is exhausted.
    #[error("delegation token budget exhausted")]
    BudgetExhausted,
    /// Durable authority/claim state could not be proven.
    #[error("delegation authority store unavailable")]
    AuthorityUnavailable,
    /// Approval event could not be signed.
    #[error("delegation approval signing failed")]
    ApprovalSign,
}

impl DelegationError {
    /// Stable machine-readable error code for audit fixtures and callers.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSchema(_) => "invalid_schema",
            Self::InvalidFormat => "invalid_format",
            Self::UnsupportedVersion => "unsupported_version",
            Self::InvalidField(_) => "invalid_field",
            Self::InvalidRequestHash => "invalid_request_hash",
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::InvalidState => "invalid_state",
            Self::MissingContext => "missing_context",
            Self::MissingApproval => "missing_approval",
            Self::InvalidApprovalSignature => "invalid_approval_signature",
            Self::InvalidApprovalEnvelope(_) => "invalid_approval_envelope",
            Self::ApprovalBindingMismatch => "approval_binding_mismatch",
            Self::ContextBindingMismatch => "context_binding_mismatch",
            Self::ParentBindingMismatch => "parent_binding_mismatch",
            Self::TenantMismatch => "tenant_mismatch",
            Self::OwnerMismatch => "owner_mismatch",
            Self::Expired => "expired",
            Self::DelegationCycle => "delegation_cycle",
            Self::HopBudgetExceeded => "hop_budget_exceeded",
            Self::TurnLimitExceeded => "turn_limit_exceeded",
            Self::CostUnknown => "cost_unknown",
            Self::CostLimitExceeded => "cost_limit_exceeded",
            Self::ApprovalReplay => "approval_replay",
            Self::ActionConflict => "action_conflict",
            Self::BudgetExhausted => "budget_exhausted",
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::ApprovalSign => "approval_sign",
        }
    }

    /// Enumeration-safe message suitable for an external refusal.
    pub const fn public_message(&self) -> &'static str {
        match self {
            Self::OwnerMismatch | Self::TenantMismatch => "delegation target unavailable",
            _ => "delegation refused",
        }
    }
}

impl DelegationRequest {
    /// Validate immutable request fields and initial-release constraints.
    pub fn validate(&self) -> Result<(), DelegationError> {
        if self.delegation_id.is_nil() {
            return Err(DelegationError::InvalidField("delegation_id"));
        }
        parse_event_id("origin_event_id", &self.origin_event_id)?;
        if let Some(parent_id) = self.parent_approval_event_id.as_deref() {
            parse_event_id("parent_approval_event_id", parent_id)?;
        }
        parse_pubkey("source_agent", &self.source_agent)?;
        parse_pubkey("target_agent", &self.target_agent)?;
        if self.source_agent == self.target_agent {
            return Err(DelegationError::InvalidField("target_agent"));
        }
        if self.hop_budget == 0 || self.hop_budget > MAX_HOP_BUDGET {
            return Err(DelegationError::HopBudgetExceeded);
        }
        if self.agent_path.len() < 2 || self.agent_path.len() > usize::from(MAX_HOP_BUDGET) + 1 {
            return Err(DelegationError::HopBudgetExceeded);
        }
        let mut seen = HashSet::with_capacity(self.agent_path.len());
        for agent in &self.agent_path {
            parse_pubkey("agent_path", agent)?;
            if !seen.insert(agent.as_str()) {
                return Err(DelegationError::DelegationCycle);
            }
        }
        if (self.agent_path.len() == 2) != self.parent_approval_event_id.is_none() {
            return Err(DelegationError::ParentBindingMismatch);
        }
        let path_hops = self.agent_path.len() - 1;
        if path_hops > usize::from(self.hop_budget) {
            return Err(DelegationError::HopBudgetExceeded);
        }
        if self.agent_path.last() != Some(&self.target_agent)
            || self.agent_path.get(self.agent_path.len() - 2) != Some(&self.source_agent)
        {
            return Err(DelegationError::InvalidField("agent_path"));
        }
        if self.max_turns == 0 {
            return Err(DelegationError::TurnLimitExceeded);
        }
        if self.token_budget == 0 {
            return Err(DelegationError::InvalidField("token_budget"));
        }
        validate_idempotency_key(&self.idempotency_key)?;
        if self.expires_at == 0 {
            return Err(DelegationError::InvalidTimestamp);
        }
        Ok(())
    }
}

/// Compute the frozen, domain-separated immutable request hash.
///
/// `community_id` must be resolved by the server. It is deliberately absent
/// from [`DelegationRequest`] so an agent cannot select its tenant namespace.
pub fn immutable_request_hash(
    community_id: CommunityId,
    request: &DelegationRequest,
) -> Result<String, DelegationError> {
    request.validate()?;
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_HASH_DOMAIN);
    hasher.update(community_id.as_uuid().as_bytes());
    hasher.update(request.delegation_id.as_bytes());
    match request.parent_approval_event_id.as_deref() {
        Some(parent_id) => {
            hasher.update([1]);
            hash_string(&mut hasher, parent_id);
        }
        None => hasher.update([0]),
    }
    hash_string(&mut hasher, &request.origin_event_id);
    hash_string(&mut hasher, &request.source_agent);
    hash_string(&mut hasher, &request.target_agent);
    hasher
        .update([u8::try_from(request.agent_path.len())
            .map_err(|_| DelegationError::HopBudgetExceeded)?]);
    for agent in &request.agent_path {
        hash_string(&mut hasher, agent);
    }
    hasher.update([request.hop_budget]);
    hasher.update(request.max_turns.to_be_bytes());
    match request.cost_cap_microusd {
        Some(cost) => {
            hasher.update([1]);
            hasher.update(cost.to_be_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.update(request.token_budget.to_be_bytes());
    hash_string(&mut hasher, &request.idempotency_key);
    hasher.update(request.expires_at.to_be_bytes());
    Ok(hex::encode(hasher.finalize()))
}

/// Build and sign the dedicated operator-approval event.
pub fn build_operator_approval_event(
    operator_keys: &Keys,
    community_id: CommunityId,
    request: &DelegationRequest,
    created_at: u64,
) -> Result<Event, DelegationError> {
    request.validate()?;
    if created_at >= request.expires_at {
        return Err(DelegationError::InvalidTimestamp);
    }
    let approval = DelegationApproval::for_request(community_id, request)?;
    let content = serde_json::to_string(&approval).map_err(|_| DelegationError::ApprovalSign)?;
    let delegation_id = request.delegation_id.to_string();
    let expires_at = request.expires_at.to_string();
    let tags = vec![
        parse_tag(["d", delegation_id.as_str()])?,
        parse_tag(["e", request.origin_event_id.as_str()])?,
        parse_tag(["p", request.target_agent.as_str()])?,
        parse_tag(["request", approval.immutable_request_hash.as_str()])?,
        parse_tag(["expiration", expires_at.as_str()])?,
    ];
    EventBuilder::new(Kind::Custom(KIND_DELEGATION_APPROVAL as u16), content)
        .tags(tags)
        .custom_created_at(nostr::Timestamp::from(created_at))
        .sign_with_keys(operator_keys)
        .map_err(|_| DelegationError::ApprovalSign)
}

/// Strictly parse a delegation record JSON document in a server-resolved tenant.
pub fn parse_record_json(
    bytes: &[u8],
    community_id: CommunityId,
) -> Result<DelegationRecord, DelegationError> {
    let record: DelegationRecord = serde_json::from_slice(bytes)
        .map_err(|error| DelegationError::InvalidSchema(error.to_string()))?;
    record.validate(community_id)?;
    Ok(record)
}

/// Strictly parse an execution-context JSON document.
pub fn parse_context_json(bytes: &[u8]) -> Result<DelegationExecutionContext, DelegationError> {
    let context: DelegationExecutionContext = serde_json::from_slice(bytes)
        .map_err(|error| DelegationError::InvalidSchema(error.to_string()))?;
    context.validate_shape()?;
    Ok(context)
}

/// Verify and bind a context, approval event, current owners, expiry, and cost.
///
/// This function does not claim replay/idempotency keys. On success, call the
/// durable claim transaction and pass its result to [`classify_claim_outcome`].
/// It performs CPU-bound Schnorr verification; async handlers must invoke it
/// through their blocking-work facility (for example, `spawn_blocking`).
pub fn validate_for_claim(
    record: &DelegationRecord,
    context: Option<&DelegationExecutionContext>,
    approval_event: Option<&Event>,
    facts: &ResolvedDelegationFacts,
) -> Result<ValidatedDelegationContext, DelegationError> {
    let context = context.ok_or(DelegationError::MissingContext)?;
    let approval_event = approval_event.ok_or(DelegationError::MissingApproval)?;
    record.validate(facts.community_id)?;
    context.validate_shape()?;
    if record.state != DelegationState::Approved {
        return Err(if record.operator_approval_event_id.is_none() {
            DelegationError::MissingApproval
        } else {
            DelegationError::InvalidState
        });
    }
    if context.request != record.request
        || context.immutable_request_hash != record.immutable_request_hash
    {
        return Err(DelegationError::ContextBindingMismatch);
    }
    let record_approval_id = record
        .operator_approval_event_id
        .as_deref()
        .ok_or(DelegationError::MissingApproval)?;
    if context.operator_approval_event_id != record_approval_id
        || approval_event.id.to_hex() != record_approval_id
    {
        return Err(DelegationError::ApprovalBindingMismatch);
    }
    if approval_event.kind.as_u16() as u32 != KIND_DELEGATION_APPROVAL {
        return Err(DelegationError::InvalidApprovalEnvelope(
            "wrong kind".into(),
        ));
    }
    verify_event(approval_event).map_err(|_| DelegationError::InvalidApprovalSignature)?;

    let approval: DelegationApproval = serde_json::from_str(&approval_event.content)
        .map_err(|error| DelegationError::InvalidApprovalEnvelope(error.to_string()))?;
    approval.validate()?;
    if approval.delegation_id != record.request.delegation_id
        || approval.immutable_request_hash != record.immutable_request_hash
        || approval.expires_at != record.request.expires_at
    {
        return Err(DelegationError::ApprovalBindingMismatch);
    }
    validate_approval_tags(approval_event, record)?;
    let approval_created_at = approval_event.created_at.as_secs();
    if approval_created_at < record.created_at
        || approval_created_at > facts.now
        || approval_created_at >= record.request.expires_at
    {
        return Err(DelegationError::InvalidTimestamp);
    }

    if facts.now >= record.request.expires_at {
        return Err(DelegationError::Expired);
    }
    let operator_pubkey = approval_event.pubkey.to_hex();
    validate_parent_lineage(
        &record.request,
        &facts.lineage,
        facts.community_id,
        facts.now,
        &operator_pubkey,
    )?;
    validate_current_owners(&record.request, &operator_pubkey, &facts.agent_owners)?;
    Ok(ValidatedDelegationContext {
        context: context.clone(),
        community_id: facts.community_id,
        operator_pubkey,
    })
}

/// Revalidate mutable authority and budget facts before every target action.
///
/// Callers must first rebuild [`ValidatedDelegationContext`] from the current
/// stored record, context, and complete approval event. This second gate catches
/// ownership changes, expiry, stale turn counters, and cumulative-cost breaches
/// that can occur after the initial durable enqueue.
pub fn validate_next_action(
    validated: &ValidatedDelegationContext,
    facts: &ResolvedDelegationActionFacts,
) -> Result<ValidatedDelegationAction, DelegationError> {
    let context = validated.context();
    if facts.community_id != validated.community_id {
        return Err(DelegationError::TenantMismatch);
    }
    if facts.now >= context.request.expires_at {
        return Err(DelegationError::Expired);
    }
    let owner_snapshot = resolve_current_owner_snapshot(
        &context.request,
        validated.operator_pubkey(),
        &facts.agent_owners,
    )?;
    if facts.remaining_turns == 0
        || facts.remaining_turns > context.request.max_turns
        || facts.remaining_turns > context.remaining_turns
    {
        return Err(DelegationError::TurnLimitExceeded);
    }

    let (
        cost_committed_before_microusd,
        cost_reservation_microusd,
        projected_committed_cost_microusd,
    ) = match context.request.cost_cap_microusd {
        Some(cap) => {
            let committed = facts
                .cost_committed_microusd
                .ok_or(DelegationError::CostUnknown)?;
            let reservation = facts
                .action_cost_reservation_microusd
                .ok_or(DelegationError::CostUnknown)?;
            let projected = committed
                .checked_add(reservation)
                .ok_or(DelegationError::CostLimitExceeded)?;
            if projected > cap {
                return Err(DelegationError::CostLimitExceeded);
            }
            (Some(committed), Some(reservation), Some(projected))
        }
        None => (None, None, None),
    };

    Ok(ValidatedDelegationAction {
        community_id: validated.community_id,
        delegation_id: context.request.delegation_id,
        approval_event_id: context.operator_approval_event_id.clone(),
        immutable_request_hash: context.immutable_request_hash.clone(),
        expires_at: context.request.expires_at,
        owner_snapshot,
        remaining_turns_before: facts.remaining_turns,
        cost_committed_before_microusd,
        cost_reservation_microusd,
        projected_committed_cost_microusd,
    })
}

/// Opaque proof that no open action row names `source_agent` as its target.
///
/// Constructible only by [`DelegationClaimStore::prove_no_open_parent`], which
/// a caller must obtain inside the same transaction as
/// [`claim_and_enqueue`] so root-ness is proven under that transaction's
/// snapshot. There is no public constructor; a `pub` inner field would defeat
/// this (Slice 4, AC-4).
#[derive(Debug)]
pub struct RootProof(());

/// A durable store's outcome for [`claim_and_enqueue`].
///
/// Only [`AcquiredAndEnqueued`](Self::AcquiredAndEnqueued) yields a
/// [`DelegationExecutionPermit`]; every other outcome yields none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStoreOutcome {
    /// All three claim keys and the first `delegation_actions` row were
    /// committed atomically in the same transaction.
    AcquiredAndEnqueued,
    /// The exact claim already has durable pending work; suppress this submit.
    ExactDuplicatePending,
    /// The exact claim already completed; suppress this submit.
    ExactDuplicateCompleted,
    /// The delegation id was already bound to different claim data.
    DelegationConflict,
    /// The approval event was already bound to different claim data.
    ApprovalConflict,
    /// The idempotency key was already bound to different claim data.
    IdempotencyConflict,
    /// Durable claim state could not be read or committed.
    StoreUnavailable,
}

/// Sealed, unforgeable evidence that a delegation was durably claimed.
///
/// No code outside [`claim_and_enqueue`] can construct this value. Its fields
/// are private and it derives neither `Clone`, `Copy`, `Default`, nor
/// `serde::Deserialize` (Slice 4, I-4/AC-4).
#[derive(Debug)]
pub struct DelegationExecutionPermit {
    community_id: CommunityId,
    run_id: Uuid,
    delegation_id: Uuid,
    approval_event_id: String,
    immutable_request_hash: String,
    operator_pubkey: String,
    source_agent: String,
    target_agent: String,
    agent_path: Vec<String>,
    expires_at: u64,
}

impl DelegationExecutionPermit {
    /// Server-resolved tenant this permit was claimed under.
    pub fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Durable run id minted for this claim.
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Delegation id this permit authorizes.
    pub fn delegation_id(&self) -> Uuid {
        self.delegation_id
    }

    /// Approval event id bound to this permit.
    pub fn approval_event_id(&self) -> &str {
        &self.approval_event_id
    }

    /// Immutable request hash bound to this permit.
    pub fn immutable_request_hash(&self) -> &str {
        &self.immutable_request_hash
    }

    /// Verified operator signer.
    pub fn operator_pubkey(&self) -> &str {
        &self.operator_pubkey
    }

    /// Source agent that requested this delegation.
    pub fn source_agent(&self) -> &str {
        &self.source_agent
    }

    /// Target agent authorized to execute this delegation.
    pub fn target_agent(&self) -> &str {
        &self.target_agent
    }

    /// Ordered, same-owner agent path this permit authorizes.
    pub fn agent_path(&self) -> &[String] {
        &self.agent_path
    }

    /// Signed deadline; the permit confers no authority at or after this instant.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    fn provisional_root(
        community_id: CommunityId,
        run_id: Uuid,
        source_agent: &str,
        operator_pubkey: &str,
        expires_at: u64,
    ) -> Self {
        Self {
            community_id,
            run_id,
            delegation_id: Uuid::nil(),
            approval_event_id: String::new(),
            immutable_request_hash: String::new(),
            operator_pubkey: operator_pubkey.to_owned(),
            source_agent: source_agent.to_owned(),
            target_agent: String::new(),
            agent_path: Vec::new(),
            expires_at,
        }
    }
}

/// Outcome of [`claim_and_enqueue`]: a fresh permit, or a suppressed duplicate.
#[derive(Debug)]
pub enum ClaimDisposition {
    /// A fresh durable claim was acquired; execution may proceed with this permit.
    Permit(DelegationExecutionPermit),
    /// An identical claim already has pending or completed durable work.
    DuplicateSuppressed(DelegationClaim),
}

/// Durable store adapter a caller must implement to make delegation claims
/// atomic. Every method must run its I/O inside one transaction as documented.
pub trait DelegationClaimStore {
    /// Must, in ONE transaction: recheck `transaction_now < claim.expires_at`,
    /// insert the three claim keys, insert `delegation_records` (approved) and
    /// the first `delegation_actions` row, and return the outcome. Never
    /// returns a permit; only this crate mints one.
    fn claim_and_enqueue(
        &mut self,
        claim: &DelegationClaim,
        run_id: Uuid,
        transaction_now: u64,
    ) -> Result<ClaimStoreOutcome, DelegationError>;

    /// Prove, under the current transaction's snapshot, that `source_agent`
    /// has no open action row naming it as target. Returns `Ok(None)` when an
    /// open action exists (the request is a nested hop, not a root).
    fn prove_no_open_parent(
        &mut self,
        community_id: CommunityId,
        source_agent: &str,
    ) -> Result<Option<RootProof>, DelegationError>;

    /// Reconstitute the parent's live permit from its open action row.
    /// Returns `Some` only when the parent record is `approved` and has an
    /// action row with `settled_at IS NULL` or `outcome = 'delegated'`
    /// awaiting a child.
    fn reopen_live_permit(
        &mut self,
        community_id: CommunityId,
        parent_delegation_id: Uuid,
    ) -> Result<Option<DelegationExecutionPermit>, DelegationError>;
}

/// Claim a delegation durably and mint its execution permit.
///
/// Rechecks freshness with the claim's own bounds, then delegates to `store`,
/// then maps outcomes exactly as the crate-private reference classifier
/// (`classify_claim_outcome`) does: conflicts become [`DelegationError::ApprovalReplay`],
/// unavailability becomes [`DelegationError::AuthorityUnavailable`]. Mints the
/// permit only on [`ClaimStoreOutcome::AcquiredAndEnqueued`].
pub fn claim_and_enqueue(
    store: &mut dyn DelegationClaimStore,
    validated: &ValidatedDelegationContext,
    run_id: Uuid,
    transaction_now: u64,
) -> Result<ClaimDisposition, DelegationError> {
    let claim = validated.claim();
    claim.ensure_fresh_at(transaction_now)?;
    let outcome = store.claim_and_enqueue(&claim, run_id, transaction_now)?;
    match outcome {
        ClaimStoreOutcome::AcquiredAndEnqueued => {
            let context = validated.context();
            Ok(ClaimDisposition::Permit(DelegationExecutionPermit {
                community_id: validated.community_id,
                run_id,
                delegation_id: context.request.delegation_id,
                approval_event_id: context.operator_approval_event_id.clone(),
                immutable_request_hash: context.immutable_request_hash.clone(),
                operator_pubkey: validated.operator_pubkey.clone(),
                source_agent: context.request.source_agent.clone(),
                target_agent: context.request.target_agent.clone(),
                agent_path: context.request.agent_path.clone(),
                expires_at: context.request.expires_at,
            }))
        }
        ClaimStoreOutcome::ExactDuplicatePending | ClaimStoreOutcome::ExactDuplicateCompleted => {
            Ok(ClaimDisposition::DuplicateSuppressed(claim))
        }
        ClaimStoreOutcome::DelegationConflict
        | ClaimStoreOutcome::ApprovalConflict
        | ClaimStoreOutcome::IdempotencyConflict => Err(DelegationError::ApprovalReplay),
        ClaimStoreOutcome::StoreUnavailable => Err(DelegationError::AuthorityUnavailable),
    }
}

impl ResolvedDelegationLineage {
    /// No lineage could be resolved; every match against it fails closed.
    pub fn unavailable() -> Self {
        Self {
            kind: ResolvedDelegationLineageKind::Unavailable,
        }
    }

    /// Bind a root delegation's lineage from a provisional root permit minted
    /// only for the root run itself, proven root-less-parent by `proof`.
    ///
    /// `proof` is obtainable only from
    /// [`DelegationClaimStore::prove_no_open_parent`], so this constructor
    /// cannot be reached from wire data or stateless validation alone.
    pub fn root_for_run(
        community_id: CommunityId,
        run_id: Uuid,
        source_agent: &str,
        operator_pubkey: &str,
        expires_at: u64,
        proof: RootProof,
    ) -> Self {
        let _ = proof;
        let permit = DelegationExecutionPermit::provisional_root(
            community_id,
            run_id,
            source_agent,
            operator_pubkey,
            expires_at,
        );
        Self::root_from_permit(&permit)
    }

    /// Bind a root delegation's lineage from a (possibly provisional) permit
    /// for the root run itself.
    pub fn root_from_permit(permit: &DelegationExecutionPermit) -> Self {
        Self {
            kind: ResolvedDelegationLineageKind::Root(ResolvedDelegationRoot {
                community_id: permit.community_id,
                run_id: permit.run_id,
                source_agent: permit.source_agent.clone(),
                operator_pubkey: permit.operator_pubkey.clone(),
                expires_at: permit.expires_at,
            }),
        }
    }

    /// Bind a nested delegation's lineage from its parent's live permit.
    pub fn parent_from_permit(permit: &DelegationExecutionPermit) -> Self {
        Self {
            kind: ResolvedDelegationLineageKind::Parent(ResolvedDelegationParent {
                community_id: permit.community_id,
                run_id: permit.run_id,
                delegation_id: permit.delegation_id,
                approval_event_id: permit.approval_event_id.clone(),
                immutable_request_hash: permit.immutable_request_hash.clone(),
                operator_pubkey: permit.operator_pubkey.clone(),
                expires_at: permit.expires_at,
                agent_path: permit.agent_path.clone(),
            }),
        }
    }
}

/// A durable store's outcome for [`cas_action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionStoreOutcome {
    /// The exact claim/state matched and the action row was committed.
    AppliedAndRecorded {
        /// Sequence number (`1..=max_turns`) assigned to the recorded action.
        action_seq: u32,
        /// Durable run id this delegation was claimed under.
        run_id: Uuid,
        /// Remaining turns immediately after this action was recorded.
        remaining_turns_after: u32,
        /// Remaining token budget immediately after this action was recorded.
        token_budget_remaining: u64,
    },
    /// The durable claim was absent or its tenant/request/approval binding differed.
    ClaimConflict,
    /// The expected remaining-turn or committed-cost state was stale.
    StateConflict,
    /// Locked ownership rows no longer matched the action's revisions.
    AuthorityConflict,
    /// The delegation's remaining token budget is exhausted.
    BudgetExhausted,
    /// Durable action state could not be read or committed.
    StoreUnavailable,
}

/// Sealed, unforgeable evidence that a target action was durably recorded.
///
/// No code outside [`cas_action`] can construct this value.
#[derive(Debug)]
pub struct DelegationActionPermit {
    community_id: CommunityId,
    delegation_id: Uuid,
    action_seq: u32,
    run_id: Uuid,
    target_agent: String,
    remaining_turns_after: u32,
    token_budget_remaining: u64,
    expires_at: u64,
    owner_snapshot: Vec<ResolvedAgentOwner>,
}

impl DelegationActionPermit {
    /// Server-resolved tenant this action was recorded under.
    pub fn community_id(&self) -> CommunityId {
        self.community_id
    }

    /// Delegation id this action belongs to.
    pub fn delegation_id(&self) -> Uuid {
        self.delegation_id
    }

    /// Sequence number assigned to this action (`1..=max_turns`).
    pub fn action_seq(&self) -> u32 {
        self.action_seq
    }

    /// Durable run id this action was recorded under.
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Target agent authorized to execute this action.
    pub fn target_agent(&self) -> &str {
        &self.target_agent
    }

    /// Remaining turns immediately after this action was recorded.
    pub fn remaining_turns_after(&self) -> u32 {
        self.remaining_turns_after
    }

    /// Remaining token budget immediately after this action was recorded.
    pub fn token_budget_remaining(&self) -> u64 {
        self.token_budget_remaining
    }

    /// Signed deadline; the permit confers no authority at or after this instant.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Locked ownership snapshot the outbox dispatcher must re-verify
    /// immediately before causing any external effect.
    pub fn owner_snapshot(&self) -> &[ResolvedAgentOwner] {
        &self.owner_snapshot
    }
}

/// Durable store adapter a caller must implement to make per-action
/// compare-and-swap dispatch atomic.
pub trait DelegationActionStore {
    /// Must, in ONE transaction: re-resolve and lock every path owner row,
    /// recheck freshness, match the exact claim identity and expected
    /// turn/cost/budget state, decrement `remaining_turns` by one, and
    /// record the action row before returning.
    fn cas_and_record(
        &mut self,
        action: &ValidatedDelegationAction,
        token_budget_check: bool,
        transaction_now: u64,
    ) -> Result<ActionStoreOutcome, DelegationError>;
}

/// Run the per-action compare-and-swap and mint its execution permit.
///
/// Maps `store` outcomes exactly as the crate-private reference classifier
/// (`classify_action_outcome`) does, plus [`ActionStoreOutcome::BudgetExhausted`]
/// mapping to [`DelegationError::BudgetExhausted`]. Mints the permit only on
/// [`ActionStoreOutcome::AppliedAndRecorded`].
pub fn cas_action(
    store: &mut dyn DelegationActionStore,
    action: &ValidatedDelegationAction,
    transaction_now: u64,
) -> Result<DelegationActionPermit, DelegationError> {
    action.ensure_fresh_at(transaction_now)?;
    let token_budget_check = true;
    let outcome = store.cas_and_record(action, token_budget_check, transaction_now)?;
    match outcome {
        ActionStoreOutcome::AppliedAndRecorded {
            action_seq,
            run_id,
            remaining_turns_after,
            token_budget_remaining,
        } => {
            let target_agent = action
                .owner_snapshot()
                .last()
                .map(|owner| owner.agent_pubkey.clone())
                .unwrap_or_default();
            Ok(DelegationActionPermit {
                community_id: action.community_id(),
                delegation_id: action.delegation_id(),
                action_seq,
                run_id,
                target_agent,
                remaining_turns_after,
                token_budget_remaining,
                expires_at: action.expires_at(),
                owner_snapshot: action.owner_snapshot().to_vec(),
            })
        }
        ActionStoreOutcome::ClaimConflict
        | ActionStoreOutcome::StateConflict
        | ActionStoreOutcome::AuthorityConflict => Err(DelegationError::ActionConflict),
        ActionStoreOutcome::BudgetExhausted => Err(DelegationError::BudgetExhausted),
        ActionStoreOutcome::StoreUnavailable => Err(DelegationError::AuthorityUnavailable),
    }
}

/// Classify a reference pre-action compare-and-swap result for contract tests.
///
/// Freshness is checked again with the transaction's own clock. Only a
/// transaction that atomically matched the exact claim and expected turn/cost
/// state, reserved capacity, and recorded the action permits later dispatch.
/// Claim and state conflicts intentionally collapse to one external error so a
/// caller cannot probe durable delegation state.
/// Runtime code must use a sealed durable adapter rather than expose or accept
/// this caller-constructible reference outcome.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn classify_action_outcome(
    validated: &ValidatedDelegationAction,
    transaction_now: u64,
    outcome: DelegationActionStoreOutcome,
) -> Result<DelegationActionDisposition, DelegationError> {
    validated.ensure_fresh_at(transaction_now)?;
    match outcome {
        DelegationActionStoreOutcome::AppliedAndRecorded => {
            Ok(DelegationActionDisposition::DurablyRecorded)
        }
        DelegationActionStoreOutcome::ClaimConflict
        | DelegationActionStoreOutcome::StateConflict
        | DelegationActionStoreOutcome::AuthorityConflict => Err(DelegationError::ActionConflict),
        DelegationActionStoreOutcome::StoreUnavailable => {
            Err(DelegationError::AuthorityUnavailable)
        }
    }
}

/// Classify a reference atomic-claim result for contract tests.
///
/// Delegation, approval, and idempotency conflicts are malicious/conflicting
/// replays. Exact pending/completed duplicates are normal at-least-once retries
/// and are collapsed because durable work or a terminal result already exists.
///
/// This function deliberately does not create an execution permit. Its
/// crate-private outcome is not proof of a database write; `transaction_now`
/// must be the claim transaction's authoritative clock, and Slice 4 creates its
/// private permit inside a co-located sealed adapter at that durable seam.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn classify_claim_outcome(
    validated: &ValidatedDelegationContext,
    transaction_now: u64,
    outcome: AtomicClaimOutcome,
) -> Result<DelegationClaimDisposition, DelegationError> {
    let claim = validated.claim();
    claim.ensure_fresh_at(transaction_now)?;
    match outcome {
        AtomicClaimOutcome::AcquiredAndEnqueued => Ok(DelegationClaimDisposition::DurablyEnqueued),
        AtomicClaimOutcome::ExactDuplicatePending | AtomicClaimOutcome::ExactDuplicateCompleted => {
            Ok(DelegationClaimDisposition::DuplicateSuppressed(claim))
        }
        AtomicClaimOutcome::DelegationConflict
        | AtomicClaimOutcome::ApprovalConflict
        | AtomicClaimOutcome::IdempotencyConflict => Err(DelegationError::ApprovalReplay),
        AtomicClaimOutcome::StoreUnavailable => Err(DelegationError::AuthorityUnavailable),
    }
}

fn validate_discriminator(
    actual_format: &str,
    expected_format: &str,
    version: u32,
) -> Result<(), DelegationError> {
    if actual_format != expected_format {
        return Err(DelegationError::InvalidFormat);
    }
    if version != VERSION {
        return Err(DelegationError::UnsupportedVersion);
    }
    Ok(())
}

fn validate_current_owners(
    request: &DelegationRequest,
    operator_pubkey: &str,
    owners: &[ResolvedAgentOwner],
) -> Result<(), DelegationError> {
    resolve_current_owner_snapshot(request, operator_pubkey, owners).map(|_| ())
}

fn resolve_current_owner_snapshot(
    request: &DelegationRequest,
    operator_pubkey: &str,
    owners: &[ResolvedAgentOwner],
) -> Result<Vec<ResolvedAgentOwner>, DelegationError> {
    if owners.len() != request.agent_path.len() {
        return Err(DelegationError::OwnerMismatch);
    }
    let mut snapshot = Vec::with_capacity(request.agent_path.len());
    for agent in &request.agent_path {
        let mut matches = owners.iter().filter(|entry| entry.agent_pubkey == *agent);
        let Some(entry) = matches.next() else {
            return Err(DelegationError::OwnerMismatch);
        };
        if matches.next().is_some()
            || entry.owner_pubkey.as_deref() != Some(operator_pubkey)
            || entry.ownership_revision == 0
            || parse_pubkey("resolved_agent", &entry.agent_pubkey).is_err()
            || entry
                .owner_pubkey
                .as_deref()
                .is_some_and(|owner| parse_pubkey("resolved_owner", owner).is_err())
        {
            return Err(DelegationError::OwnerMismatch);
        }
        snapshot.push(entry.clone());
    }
    Ok(snapshot)
}

fn validate_owner_snapshot_unchanged(
    expected: &[ResolvedAgentOwner],
    current: &[ResolvedAgentOwner],
) -> Result<(), DelegationError> {
    if current.len() != expected.len() {
        return Err(DelegationError::OwnerMismatch);
    }
    for expected_entry in expected {
        let mut matches = current
            .iter()
            .filter(|entry| entry.agent_pubkey == expected_entry.agent_pubkey);
        let Some(current_entry) = matches.next() else {
            return Err(DelegationError::OwnerMismatch);
        };
        if matches.next().is_some() || current_entry != expected_entry {
            return Err(DelegationError::OwnerMismatch);
        }
    }
    Ok(())
}

fn validate_parent_lineage(
    request: &DelegationRequest,
    lineage: &ResolvedDelegationLineage,
    community_id: CommunityId,
    now: u64,
    operator_pubkey: &str,
) -> Result<(), DelegationError> {
    match (&lineage.kind, request.parent_approval_event_id.as_deref()) {
        (ResolvedDelegationLineageKind::Root(root), None)
            if root.community_id == community_id
                && !root.run_id.is_nil()
                && root.source_agent == request.source_agent
                && root.operator_pubkey == operator_pubkey
                && now < root.expires_at
                && request.expires_at <= root.expires_at =>
        {
            Ok(())
        }
        (ResolvedDelegationLineageKind::Parent(parent), Some(parent_id))
            if parent.community_id == community_id
                && !parent.run_id.is_nil()
                && !parent.delegation_id.is_nil()
                && parent.approval_event_id == parent_id
                && validate_hash(
                    "parent_immutable_request_hash",
                    &parent.immutable_request_hash,
                )
                .is_ok()
                && parent.operator_pubkey == operator_pubkey
                && now < parent.expires_at
                && request.expires_at <= parent.expires_at
                && request.agent_path.len() == parent.agent_path.len() + 1
                && request.agent_path[..parent.agent_path.len()] == parent.agent_path
                && parent.agent_path.last() == Some(&request.source_agent) =>
        {
            Ok(())
        }
        _ => Err(DelegationError::ParentBindingMismatch),
    }
}

fn validate_approval_tags(event: &Event, record: &DelegationRecord) -> Result<(), DelegationError> {
    let mut delegation_id = None;
    let mut origin_event_id = None;
    let mut target_agent = None;
    let mut request_hash = None;
    let mut expiration = None;
    for tag in event.tags.iter() {
        let parts = tag.as_slice();
        if parts.len() != 2 {
            return Err(DelegationError::InvalidApprovalEnvelope(
                "every tag must have exactly one value".into(),
            ));
        }
        let slot = match parts[0].as_str() {
            "d" => &mut delegation_id,
            "e" => &mut origin_event_id,
            "p" => &mut target_agent,
            "request" => &mut request_hash,
            "expiration" => &mut expiration,
            name => {
                return Err(DelegationError::InvalidApprovalEnvelope(format!(
                    "unexpected tag: {name}"
                )))
            }
        };
        if slot.replace(parts[1].clone()).is_some() {
            return Err(DelegationError::InvalidApprovalEnvelope(format!(
                "duplicate {} tag",
                parts[0]
            )));
        }
    }
    let expected_expiration = record.request.expires_at.to_string();
    if delegation_id.as_deref() != Some(record.request.delegation_id.to_string().as_str())
        || origin_event_id.as_deref() != Some(record.request.origin_event_id.as_str())
        || target_agent.as_deref() != Some(record.request.target_agent.as_str())
        || request_hash.as_deref() != Some(record.immutable_request_hash.as_str())
        || expiration.as_deref() != Some(expected_expiration.as_str())
    {
        return Err(DelegationError::ApprovalBindingMismatch);
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), DelegationError> {
    if value.is_empty()
        || value.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
    {
        return Err(DelegationError::InvalidField("idempotency_key"));
    }
    Ok(())
}

fn validate_hash(label: &'static str, value: &str) -> Result<(), DelegationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(DelegationError::InvalidField(label));
    }
    Ok(())
}

fn parse_pubkey(label: &'static str, value: &str) -> Result<PublicKey, DelegationError> {
    validate_hash(label, value)?;
    let key = PublicKey::from_hex(value).map_err(|_| DelegationError::InvalidField(label))?;
    key.xonly()
        .map_err(|_| DelegationError::InvalidField(label))?;
    Ok(key)
}

fn parse_event_id(label: &'static str, value: &str) -> Result<EventId, DelegationError> {
    validate_hash(label, value)?;
    EventId::from_hex(value).map_err(|_| DelegationError::InvalidField(label))
}

fn parse_tag<const N: usize>(parts: [&str; N]) -> Result<Tag, DelegationError> {
    Tag::parse(parts)
        .map_err(|_| DelegationError::InvalidApprovalEnvelope("failed to build tag".into()))
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::JsonUtil;

    struct Fixture {
        community_id: CommunityId,
        owner: Keys,
        other_owner: Keys,
        record: DelegationRecord,
        context: DelegationExecutionContext,
        approval: Event,
        facts: ResolvedDelegationFacts,
    }

    fn test_community_id() -> CommunityId {
        CommunityId::from_uuid(
            Uuid::parse_str("3580ca9b-47b4-4af9-b22a-1068778f26c6").expect("fixed community UUID"),
        )
    }

    fn root_lineage(
        community_id: CommunityId,
        source_agent: &str,
        owner: &Keys,
        expires_at: u64,
    ) -> ResolvedDelegationLineage {
        ResolvedDelegationLineage {
            kind: ResolvedDelegationLineageKind::Root(ResolvedDelegationRoot {
                community_id,
                run_id: Uuid::new_v4(),
                source_agent: source_agent.to_owned(),
                operator_pubkey: owner.public_key().to_hex(),
                expires_at,
            }),
        }
    }

    fn unavailable_lineage() -> ResolvedDelegationLineage {
        ResolvedDelegationLineage {
            kind: ResolvedDelegationLineageKind::Unavailable,
        }
    }

    fn parent_token_mut(lineage: &mut ResolvedDelegationLineage) -> &mut ResolvedDelegationParent {
        match &mut lineage.kind {
            ResolvedDelegationLineageKind::Parent(parent) => parent,
            _ => panic!("expected parent lineage"),
        }
    }

    fn root_token_mut(lineage: &mut ResolvedDelegationLineage) -> &mut ResolvedDelegationRoot {
        match &mut lineage.kind {
            ResolvedDelegationLineageKind::Root(root) => root,
            _ => panic!("expected root lineage"),
        }
    }

    fn fixture() -> Fixture {
        let community_id = test_community_id();
        let owner = Keys::generate();
        let other_owner = Keys::generate();
        let source = Keys::generate().public_key().to_hex();
        let target = Keys::generate().public_key().to_hex();
        let request = DelegationRequest {
            delegation_id: Uuid::new_v4(),
            origin_event_id: "11".repeat(32),
            parent_approval_event_id: None,
            source_agent: source.clone(),
            target_agent: target.clone(),
            agent_path: vec![source.clone(), target.clone()],
            hop_budget: DEFAULT_HOP_BUDGET,
            max_turns: 8,
            cost_cap_microusd: None,
            token_budget: 50_000,
            idempotency_key: "delegation-test-001".into(),
            expires_at: 1_800_000_300,
        };
        let mut record = DelegationRecord::new_offered(community_id, request, 1_800_000_000)
            .expect("valid offered record");
        let approval =
            build_operator_approval_event(&owner, community_id, &record.request, 1_800_000_001)
                .expect("valid approval event");
        record.operator_approval_event_id = Some(approval.id.to_hex());
        record.state = DelegationState::Approved;
        record.updated_at = 1_800_000_001;
        record
            .validate(community_id)
            .expect("valid approved record");
        let context = DelegationExecutionContext::from_approved_record(community_id, &record, 8)
            .expect("valid execution context");
        let owner_hex = owner.public_key().to_hex();
        let lineage = root_lineage(
            community_id,
            &record.request.source_agent,
            &owner,
            record.request.expires_at,
        );
        let facts = ResolvedDelegationFacts {
            community_id,
            now: 1_800_000_002,
            agent_owners: vec![
                ResolvedAgentOwner {
                    agent_pubkey: source,
                    owner_pubkey: Some(owner_hex.clone()),
                    ownership_revision: 1,
                },
                ResolvedAgentOwner {
                    agent_pubkey: target,
                    owner_pubkey: Some(owner_hex),
                    ownership_revision: 1,
                },
            ],
            lineage,
        };
        Fixture {
            community_id,
            owner,
            other_owner,
            record,
            context,
            approval,
            facts,
        }
    }

    fn approved_components(
        community_id: CommunityId,
        owner: &Keys,
        request: DelegationRequest,
    ) -> (DelegationRecord, DelegationExecutionContext, Event) {
        let mut record = DelegationRecord::new_offered(community_id, request, 1_800_000_000)
            .expect("valid offered record");
        let approval =
            build_operator_approval_event(owner, community_id, &record.request, 1_800_000_001)
                .expect("valid approval event");
        record.operator_approval_event_id = Some(approval.id.to_hex());
        record.state = DelegationState::Approved;
        record.updated_at = 1_800_000_001;
        let context = DelegationExecutionContext::from_approved_record(
            community_id,
            &record,
            record.request.max_turns,
        )
        .expect("valid execution context");
        (record, context, approval)
    }

    fn nested_components(
        parent: &Fixture,
    ) -> (
        DelegationRecord,
        DelegationExecutionContext,
        Event,
        ResolvedDelegationFacts,
    ) {
        let target = Keys::generate().public_key().to_hex();
        let source = parent.record.request.target_agent.clone();
        let request = DelegationRequest {
            delegation_id: Uuid::new_v4(),
            origin_event_id: "33".repeat(32),
            parent_approval_event_id: Some(parent.approval.id.to_hex()),
            source_agent: source,
            target_agent: target.clone(),
            agent_path: vec![
                parent.record.request.agent_path[0].clone(),
                parent.record.request.agent_path[1].clone(),
                target.clone(),
            ],
            hop_budget: MAX_HOP_BUDGET,
            max_turns: 4,
            cost_cap_microusd: None,
            token_budget: 50_000,
            idempotency_key: "nested-delegation-001".into(),
            expires_at: parent.record.request.expires_at,
        };
        let (record, context, approval) =
            approved_components(parent.community_id, &parent.owner, request);
        let mut owners = parent.facts.agent_owners.clone();
        owners.push(ResolvedAgentOwner {
            agent_pubkey: target,
            owner_pubkey: Some(parent.owner.public_key().to_hex()),
            ownership_revision: 1,
        });
        let facts = ResolvedDelegationFacts {
            community_id: parent.community_id,
            now: parent.facts.now,
            agent_owners: owners,
            lineage: live_parent_lineage(parent),
        };
        (record, context, approval, facts)
    }

    fn validate(fixture: &Fixture) -> Result<ValidatedDelegationContext, DelegationError> {
        validate_for_claim(
            &fixture.record,
            Some(&fixture.context),
            Some(&fixture.approval),
            &fixture.facts,
        )
    }

    fn live_parent_lineage(parent: &Fixture) -> ResolvedDelegationLineage {
        let validated = validate(parent).expect("stateless parent validation");
        assert_eq!(
            classify_claim_outcome(
                &validated,
                parent.facts.now,
                AtomicClaimOutcome::AcquiredAndEnqueued,
            )
            .expect("parent durable enqueue"),
            DelegationClaimDisposition::DurablyEnqueued
        );
        ResolvedDelegationLineage {
            kind: ResolvedDelegationLineageKind::Parent(ResolvedDelegationParent {
                community_id: parent.community_id,
                run_id: Uuid::new_v4(),
                delegation_id: parent.record.request.delegation_id,
                approval_event_id: parent.approval.id.to_hex(),
                immutable_request_hash: parent.record.immutable_request_hash.clone(),
                operator_pubkey: parent.owner.public_key().to_hex(),
                expires_at: parent.record.request.expires_at,
                agent_path: parent.record.request.agent_path.clone(),
            }),
        }
    }

    fn action_facts(fixture: &Fixture) -> ResolvedDelegationActionFacts {
        ResolvedDelegationActionFacts {
            community_id: fixture.community_id,
            now: fixture.facts.now,
            agent_owners: fixture.facts.agent_owners.clone(),
            remaining_turns: fixture.context.remaining_turns,
            cost_committed_microusd: None,
            action_cost_reservation_microusd: None,
        }
    }

    /// Test-only `DelegationClaimStore` that always returns one fixed outcome,
    /// used only to prove `claim_and_enqueue`'s permit-minting contract
    /// (`permit_only_from_store`): a permit exists if and only if the store
    /// reports `AcquiredAndEnqueued`.
    struct MockClaimStore {
        outcome: ClaimStoreOutcome,
    }

    impl MockClaimStore {
        fn always(outcome: ClaimStoreOutcome) -> Self {
            Self { outcome }
        }
    }

    impl DelegationClaimStore for MockClaimStore {
        fn claim_and_enqueue(
            &mut self,
            _claim: &DelegationClaim,
            _run_id: Uuid,
            _transaction_now: u64,
        ) -> Result<ClaimStoreOutcome, DelegationError> {
            Ok(self.outcome)
        }

        fn prove_no_open_parent(
            &mut self,
            _community_id: CommunityId,
            _source_agent: &str,
        ) -> Result<Option<RootProof>, DelegationError> {
            Ok(Some(RootProof(())))
        }

        fn reopen_live_permit(
            &mut self,
            _community_id: CommunityId,
            _parent_delegation_id: Uuid,
        ) -> Result<Option<DelegationExecutionPermit>, DelegationError> {
            Ok(None)
        }
    }

    /// Test-only `DelegationActionStore` that always returns one fixed
    /// outcome, used only to prove `cas_action`'s error-mapping contract.
    struct MockActionStore {
        outcome: ActionStoreOutcome,
    }

    impl MockActionStore {
        fn always(outcome: ActionStoreOutcome) -> Self {
            Self { outcome }
        }
    }

    impl DelegationActionStore for MockActionStore {
        fn cas_and_record(
            &mut self,
            _action: &ValidatedDelegationAction,
            _token_budget_check: bool,
            _transaction_now: u64,
        ) -> Result<ActionStoreOutcome, DelegationError> {
            Ok(self.outcome.clone())
        }
    }

    fn tamper_signature(event: &Event) -> Event {
        let mut value: serde_json::Value =
            serde_json::from_str(&event.as_json()).expect("event json");
        value["sig"] = serde_json::Value::String("0".repeat(128));
        Event::from_json(value.to_string()).expect("tampered event parses")
    }

    #[test]
    fn valid_same_owner_context_accepts_acquired_claim() {
        let fixture = fixture();
        let validated = validate(&fixture).expect("stateless validation");
        let claim = validated.claim();
        assert_eq!(claim.community_id(), fixture.community_id);
        assert_eq!(claim.operator_pubkey(), fixture.owner.public_key().to_hex());
        assert_eq!(claim.source_agent(), fixture.record.request.source_agent);
        assert_eq!(claim.expires_at(), fixture.record.request.expires_at);
        let disposition = classify_claim_outcome(
            &validated,
            fixture.facts.now,
            AtomicClaimOutcome::AcquiredAndEnqueued,
        )
        .expect("atomic claim plus durable enqueue is accepted");
        assert_eq!(disposition, DelegationClaimDisposition::DurablyEnqueued);
    }

    #[test]
    fn cross_owner_fails_closed_with_enumeration_safe_message() {
        let mut fixture = fixture();
        fixture.facts.agent_owners[1].owner_pubkey =
            Some(fixture.other_owner.public_key().to_hex());
        let error = validate(&fixture).expect_err("cross-owner target must fail");
        assert_eq!(error, DelegationError::OwnerMismatch);
        assert_eq!(error.public_message(), "delegation target unavailable");

        fixture.facts.agent_owners[1].owner_pubkey = None;
        let missing = validate(&fixture).expect_err("missing target must fail");
        assert_eq!(missing.public_message(), error.public_message());
    }

    #[test]
    fn unsigned_or_tampered_approval_fails_signature_verification() {
        let mut fixture = fixture();
        fixture.approval = tamper_signature(&fixture.approval);
        assert_eq!(
            validate(&fixture).expect_err("tampered approval must fail"),
            DelegationError::InvalidApprovalSignature
        );

        let mut value: serde_json::Value =
            serde_json::from_str(&fixture.approval.as_json()).expect("event json");
        value.as_object_mut().expect("event object").remove("sig");
        assert!(Event::from_json(value.to_string()).is_err());
    }

    #[test]
    fn expired_at_exact_deadline_fails_closed() {
        let mut fixture = fixture();
        fixture.facts.now = fixture.record.request.expires_at;
        fixture.facts.agent_owners[1].owner_pubkey =
            Some(fixture.other_owner.public_key().to_hex());
        assert_eq!(
            validate(&fixture).expect_err("expiry must win over owner details"),
            DelegationError::Expired
        );
    }

    #[test]
    fn trusted_tenant_is_bound_into_hash_claim_and_action_checks() {
        let fixture = fixture();
        let other_community = CommunityId::from_uuid(Uuid::from_u128(1));
        assert_ne!(
            immutable_request_hash(fixture.community_id, &fixture.record.request)
                .expect("primary tenant hash"),
            immutable_request_hash(other_community, &fixture.record.request)
                .expect("other tenant hash")
        );

        let wrong_claim_facts = ResolvedDelegationFacts {
            community_id: other_community,
            now: fixture.facts.now,
            agent_owners: fixture.facts.agent_owners.clone(),
            lineage: unavailable_lineage(),
        };
        assert_eq!(
            validate_for_claim(
                &fixture.record,
                Some(&fixture.context),
                Some(&fixture.approval),
                &wrong_claim_facts,
            )
            .expect_err("record cannot cross tenant hash namespaces"),
            DelegationError::InvalidRequestHash
        );

        let validated = validate(&fixture).expect("primary tenant validation");
        let mut wrong_action_facts = action_facts(&fixture);
        wrong_action_facts.community_id = other_community;
        assert_eq!(
            validate_next_action(&validated, &wrong_action_facts)
                .expect_err("action facts cannot cross tenants"),
            DelegationError::TenantMismatch
        );
        assert_eq!(
            DelegationError::TenantMismatch.public_message(),
            DelegationError::OwnerMismatch.public_message()
        );
    }

    #[test]
    fn root_lineage_is_bound_to_tenant_run_source_signer_and_expiry() {
        let mut case = fixture();
        root_token_mut(&mut case.facts.lineage).community_id =
            CommunityId::from_uuid(Uuid::from_u128(1));
        assert_eq!(
            validate(&case).expect_err("cross-tenant root lineage"),
            DelegationError::ParentBindingMismatch
        );

        let mut case = fixture();
        let wrong_source = case.record.request.target_agent.clone();
        root_token_mut(&mut case.facts.lineage).source_agent = wrong_source;
        assert_eq!(
            validate(&case).expect_err("wrong-source root lineage"),
            DelegationError::ParentBindingMismatch
        );

        let mut case = fixture();
        root_token_mut(&mut case.facts.lineage).run_id = Uuid::nil();
        assert_eq!(
            validate(&case).expect_err("missing root run identity"),
            DelegationError::ParentBindingMismatch
        );

        let mut case = fixture();
        let now = case.facts.now;
        root_token_mut(&mut case.facts.lineage).expires_at = now;
        assert_eq!(
            validate(&case).expect_err("stale root lineage"),
            DelegationError::ParentBindingMismatch
        );
    }

    #[test]
    fn nested_delegation_requires_server_derived_parent_lineage() {
        let parent = fixture();
        let (record, context, approval, mut facts) = nested_components(&parent);
        validate_for_claim(&record, Some(&context), Some(&approval), &facts)
            .expect("child extends authoritative parent path");

        facts.lineage = unavailable_lineage();
        assert_eq!(
            validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                .expect_err("nested request cannot supply its own ancestry"),
            DelegationError::ParentBindingMismatch
        );

        let request = DelegationRequest {
            delegation_id: Uuid::new_v4(),
            origin_event_id: "44".repeat(32),
            parent_approval_event_id: None,
            source_agent: parent.record.request.target_agent.clone(),
            target_agent: parent.record.request.source_agent.clone(),
            agent_path: vec![
                parent.record.request.target_agent.clone(),
                parent.record.request.source_agent.clone(),
            ],
            hop_budget: DEFAULT_HOP_BUDGET,
            max_turns: 4,
            cost_cap_microusd: None,
            token_budget: 50_000,
            idempotency_key: "cross-record-cycle".into(),
            expires_at: parent.record.request.expires_at,
        };
        let (record, context, approval) =
            approved_components(parent.community_id, &parent.owner, request);
        facts.lineage = live_parent_lineage(&parent);
        assert_eq!(
            validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                .expect_err("fresh direct record cannot erase active ancestry"),
            DelegationError::ParentBindingMismatch
        );
    }

    #[test]
    fn parent_lineage_is_bound_to_tenant_expiry_and_claim_identity() {
        let parent = fixture();
        let (record, context, approval, mut facts) = nested_components(&parent);
        parent_token_mut(&mut facts.lineage).community_id =
            CommunityId::from_uuid(Uuid::from_u128(1));
        assert_eq!(
            validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                .expect_err("cross-tenant parent rejection"),
            DelegationError::ParentBindingMismatch
        );

        let (record, context, approval, mut facts) = nested_components(&parent);
        parent_token_mut(&mut facts.lineage).expires_at = facts.now;
        assert_eq!(
            validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                .expect_err("stale parent rejection"),
            DelegationError::ParentBindingMismatch
        );
    }

    #[test]
    fn conflicting_replay_never_yields_execution_permit() {
        let fixture = fixture();
        let validated = validate(&fixture).expect("stateless validation");
        for outcome in [
            AtomicClaimOutcome::DelegationConflict,
            AtomicClaimOutcome::ApprovalConflict,
            AtomicClaimOutcome::IdempotencyConflict,
        ] {
            assert_eq!(
                classify_claim_outcome(&validated, fixture.facts.now, outcome)
                    .expect_err("conflicting durable key reuse must fail"),
                DelegationError::ApprovalReplay
            );
        }
    }

    #[test]
    fn exact_duplicate_is_suppressed_not_reexecuted() {
        let fixture = fixture();
        let validated = validate(&fixture).expect("stateless validation");
        for outcome in [
            AtomicClaimOutcome::ExactDuplicatePending,
            AtomicClaimOutcome::ExactDuplicateCompleted,
        ] {
            let expected_claim = validated.claim();
            let decision = classify_claim_outcome(&validated, fixture.facts.now, outcome)
                .expect("durably pending/completed duplicate is a successful collapse");
            assert_eq!(
                decision,
                DelegationClaimDisposition::DuplicateSuppressed(expected_claim)
            );
        }
    }

    #[test]
    fn stripped_context_fails_before_execution() {
        let fixture = fixture();
        assert_eq!(
            validate_for_claim(
                &fixture.record,
                None,
                Some(&fixture.approval),
                &fixture.facts
            )
            .expect_err("context is mandatory"),
            DelegationError::MissingContext
        );
    }

    #[test]
    fn missing_approval_fails_before_execution() {
        let fixture = fixture();
        assert_eq!(
            validate_for_claim(
                &fixture.record,
                Some(&fixture.context),
                None,
                &fixture.facts
            )
            .expect_err("approval event is mandatory"),
            DelegationError::MissingApproval
        );
    }

    #[test]
    fn context_mutation_cannot_escalate_turns() {
        let mut fixture = fixture();
        fixture.context.remaining_turns = fixture.record.request.max_turns + 1;
        assert_eq!(
            validate(&fixture).expect_err("turn escalation must fail"),
            DelegationError::TurnLimitExceeded
        );
    }

    #[test]
    fn request_hash_mutation_is_detected() {
        let mut fixture = fixture();
        fixture.record.immutable_request_hash = "22".repeat(32);
        assert_eq!(
            validate(&fixture).expect_err("stored hash mismatch must fail"),
            DelegationError::InvalidRequestHash
        );
    }

    #[test]
    fn context_request_mutation_is_detected_even_when_rehashed() {
        let mut fixture = fixture();
        fixture.context.request.idempotency_key = "forged-context".into();
        fixture.context.immutable_request_hash =
            immutable_request_hash(fixture.community_id, &fixture.context.request)
                .expect("forged hash is well formed");
        assert_eq!(
            validate(&fixture).expect_err("context cannot replace stored request"),
            DelegationError::ContextBindingMismatch
        );
    }

    #[test]
    fn signed_approval_for_wrong_request_is_rejected() {
        let mut fixture = fixture();
        let mut other_request = fixture.record.request.clone();
        other_request.idempotency_key = "different-request".into();
        fixture.approval = build_operator_approval_event(
            &fixture.owner,
            fixture.community_id,
            &other_request,
            1_800_000_001,
        )
        .expect("well-formed but wrong approval");
        fixture.record.operator_approval_event_id = Some(fixture.approval.id.to_hex());
        fixture.context.operator_approval_event_id = fixture.approval.id.to_hex();
        assert_eq!(
            validate(&fixture).expect_err("wrong request binding must fail"),
            DelegationError::ApprovalBindingMismatch
        );
    }

    #[test]
    fn repeated_agent_path_is_a_cycle() {
        let fixture = fixture();
        let request = DelegationRequest {
            source_agent: fixture.record.request.target_agent.clone(),
            target_agent: fixture.record.request.source_agent.clone(),
            agent_path: vec![
                fixture.record.request.source_agent.clone(),
                fixture.record.request.target_agent.clone(),
                fixture.record.request.source_agent.clone(),
            ],
            hop_budget: MAX_HOP_BUDGET,
            ..fixture.record.request
        };
        assert_eq!(
            request.validate().expect_err("A to B to A is a cycle"),
            DelegationError::DelegationCycle
        );
    }

    #[test]
    fn unknown_cost_under_cap_is_refused() {
        let mut fixture = fixture();
        fixture.record.request.cost_cap_microusd = Some(1_000_000);
        fixture.record.immutable_request_hash =
            immutable_request_hash(fixture.community_id, &fixture.record.request)
                .expect("new request hash");
        fixture.approval = build_operator_approval_event(
            &fixture.owner,
            fixture.community_id,
            &fixture.record.request,
            1_800_000_001,
        )
        .expect("new approval");
        fixture.record.operator_approval_event_id = Some(fixture.approval.id.to_hex());
        fixture.context = DelegationExecutionContext::from_approved_record(
            fixture.community_id,
            &fixture.record,
            8,
        )
        .expect("updated context");
        let validated = validate(&fixture).expect("stateless validation");
        let facts = action_facts(&fixture);
        assert_eq!(
            validate_next_action(&validated, &facts).expect_err("unknown cost cannot pass a cap"),
            DelegationError::CostUnknown
        );
    }

    #[test]
    fn cumulative_cost_cap_is_checked_before_every_action() {
        let mut fixture = fixture();
        fixture.record.request.cost_cap_microusd = Some(1_000_000);
        fixture.record.immutable_request_hash =
            immutable_request_hash(fixture.community_id, &fixture.record.request)
                .expect("new request hash");
        fixture.approval = build_operator_approval_event(
            &fixture.owner,
            fixture.community_id,
            &fixture.record.request,
            1_800_000_001,
        )
        .expect("new approval");
        fixture.record.operator_approval_event_id = Some(fixture.approval.id.to_hex());
        fixture.context = DelegationExecutionContext::from_approved_record(
            fixture.community_id,
            &fixture.record,
            8,
        )
        .expect("updated context");
        let validated = validate(&fixture).expect("stateless validation");
        let mut facts = action_facts(&fixture);
        facts.cost_committed_microusd = Some(750_000);
        facts.action_cost_reservation_microusd = Some(250_000);
        let action = validate_next_action(&validated, &facts).expect("cap boundary is inclusive");
        assert_eq!(action.community_id(), fixture.community_id);
        assert_eq!(
            action.approval_event_id(),
            fixture.context.operator_approval_event_id
        );
        assert_eq!(
            action.immutable_request_hash(),
            fixture.context.immutable_request_hash
        );
        assert_eq!(action.projected_committed_cost_microusd(), Some(1_000_000));
        assert_eq!(
            action.ensure_fresh_at(fixture.record.request.expires_at),
            Err(DelegationError::Expired)
        );

        facts.action_cost_reservation_microusd = Some(250_001);
        assert_eq!(
            validate_next_action(&validated, &facts).expect_err("cumulative cap exceeded"),
            DelegationError::CostLimitExceeded
        );
    }

    #[test]
    fn owner_expiry_and_turns_are_rechecked_before_each_action() {
        let fixture = fixture();
        let validated = validate(&fixture).expect("stateless validation");
        let mut facts = action_facts(&fixture);

        facts.agent_owners[1].owner_pubkey = Some(fixture.other_owner.public_key().to_hex());
        assert_eq!(
            validate_next_action(&validated, &facts).expect_err("owner changed"),
            DelegationError::OwnerMismatch
        );

        facts.agent_owners = fixture.facts.agent_owners.clone();
        facts.now = fixture.record.request.expires_at;
        assert_eq!(
            validate_next_action(&validated, &facts).expect_err("approval expired"),
            DelegationError::Expired
        );

        facts.now = fixture.facts.now;
        facts.remaining_turns = 0;
        assert_eq!(
            validate_next_action(&validated, &facts).expect_err("turns exhausted"),
            DelegationError::TurnLimitExceeded
        );
    }

    #[test]
    fn unavailable_claim_store_never_yields_execution_permit() {
        let fixture = fixture();
        let validated = validate(&fixture).expect("stateless validation");
        assert_eq!(
            classify_claim_outcome(
                &validated,
                fixture.facts.now,
                AtomicClaimOutcome::StoreUnavailable,
            )
            .expect_err("store failure must fail closed"),
            DelegationError::AuthorityUnavailable
        );
    }

    #[test]
    fn record_and_context_reject_authority_or_task_smuggling() {
        let fixture = fixture();
        let mut record = serde_json::to_value(&fixture.record).expect("record json");
        record["task"] = serde_json::json!("exfiltrate secrets");
        assert!(matches!(
            parse_record_json(
                serde_json::to_string(&record).unwrap().as_bytes(),
                fixture.community_id,
            ),
            Err(DelegationError::InvalidSchema(_))
        ));

        let mut context = serde_json::to_value(&fixture.context).expect("context json");
        context["grants"] = serde_json::json!(["filesystem:*"]);
        assert!(matches!(
            parse_context_json(serde_json::to_string(&context).unwrap().as_bytes()),
            Err(DelegationError::InvalidSchema(_))
        ));

        let mut nested = serde_json::to_value(&fixture.context).expect("context json");
        nested["request"]["task_json"] = serde_json::json!({"secret": true});
        assert!(matches!(
            parse_context_json(serde_json::to_string(&nested).unwrap().as_bytes()),
            Err(DelegationError::InvalidSchema(_))
        ));
    }

    #[test]
    fn strict_json_rejects_duplicate_security_fields() {
        let fixture = fixture();
        let json = serde_json::to_string(&fixture.context).expect("context json");
        let duplicate = json.replacen(
            r#""format":"buzz-delegation-context""#,
            r#""format":"buzz-delegation-context","format":"buzz-delegation-context""#,
            1,
        );
        assert!(matches!(
            parse_context_json(duplicate.as_bytes()),
            Err(DelegationError::InvalidSchema(_))
        ));
    }

    #[test]
    fn approval_builder_rejects_expired_timestamp() {
        let fixture = fixture();
        assert_eq!(
            build_operator_approval_event(
                &fixture.owner,
                fixture.community_id,
                &fixture.record.request,
                fixture.record.request.expires_at,
            )
            .expect_err("approval cannot be signed at its deadline"),
            DelegationError::InvalidTimestamp
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ManifestOutcome {
        expect: String,
        error_code: Option<String>,
        error_layer: Option<String>,
    }

    fn manifest_success(expect: &'static str) -> ManifestOutcome {
        ManifestOutcome {
            expect: expect.to_owned(),
            error_code: None,
            error_layer: None,
        }
    }

    fn manifest_rejection(error: DelegationError) -> ManifestOutcome {
        ManifestOutcome {
            expect: "reject".to_owned(),
            error_code: Some(error.code().to_owned()),
            error_layer: None,
        }
    }

    fn rebuild_with_cost_cap(fixture: &mut Fixture, cap: u64) {
        fixture.record.request.cost_cap_microusd = Some(cap);
        fixture.record.immutable_request_hash =
            immutable_request_hash(fixture.community_id, &fixture.record.request)
                .expect("cost-capped request hash");
        fixture.approval = build_operator_approval_event(
            &fixture.owner,
            fixture.community_id,
            &fixture.record.request,
            1_800_000_001,
        )
        .expect("cost-capped approval");
        fixture.record.operator_approval_event_id = Some(fixture.approval.id.to_hex());
        fixture.context = DelegationExecutionContext::from_approved_record(
            fixture.community_id,
            &fixture.record,
            fixture.record.request.max_turns,
        )
        .expect("cost-capped context");
    }

    fn observe_manifest_case(name: &str) -> ManifestOutcome {
        match name {
            "valid_same_owner" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let disposition = classify_claim_outcome(
                    &validated,
                    fixture.facts.now,
                    AtomicClaimOutcome::AcquiredAndEnqueued,
                )
                .expect("durable enqueue");
                assert_eq!(disposition, DelegationClaimDisposition::DurablyEnqueued);
                manifest_success("durably_enqueued")
            }
            "cross_owner" => {
                let mut fixture = fixture();
                fixture.facts.agent_owners[1].owner_pubkey =
                    Some(fixture.other_owner.public_key().to_hex());
                manifest_rejection(validate(&fixture).expect_err("cross-owner rejection"))
            }
            "target_enumeration_probe" => {
                let mut fixture = fixture();
                fixture.facts.agent_owners[1].owner_pubkey =
                    Some(fixture.other_owner.public_key().to_hex());
                let wrong_owner =
                    validate(&fixture).expect_err("wrong-owner target must be refused");

                fixture.facts.agent_owners[1].owner_pubkey = None;
                let invisible = validate(&fixture).expect_err("invisible target must be refused");
                assert_eq!(
                    invisible.public_message(),
                    wrong_owner.public_message(),
                    "target visibility must not change the public refusal"
                );
                manifest_rejection(invisible)
            }
            "cross_tenant" => {
                let mut fixture = fixture();
                fixture.facts.community_id = CommunityId::from_uuid(Uuid::from_u128(1));
                manifest_rejection(validate(&fixture).expect_err("cross-tenant rejection"))
            }
            "cross_tenant_action" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let mut facts = action_facts(&fixture);
                facts.community_id = CommunityId::from_uuid(Uuid::from_u128(1));
                manifest_rejection(
                    validate_next_action(&validated, &facts)
                        .expect_err("cross-tenant action rejection"),
                )
            }
            "unsigned_approval" => {
                let fixture = fixture();
                let mut value: serde_json::Value =
                    serde_json::from_str(&fixture.approval.as_json()).expect("event json");
                value.as_object_mut().expect("event object").remove("sig");
                assert!(
                    Event::from_json(value.to_string()).is_err(),
                    "unsigned event must fail before delegation validation"
                );
                ManifestOutcome {
                    expect: "reject".to_owned(),
                    error_code: Some("invalid_event".to_owned()),
                    error_layer: Some("nostr_event_decode".to_owned()),
                }
            }
            "tampered_approval" => {
                let mut fixture = fixture();
                fixture.approval = tamper_signature(&fixture.approval);
                manifest_rejection(validate(&fixture).expect_err("tampered signature rejection"))
            }
            "missing_approval" => {
                let fixture = fixture();
                manifest_rejection(
                    validate_for_claim(
                        &fixture.record,
                        Some(&fixture.context),
                        None,
                        &fixture.facts,
                    )
                    .expect_err("missing approval rejection"),
                )
            }
            "expired" => {
                let mut fixture = fixture();
                fixture.facts.now = fixture.record.request.expires_at;
                manifest_rejection(validate(&fixture).expect_err("expiry rejection"))
            }
            "post_expiry_claim_token" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                manifest_rejection(
                    classify_claim_outcome(
                        &validated,
                        fixture.record.request.expires_at,
                        AtomicClaimOutcome::AcquiredAndEnqueued,
                    )
                    .expect_err("transaction-time claim expiry rejection"),
                )
            }
            "post_expiry_action_token" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                manifest_rejection(
                    classify_action_outcome(
                        &action,
                        fixture.record.request.expires_at,
                        DelegationActionStoreOutcome::AppliedAndRecorded,
                    )
                    .expect_err("transaction-time expiry rejection"),
                )
            }
            "nil_delegation_id" => {
                let mut fixture = fixture();
                fixture.record.request.delegation_id = Uuid::nil();
                manifest_rejection(
                    fixture
                        .record
                        .request
                        .validate()
                        .expect_err("nil delegation id rejection"),
                )
            }
            "action_cas_applied" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                assert_eq!(
                    classify_action_outcome(
                        &action,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::AppliedAndRecorded,
                    )
                    .expect("durable action record"),
                    DelegationActionDisposition::DurablyRecorded,
                );
                manifest_success("action_durably_recorded")
            }
            "sequential_action_turns" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let first_facts = action_facts(&fixture);
                let first = validate_next_action(&validated, &first_facts)
                    .expect("first action validation");
                assert_eq!(
                    classify_action_outcome(
                        &first,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::AppliedAndRecorded,
                    )
                    .expect("first durable action record"),
                    DelegationActionDisposition::DurablyRecorded,
                );

                let mut second_facts = first_facts;
                second_facts.remaining_turns = first.remaining_turns_before() - 1;
                let second = validate_next_action(&validated, &second_facts)
                    .expect("attenuated second action validation");
                assert_eq!(
                    second.remaining_turns_before(),
                    fixture.context.remaining_turns - 1
                );
                assert_eq!(
                    classify_action_outcome(
                        &second,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::AppliedAndRecorded,
                    )
                    .expect("second durable action record"),
                    DelegationActionDisposition::DurablyRecorded,
                );
                manifest_success("action_durably_recorded")
            }
            "action_turns_above_context" => {
                let mut fixture = fixture();
                fixture.context.remaining_turns -= 1;
                let validated = validate(&fixture).expect("attenuated context is valid");
                let mut facts = action_facts(&fixture);
                facts.remaining_turns += 1;
                manifest_rejection(
                    validate_next_action(&validated, &facts)
                        .expect_err("durable counter cannot exceed context ceiling"),
                )
            }
            "ownership_change_before_action_cas" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                let mut changed_owners = fixture.facts.agent_owners.clone();
                let original_owner = changed_owners[1].owner_pubkey.clone();
                changed_owners[1].ownership_revision += 1;
                assert_eq!(changed_owners[1].owner_pubkey, original_owner);
                assert_eq!(
                    action.ensure_owner_snapshot(&changed_owners),
                    Err(DelegationError::ActionConflict),
                    "revision-only ABA change must invalidate the pre-CAS action token"
                );
                manifest_rejection(
                    classify_action_outcome(
                        &action,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::AuthorityConflict,
                    )
                    .expect_err("locked ownership revision conflict"),
                )
            }
            "ownership_change_before_outbox" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                assert_eq!(
                    classify_action_outcome(
                        &action,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::AppliedAndRecorded,
                    )
                    .expect("durable action and outbox record"),
                    DelegationActionDisposition::DurablyRecorded,
                );

                // Model A -> B -> A after commit: the pubkey is identical, but
                // the monotonic revision has advanced before the outbox effect.
                let mut changed_owners = fixture.facts.agent_owners.clone();
                let original_owner = changed_owners[1].owner_pubkey.clone();
                changed_owners[1].ownership_revision += 1;
                assert_eq!(changed_owners[1].owner_pubkey, original_owner);
                manifest_rejection(
                    action
                        .ensure_owner_snapshot(&changed_owners)
                        .expect_err("revision-only ABA must cancel outbox dispatch"),
                )
            }
            "action_claim_conflict" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                manifest_rejection(
                    classify_action_outcome(
                        &action,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::ClaimConflict,
                    )
                    .expect_err("claim identity conflict"),
                )
            }
            "action_state_conflict" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                manifest_rejection(
                    classify_action_outcome(
                        &action,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::StateConflict,
                    )
                    .expect_err("stale action state conflict"),
                )
            }
            "action_store_unavailable" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                manifest_rejection(
                    classify_action_outcome(
                        &action,
                        fixture.facts.now,
                        DelegationActionStoreOutcome::StoreUnavailable,
                    )
                    .expect_err("action store unavailable"),
                )
            }
            "delegation_id_reuse" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                manifest_rejection(
                    classify_claim_outcome(
                        &validated,
                        fixture.facts.now,
                        AtomicClaimOutcome::DelegationConflict,
                    )
                    .expect_err("delegation id collision rejection"),
                )
            }
            "approval_replay" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                manifest_rejection(
                    classify_claim_outcome(
                        &validated,
                        fixture.facts.now,
                        AtomicClaimOutcome::ApprovalConflict,
                    )
                    .expect_err("approval replay rejection"),
                )
            }
            "idempotency_replay" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                manifest_rejection(
                    classify_claim_outcome(
                        &validated,
                        fixture.facts.now,
                        AtomicClaimOutcome::IdempotencyConflict,
                    )
                    .expect_err("idempotency replay rejection"),
                )
            }
            "exact_duplicate_pending" | "exact_duplicate_completed" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let outcome = if name == "exact_duplicate_pending" {
                    AtomicClaimOutcome::ExactDuplicatePending
                } else {
                    AtomicClaimOutcome::ExactDuplicateCompleted
                };
                assert_eq!(
                    classify_claim_outcome(&validated, fixture.facts.now, outcome)
                        .expect("exact duplicate collapse"),
                    DelegationClaimDisposition::DuplicateSuppressed(validated.claim())
                );
                manifest_success("duplicate_suppressed")
            }
            "context_stripped" => {
                let fixture = fixture();
                manifest_rejection(
                    validate_for_claim(
                        &fixture.record,
                        None,
                        Some(&fixture.approval),
                        &fixture.facts,
                    )
                    .expect_err("missing context rejection"),
                )
            }
            "context_binding_forged" => {
                let mut fixture = fixture();
                fixture.context.request.idempotency_key = "forged-context".into();
                manifest_rejection(validate(&fixture).expect_err("forged context rejection"))
            }
            "approval_binding_forged" => {
                let mut fixture = fixture();
                let mut other_request = fixture.record.request.clone();
                other_request.idempotency_key = "different-request".into();
                fixture.approval = build_operator_approval_event(
                    &fixture.owner,
                    fixture.community_id,
                    &other_request,
                    1_800_000_001,
                )
                .expect("well-formed wrong approval");
                fixture.record.operator_approval_event_id = Some(fixture.approval.id.to_hex());
                fixture.context.operator_approval_event_id = fixture.approval.id.to_hex();
                manifest_rejection(validate(&fixture).expect_err("approval binding rejection"))
            }
            "delegation_cycle" => {
                let fixture = fixture();
                let request = DelegationRequest {
                    source_agent: fixture.record.request.target_agent.clone(),
                    target_agent: fixture.record.request.source_agent.clone(),
                    agent_path: vec![
                        fixture.record.request.source_agent.clone(),
                        fixture.record.request.target_agent.clone(),
                        fixture.record.request.source_agent.clone(),
                    ],
                    hop_budget: MAX_HOP_BUDGET,
                    ..fixture.record.request
                };
                manifest_rejection(request.validate().expect_err("cycle rejection"))
            }
            "parent_context_stripped" => {
                let parent = fixture();
                let (record, context, approval, mut facts) = nested_components(&parent);
                facts.lineage = unavailable_lineage();
                manifest_rejection(
                    validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                        .expect_err("stripped parent lineage rejection"),
                )
            }
            "cross_tenant_parent" => {
                let parent = fixture();
                let (record, context, approval, mut facts) = nested_components(&parent);
                parent_token_mut(&mut facts.lineage).community_id =
                    CommunityId::from_uuid(Uuid::from_u128(1));
                manifest_rejection(
                    validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                        .expect_err("cross-tenant parent rejection"),
                )
            }
            "stale_parent" => {
                let parent = fixture();
                let (record, context, approval, mut facts) = nested_components(&parent);
                parent_token_mut(&mut facts.lineage).expires_at = facts.now;
                manifest_rejection(
                    validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                        .expect_err("stale parent rejection"),
                )
            }
            "cross_record_cycle" => {
                let parent = fixture();
                let request = DelegationRequest {
                    delegation_id: Uuid::new_v4(),
                    origin_event_id: "44".repeat(32),
                    parent_approval_event_id: None,
                    source_agent: parent.record.request.target_agent.clone(),
                    target_agent: parent.record.request.source_agent.clone(),
                    agent_path: vec![
                        parent.record.request.target_agent.clone(),
                        parent.record.request.source_agent.clone(),
                    ],
                    hop_budget: DEFAULT_HOP_BUDGET,
                    max_turns: 4,
                    cost_cap_microusd: None,
                    token_budget: 50_000,
                    idempotency_key: "cross-record-cycle".into(),
                    expires_at: parent.record.request.expires_at,
                };
                let (record, context, approval) =
                    approved_components(parent.community_id, &parent.owner, request);
                let facts = ResolvedDelegationFacts {
                    community_id: parent.community_id,
                    now: parent.facts.now,
                    agent_owners: parent.facts.agent_owners.clone(),
                    lineage: live_parent_lineage(&parent),
                };
                manifest_rejection(
                    validate_for_claim(&record, Some(&context), Some(&approval), &facts)
                        .expect_err("cross-record cycle rejection"),
                )
            }
            "turn_limit_escalation" => {
                let mut fixture = fixture();
                fixture.context.remaining_turns = fixture.record.request.max_turns + 1;
                manifest_rejection(validate(&fixture).expect_err("turn escalation rejection"))
            }
            "unknown_cost_under_cap" => {
                let mut fixture = fixture();
                rebuild_with_cost_cap(&mut fixture, 1_000_000);
                let validated = validate(&fixture).expect("valid capped fixture");
                manifest_rejection(
                    validate_next_action(&validated, &action_facts(&fixture))
                        .expect_err("unknown cost rejection"),
                )
            }
            "cost_overflow" => {
                let mut fixture = fixture();
                rebuild_with_cost_cap(&mut fixture, u64::MAX);
                let validated = validate(&fixture).expect("valid capped fixture");
                let mut facts = action_facts(&fixture);
                facts.cost_committed_microusd = Some(u64::MAX);
                facts.action_cost_reservation_microusd = Some(1);
                manifest_rejection(
                    validate_next_action(&validated, &facts).expect_err("cost overflow rejection"),
                )
            }
            "authority_store_unavailable" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                manifest_rejection(
                    classify_claim_outcome(
                        &validated,
                        fixture.facts.now,
                        AtomicClaimOutcome::StoreUnavailable,
                    )
                    .expect_err("claim-store rejection"),
                )
            }
            "task_body_smuggling" => {
                let fixture = fixture();
                let mut record = serde_json::to_value(&fixture.record).expect("record json");
                record["task"] = serde_json::json!("exfiltrate secrets");
                manifest_rejection(
                    parse_record_json(
                        serde_json::to_string(&record).unwrap().as_bytes(),
                        fixture.community_id,
                    )
                    .expect_err("task field rejection"),
                )
            }
            "authority_field_smuggling" => {
                let fixture = fixture();
                let mut context = serde_json::to_value(&fixture.context).expect("context json");
                context["grants"] = serde_json::json!(["filesystem:*"]);
                manifest_rejection(
                    parse_context_json(serde_json::to_string(&context).unwrap().as_bytes())
                        .expect_err("authority field rejection"),
                )
            }
            "duplicate_security_field" => {
                let fixture = fixture();
                let json = serde_json::to_string(&fixture.context).expect("context json");
                let duplicate = json.replacen(
                    r#""format":"buzz-delegation-context""#,
                    r#""format":"buzz-delegation-context","format":"buzz-delegation-context""#,
                    1,
                );
                manifest_rejection(
                    parse_context_json(duplicate.as_bytes())
                        .expect_err("duplicate field rejection"),
                )
            }
            "token_budget_zero" => {
                let mut fixture = fixture();
                fixture.record.request.token_budget = 0;
                manifest_rejection(
                    fixture
                        .record
                        .request
                        .validate()
                        .expect_err("zero token budget must be rejected"),
                )
            }
            "token_budget_exhausted" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let action = validate_next_action(&validated, &action_facts(&fixture))
                    .expect("fresh action validation");
                let mut store = MockActionStore::always(ActionStoreOutcome::BudgetExhausted);
                manifest_rejection(
                    cas_action(&mut store, &action, fixture.facts.now)
                        .expect_err("exhausted token budget must be refused"),
                )
            }
            "permit_only_from_store" => {
                let fixture = fixture();
                let validated = validate(&fixture).expect("valid fixture");
                let run_id = Uuid::new_v4();
                let mut denying_store = MockClaimStore::always(ClaimStoreOutcome::StoreUnavailable);
                assert!(
                    claim_and_enqueue(&mut denying_store, &validated, run_id, fixture.facts.now)
                        .is_err(),
                    "every non-AcquiredAndEnqueued outcome must yield no permit"
                );
                let mut granting_store =
                    MockClaimStore::always(ClaimStoreOutcome::AcquiredAndEnqueued);
                match claim_and_enqueue(&mut granting_store, &validated, run_id, fixture.facts.now)
                    .expect("granting store yields a permit")
                {
                    ClaimDisposition::Permit(permit) => {
                        assert_eq!(permit.run_id(), run_id);
                        assert_eq!(permit.delegation_id(), fixture.record.request.delegation_id);
                    }
                    ClaimDisposition::DuplicateSuppressed(_) => {
                        panic!("AcquiredAndEnqueued must yield a permit, not a duplicate")
                    }
                }
                manifest_success("durably_enqueued")
            }
            "lineage_unavailable_fails_closed" => {
                let mut fixture = fixture();
                fixture.facts.lineage = ResolvedDelegationLineage::unavailable();
                manifest_rejection(
                    validate_for_claim(
                        &fixture.record,
                        Some(&fixture.context),
                        Some(&fixture.approval),
                        &fixture.facts,
                    )
                    .expect_err("unavailable lineage must fail closed"),
                )
            }
            unknown => panic!("manifest case has no executable runner: {unknown}"),
        }
    }

    #[test]
    fn v1_vector_is_rejected_after_v2() {
        // The exact pre-Slice-4 golden request JSON (no `token_budget`) and its
        // v1 hash (domain `buzz-delegation/request/v1\0`), frozen here so a v2
        // regression that silently re-accepts v1-shaped requests is caught.
        let v1_request_json = r#"{
            "delegation_id": "01890f47-2fb0-7cc0-98c4-dc0c0c07398f",
            "origin_event_id": "0101010101010101010101010101010101010101010101010101010101010101",
            "parent_approval_event_id": null,
            "source_agent": "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            "target_agent": "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
            "agent_path": [
                "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
                "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5"
            ],
            "hop_budget": 2,
            "max_turns": 8,
            "cost_cap_microusd": 1500000,
            "idempotency_key": "dg-vector-001",
            "expires_at": 1800000300
        }"#;
        let v1_golden_hash = "db50bdae160774dc27691848de63f5f490dc64da65f62d095607fbc6d86a7042";

        // The v1-shaped JSON no longer strictly parses: `token_budget` is a
        // required field on `DelegationRequest` (Slice 4, N4).
        let parse_error = serde_json::from_str::<DelegationRequest>(v1_request_json)
            .expect_err("v1-shaped request (no token_budget) must not parse under v2");
        assert!(
            parse_error.to_string().contains("token_budget"),
            "expected a missing-field error naming token_budget, got: {parse_error}"
        );

        // Even if a caller reconstructed the v1 fields with a placeholder
        // token_budget, the v1 golden hash must not verify: the v2 domain
        // separator and the extra hashed field change every output.
        let community_id = CommunityId::from_uuid(
            Uuid::parse_str("3580ca9b-47b4-4af9-b22a-1068778f26c6").expect("golden community UUID"),
        );
        let mut request: DelegationRequest = {
            let mut value: serde_json::Value =
                serde_json::from_str(v1_request_json).expect("v1 json parses as raw value");
            value["token_budget"] = serde_json::json!(50_000u64);
            serde_json::from_value(value).expect("v1 fields plus token_budget parse under v2")
        };
        assert_ne!(
            immutable_request_hash(community_id, &request).expect("v2 request hashes"),
            v1_golden_hash,
            "v1 golden hash must not verify against any v2-computed hash"
        );

        // Confirm the mismatch is specifically the domain/field change, not an
        // unrelated fixture drift: hashing the identical fields again is
        // deterministic and still differs from the v1 vector.
        request.idempotency_key = "dg-vector-001".to_owned();
        assert_ne!(
            immutable_request_hash(community_id, &request).expect("v2 request hashes again"),
            v1_golden_hash
        );
    }

    #[test]
    fn fixture_manifest_executes_every_declared_case() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/nips/NIP-DG.fixtures.json"))
                .expect("fixture manifest parses");
        assert_eq!(manifest["version"].as_u64(), Some(u64::from(VERSION)));
        assert_eq!(
            manifest["approval_kind"].as_u64(),
            Some(u64::from(KIND_DELEGATION_APPROVAL))
        );
        let cases = manifest["cases"].as_array().expect("cases array");
        let mut names = HashSet::new();
        for case in cases {
            let name = case["name"].as_str().expect("fixture name");
            assert!(names.insert(name), "duplicate fixture name: {name}");
            let expected = ManifestOutcome {
                expect: case["expect"]
                    .as_str()
                    .expect("fixture expectation")
                    .to_owned(),
                error_code: case
                    .get("error_code")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                error_layer: case
                    .get("error_layer")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            };
            assert_eq!(
                observe_manifest_case(name),
                expected,
                "fixture outcome drifted: {name}"
            );
        }
        let expected_names: HashSet<&str> = [
            "valid_same_owner",
            "cross_owner",
            "target_enumeration_probe",
            "cross_tenant",
            "cross_tenant_action",
            "unsigned_approval",
            "tampered_approval",
            "missing_approval",
            "expired",
            "post_expiry_claim_token",
            "post_expiry_action_token",
            "nil_delegation_id",
            "action_cas_applied",
            "sequential_action_turns",
            "action_turns_above_context",
            "ownership_change_before_action_cas",
            "ownership_change_before_outbox",
            "action_claim_conflict",
            "action_state_conflict",
            "action_store_unavailable",
            "delegation_id_reuse",
            "approval_replay",
            "idempotency_replay",
            "exact_duplicate_pending",
            "exact_duplicate_completed",
            "context_stripped",
            "context_binding_forged",
            "approval_binding_forged",
            "delegation_cycle",
            "parent_context_stripped",
            "cross_tenant_parent",
            "stale_parent",
            "cross_record_cycle",
            "turn_limit_escalation",
            "unknown_cost_under_cap",
            "cost_overflow",
            "authority_store_unavailable",
            "task_body_smuggling",
            "authority_field_smuggling",
            "duplicate_security_field",
            "token_budget_zero",
            "token_budget_exhausted",
            "permit_only_from_store",
            "lineage_unavailable_fails_closed",
        ]
        .into_iter()
        .collect();
        assert_eq!(names, expected_names, "fixture contract case set drifted");

        let vector = &manifest["hash_vector"];
        let community_id = CommunityId::from_uuid(
            Uuid::parse_str(
                vector["community_id"]
                    .as_str()
                    .expect("golden community id"),
            )
            .expect("golden community UUID"),
        );
        let request: DelegationRequest = serde_json::from_value(vector["request"].clone())
            .expect("golden request must match the strict schema");
        let expected = vector["immutable_request_hash"]
            .as_str()
            .expect("golden hash string");
        assert_eq!(
            immutable_request_hash(community_id, &request).expect("golden request must hash"),
            expected,
            "cross-language immutable hash contract drifted"
        );
    }
}
