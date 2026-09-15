//! Delegation (operator-approved agent-to-agent delegation, kind 43007)
//! durable persistence: the atomic three-key claim + outbox row, the
//! per-action compare-and-swap, settlement, sweep, and owner-snapshot
//! resolution.
//!
//! All security-bearing pubkeys/event ids are stored as `BYTEA` and are
//! hex-encoded at the crate boundary (`buzz_core::delegation` types use hex
//! `String`). Never uses string interpolation for query values — all user
//! data goes through bind parameters. States use `TEXT` columns deliberately
//! (Slice 3 `::text` casting lesson) so no Postgres enum casts are needed.
//!
//! ## Blocking discipline
//!
//! [`buzz_core::delegation::DelegationClaimStore`] and
//! `DelegationActionStore` are synchronous traits (NIP-DG's `validate_for_claim`
//! and Schnorr verification are CPU-bound and run under
//! `tokio::task::spawn_blocking`; the claim/action transaction seam that
//! immediately follows them is written the same way so the caller does one
//! blocking hop, not two). [`PgDelegationClaimStore`] and
//! [`PgDelegationActionStore`] bridge to async `sqlx` via the private
//! `block_on_current` helper (`tokio::task::block_in_place` wrapping
//! `Handle::current().block_on`). This requires the current worker thread's
//! runtime to be `flavor = "multi_thread"` — `block_in_place` hands this
//! thread's other queued work to a sibling worker for the duration of the
//! block, which a bare `Handle::current().block_on` does not do and which a
//! single-threaded (`#[tokio::main]`/`#[tokio::test]` default) runtime
//! cannot do at all (it panics). Callers MUST run on a multi-thread runtime;
//! calling these adapters from inside `tokio::task::spawn_blocking` is also
//! sound and preferred where the caller already has that hop available.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use buzz_core::delegation::{
    ActionStoreOutcome, ClaimStoreOutcome, DelegationActionStore, DelegationClaim,
    DelegationClaimStore, DelegationError, ResolvedAgentOwner, ValidatedDelegationAction,
};
use buzz_core::CommunityId;

use crate::error::Result;
use crate::store::channel_members;
use crate::Db;
use buzz_datastore_tracing::datastore_span;

fn hex_encode(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn hex_decode(label: &'static str, value: &str) -> Result<Vec<u8>> {
    hex::decode(value)
        .map_err(|_| crate::error::DbError::InvalidData(format!("invalid hex for {label}")))
}

/// `ResolvedAgentOwner` deliberately does not derive `serde::Serialize` (it
/// carries trusted runtime facts, not a wire type); build the
/// `delegation_actions.owner_snapshot` JSONB manually from its public fields
/// instead of adding a derive to a `buzz-core` security-facing type.
fn owner_snapshot_to_json(snapshot: &[ResolvedAgentOwner]) -> serde_json::Value {
    serde_json::Value::Array(
        snapshot
            .iter()
            .map(|owner| {
                serde_json::json!({
                    "agent_pubkey": owner.agent_pubkey,
                    "owner_pubkey": owner.owner_pubkey,
                    "ownership_revision": owner.ownership_revision,
                })
            })
            .collect(),
    )
}

/// Resolve current owner/visibility snapshots for a set of agents in one
/// query. Missing rows come back with `owner_pubkey: None` so a nonexistent
/// agent and a foreign-owned agent are indistinguishable to the caller
/// (Slice 4, Risk 6 enumeration-safety ordering).
#[datastore_span(name = "resolve_agent_owners", system = "postgresql")]
pub async fn resolve_agent_owners(
    pool: &PgPool,
    community_id: CommunityId,
    agents: &[String],
) -> Result<Vec<ResolvedAgentOwner>> {
    let agent_bytes = agents
        .iter()
        .map(|hex_pubkey| hex_decode("agent_path entry", hex_pubkey))
        .collect::<Result<Vec<Vec<u8>>>>()?;
    let rows = sqlx::query(
        "SELECT pubkey, agent_owner_pubkey, deactivated_at FROM users \
         WHERE community_id=$1 AND pubkey = ANY($2)",
    )
    .bind(community_id.as_uuid())
    .bind(&agent_bytes)
    .fetch_all(pool)
    .await?;
    let mut found = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let pubkey: Vec<u8> = row.get("pubkey");
        let owner: Option<Vec<u8>> = row.get("agent_owner_pubkey");
        let deactivated_at: Option<DateTime<Utc>> = row.get("deactivated_at");
        found.insert(
            hex_encode(&pubkey),
            ResolvedAgentOwner {
                agent_pubkey: hex_encode(&pubkey),
                owner_pubkey: if deactivated_at.is_none() {
                    owner.as_deref().map(hex_encode)
                } else {
                    None
                },
                ownership_revision: 1,
            },
        );
    }
    Ok(agents
        .iter()
        .map(|agent| {
            found.get(agent).cloned().unwrap_or_else(|| ResolvedAgentOwner {
                agent_pubkey: agent.clone(),
                owner_pubkey: None,
                ownership_revision: 1,
            })
        })
        .collect())
}

/// Find the open (unsettled or awaiting-child) action naming `agent` as its
/// target, used for root/parent lineage proof.
#[datastore_span(name = "open_action_as_target", system = "postgresql")]
pub async fn open_action_as_target(
    pool: &PgPool,
    community_id: CommunityId,
    agent_hex: &str,
) -> Result<Option<(Uuid, String)>> {
    let agent_bytes = hex_decode("target agent", agent_hex)?;
    let row = sqlx::query(
        "SELECT r.delegation_id, r.operator_approval_event_id \
         FROM delegation_records r \
         JOIN delegation_actions a \
           ON a.community_id = r.community_id AND a.delegation_id = r.delegation_id \
         WHERE r.community_id=$1 AND r.target_agent=$2 AND r.state='approved' \
           AND (a.settled_at IS NULL OR a.outcome='delegated') \
         ORDER BY a.action_seq DESC LIMIT 1",
    )
    .bind(community_id.as_uuid())
    .bind(&agent_bytes)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| {
        let delegation_id: Uuid = r.get("delegation_id");
        let approval_event_id: Vec<u8> = r.get("operator_approval_event_id");
        (delegation_id, hex_encode(&approval_event_id))
    }))
}

/// True when `pubkey` is an active member of `channel_id` (reuses
/// [`channel_members::is_member`]).
pub async fn is_channel_member(
    pool: &PgPool,
    community_id: CommunityId,
    channel_id: Uuid,
    pubkey_hex: &str,
) -> Result<bool> {
    let pubkey = hex_decode("member pubkey", pubkey_hex)?;
    channel_members::is_member(pool, community_id, channel_id, &pubkey).await
}

/// Postgres-backed [`DelegationClaimStore`] bound to one open transaction.
///
/// The caller opens the transaction, constructs this adapter, calls the
/// trait methods, and commits (or rolls back) the transaction itself — every
/// method here runs its statements against the same `tx`.
pub struct PgDelegationClaimStore<'a> {
    tx: &'a mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    /// Full request/record fields needed to insert `delegation_records` on
    /// first claim. Populated by the caller before calling
    /// [`DelegationClaimStore::claim_and_enqueue`].
    pub pending_record: Option<PendingDelegationRecord>,
}

/// Fields the caller must supply to insert the first `delegation_records`
/// row when a claim is acquired — everything [`DelegationClaim`] itself does
/// not carry (agent path, budgets, origin, approval envelope).
#[derive(Debug, Clone)]
pub struct PendingDelegationRecord {
    /// Encrypted originating message event id (hex).
    pub origin_event_id: String,
    /// Parent approval event id (hex), if this is a nested hop.
    pub parent_approval_event_id: Option<String>,
    /// Delegating agent (hex).
    pub source_agent: String,
    /// Receiving agent (hex).
    pub target_agent: String,
    /// Ordered, same-owner agent path (hex).
    pub agent_path: Vec<String>,
    /// Maximum number of delegation edges in `agent_path`.
    pub hop_budget: u8,
    /// Maximum target-agent turns approved for the request.
    pub max_turns: u32,
    /// Optional maximum cost in integer millionths of a US dollar.
    pub cost_cap_microusd: Option<u64>,
    /// Total token budget approved for this delegation's lifetime.
    pub token_budget: u64,
    /// Verified operator signer (hex).
    pub operator_pubkey: String,
    /// The signed 43007 approval envelope, stored verbatim (identifiers only).
    pub approval_event_json: serde_json::Value,
    /// Approved immutable request hash (hex).
    pub immutable_request_hash: String,
    /// Origin channel the delegation was drafted in.
    pub origin_channel_id: Uuid,
}

impl<'a> PgDelegationClaimStore<'a> {
    /// Bind a claim-store adapter to an already-open transaction.
    pub fn new(tx: &'a mut Transaction<'static, Postgres>, community_id: CommunityId) -> Self {
        Self {
            tx,
            community_id,
            pending_record: None,
        }
    }
}

/// Bridge a sync trait method to an async future from within an already
/// async context. Requires the current-thread's runtime to be
/// `flavor = "multi_thread"` (see the module-level "Blocking discipline"
/// doc) — `block_in_place` hands this worker thread's other queued tasks to
/// a sibling worker for the duration of the block, which plain
/// `Handle::current().block_on` does not do and which is what makes this
/// safe to call from inside `spawn_blocking` (already off the async
/// scheduler) as well as, defensively, from a plain multi-thread async task.
fn block_on_current<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}

impl DelegationClaimStore for PgDelegationClaimStore<'_> {
    fn claim_and_enqueue(
        &mut self,
        claim: &DelegationClaim,
        run_id: Uuid,
        transaction_now: u64,
    ) -> std::result::Result<ClaimStoreOutcome, DelegationError> {
        block_on_current(claim_and_enqueue_tx(
            self.tx,
            self.community_id,
            claim,
            run_id,
            transaction_now,
            self.pending_record.as_ref(),
        ))
        .map_err(|_| DelegationError::AuthorityUnavailable)
    }

    fn has_open_action_as_target(
        &mut self,
        community_id: CommunityId,
        source_agent: &str,
    ) -> std::result::Result<bool, DelegationError> {
        block_on_current(has_open_action_as_target_tx(self.tx, community_id, source_agent))
            .map_err(|_| DelegationError::AuthorityUnavailable)
    }

    fn read_live_permit_fields(
        &mut self,
        community_id: CommunityId,
        parent_delegation_id: Uuid,
    ) -> std::result::Result<Option<buzz_core::delegation::LivePermitFields>, DelegationError> {
        block_on_current(read_live_permit_fields_tx(
            self.tx,
            community_id,
            parent_delegation_id,
        ))
        .map_err(|_| DelegationError::AuthorityUnavailable)
    }
}

/// Raw I/O behind [`DelegationClaimStore::has_open_action_as_target`] — never
/// constructs a `RootProof`; only `buzz-core`'s provided
/// `prove_no_open_parent` default method does that, from this answer.
async fn has_open_action_as_target_tx(
    tx: &mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    source_agent_hex: &str,
) -> Result<bool> {
    let source_agent = hex_decode("source_agent", source_agent_hex)?;
    let row: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM delegation_records r \
         JOIN delegation_actions a \
           ON a.community_id = r.community_id AND a.delegation_id = r.delegation_id \
         WHERE r.community_id=$1 AND r.target_agent=$2 AND r.state='approved' \
           AND (a.settled_at IS NULL OR a.outcome='delegated') \
         LIMIT 1",
    )
    .bind(community_id.as_uuid())
    .bind(&source_agent)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.is_some())
}

/// Raw I/O behind [`DelegationClaimStore::read_live_permit_fields`] — never
/// constructs a `DelegationExecutionPermit`; only `buzz-core`'s provided
/// `reopen_live_permit` default method does that, from this answer.
async fn read_live_permit_fields_tx(
    tx: &mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    parent_delegation_id: Uuid,
) -> Result<Option<buzz_core::delegation::LivePermitFields>> {
    let row = sqlx::query(
        "SELECT r.run_id, r.delegation_id, r.operator_approval_event_id, \
         r.immutable_request_hash, r.operator_pubkey, r.source_agent, r.target_agent, \
         r.agent_path, r.expires_at \
         FROM delegation_records r \
         JOIN delegation_actions a \
           ON a.community_id = r.community_id AND a.delegation_id = r.delegation_id \
         WHERE r.community_id=$1 AND r.delegation_id=$2 AND r.state='approved' \
           AND (a.settled_at IS NULL OR a.outcome='delegated') \
         ORDER BY a.action_seq DESC LIMIT 1",
    )
    .bind(community_id.as_uuid())
    .bind(parent_delegation_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let run_id: Uuid = row.get("run_id");
    let delegation_id: Uuid = row.get("delegation_id");
    let approval_event_id: Vec<u8> = row.get("operator_approval_event_id");
    let immutable_request_hash: Vec<u8> = row.get("immutable_request_hash");
    let operator_pubkey: Vec<u8> = row.get("operator_pubkey");
    let source_agent: Vec<u8> = row.get("source_agent");
    let target_agent: Vec<u8> = row.get("target_agent");
    let agent_path: Vec<Vec<u8>> = row.get("agent_path");
    let expires_at: DateTime<Utc> = row.get("expires_at");
    Ok(Some(buzz_core::delegation::LivePermitFields {
        community_id,
        run_id,
        delegation_id,
        approval_event_id: hex_encode(&approval_event_id),
        immutable_request_hash: hex_encode(&immutable_request_hash),
        operator_pubkey: hex_encode(&operator_pubkey),
        source_agent: hex_encode(&source_agent),
        target_agent: hex_encode(&target_agent),
        agent_path: agent_path.iter().map(|p| hex_encode(p)).collect(),
        expires_at: expires_at.timestamp().max(0) as u64,
    }))
}

async fn claim_and_enqueue_tx(
    tx: &mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    claim: &DelegationClaim,
    run_id: Uuid,
    transaction_now: u64,
    pending: Option<&PendingDelegationRecord>,
) -> Result<ClaimStoreOutcome> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT NOW()").fetch_one(&mut **tx).await?;
    if now.timestamp() as u64 >= claim.expires_at() {
        return Ok(ClaimStoreOutcome::StoreUnavailable);
    }
    let _ = transaction_now;

    let existing = sqlx::query(
        "SELECT c.delegation_id, c.approval_event_id, c.operator_pubkey, c.source_agent, \
         c.idempotency_key, c.immutable_request_hash, r.state \
         FROM delegation_claims c \
         JOIN delegation_records r ON r.community_id=c.community_id AND r.delegation_id=c.delegation_id \
         WHERE c.community_id=$1 AND ( \
           c.delegation_id=$2 OR c.approval_event_id=$3 \
           OR (c.operator_pubkey=$4 AND c.source_agent=$5 AND c.idempotency_key=$6) \
         )",
    )
    .bind(community_id.as_uuid())
    .bind(claim.delegation_id())
    .bind(hex_decode("approval_event_id", claim.approval_event_id())?)
    .bind(hex_decode("operator_pubkey", claim.operator_pubkey())?)
    .bind(hex_decode("source_agent", claim.source_agent())?)
    .bind(claim.idempotency_key())
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(row) = existing {
        let row_delegation_id: Uuid = row.get("delegation_id");
        let row_approval_event_id: Vec<u8> = row.get("approval_event_id");
        let row_operator_pubkey: Vec<u8> = row.get("operator_pubkey");
        let row_source_agent: Vec<u8> = row.get("source_agent");
        let row_idempotency_key: String = row.get("idempotency_key");
        let row_hash: Vec<u8> = row.get("immutable_request_hash");
        let row_state: String = row.get("state");

        let exact_match = row_delegation_id == claim.delegation_id()
            && hex_encode(&row_approval_event_id) == claim.approval_event_id()
            && hex_encode(&row_operator_pubkey) == claim.operator_pubkey()
            && hex_encode(&row_source_agent) == claim.source_agent()
            && row_idempotency_key == claim.idempotency_key()
            && hex_encode(&row_hash) == claim.immutable_request_hash();

        if exact_match {
            return Ok(if matches!(row_state.as_str(), "delivered" | "failed" | "expired") {
                ClaimStoreOutcome::ExactDuplicateCompleted
            } else {
                ClaimStoreOutcome::ExactDuplicatePending
            });
        }
        if row_delegation_id == claim.delegation_id() {
            return Ok(ClaimStoreOutcome::DelegationConflict);
        }
        if hex_encode(&row_approval_event_id) == claim.approval_event_id() {
            return Ok(ClaimStoreOutcome::ApprovalConflict);
        }
        return Ok(ClaimStoreOutcome::IdempotencyConflict);
    }

    let Some(pending) = pending else {
        return Ok(ClaimStoreOutcome::StoreUnavailable);
    };

    let agent_path_bytes = pending
        .agent_path
        .iter()
        .map(|hex_pk| hex_decode("agent_path entry", hex_pk))
        .collect::<Result<Vec<Vec<u8>>>>()?;
    let source_agent_bytes = hex_decode("source_agent", &pending.source_agent)?;
    let target_agent_bytes = hex_decode("target_agent", &pending.target_agent)?;
    let origin_event_id_bytes = hex_decode("origin_event_id", &pending.origin_event_id)?;
    let parent_approval_bytes = pending
        .parent_approval_event_id
        .as_deref()
        .map(|id| hex_decode("parent_approval_event_id", id))
        .transpose()?;
    let operator_pubkey_bytes = hex_decode("operator_pubkey", &pending.operator_pubkey)?;
    let approval_event_id_bytes = hex_decode("approval_event_id", claim.approval_event_id())?;
    let immutable_request_hash_bytes =
        hex_decode("immutable_request_hash", &pending.immutable_request_hash)?;
    let expires_at = DateTime::<Utc>::from_timestamp(claim.expires_at() as i64, 0)
        .ok_or_else(|| crate::error::DbError::InvalidData("invalid expires_at".into()))?;

    let insert_record = sqlx::query(
        "INSERT INTO delegation_records \
         (community_id, delegation_id, run_id, origin_event_id, parent_approval_event_id, \
          source_agent, target_agent, agent_path, hop_budget, max_turns, cost_cap_microusd, \
          token_budget, idempotency_key, expires_at, operator_pubkey, operator_approval_event_id, \
          approval_event_json, immutable_request_hash, state, remaining_turns, \
          token_budget_remaining, origin_channel_id) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,'approved',$10,$12,$19)",
    )
    .bind(community_id.as_uuid())
    .bind(claim.delegation_id())
    .bind(run_id)
    .bind(&origin_event_id_bytes)
    .bind(&parent_approval_bytes)
    .bind(&source_agent_bytes)
    .bind(&target_agent_bytes)
    .bind(&agent_path_bytes)
    .bind(i16::from(pending.hop_budget))
    .bind(pending.max_turns as i32)
    .bind(pending.cost_cap_microusd.map(|c| c as i64))
    .bind(pending.token_budget as i64)
    .bind(claim.idempotency_key())
    .bind(expires_at)
    .bind(&operator_pubkey_bytes)
    .bind(&approval_event_id_bytes)
    .bind(&pending.approval_event_json)
    .bind(&immutable_request_hash_bytes)
    .bind(pending.origin_channel_id)
    .execute(&mut **tx)
    .await;
    if insert_record.is_err() {
        return Ok(ClaimStoreOutcome::StoreUnavailable);
    }

    let insert_claim = sqlx::query(
        "INSERT INTO delegation_claims \
         (community_id, delegation_id, approval_event_id, operator_pubkey, source_agent, \
          idempotency_key, immutable_request_hash, expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(community_id.as_uuid())
    .bind(claim.delegation_id())
    .bind(&approval_event_id_bytes)
    .bind(&operator_pubkey_bytes)
    .bind(&source_agent_bytes)
    .bind(claim.idempotency_key())
    .bind(&immutable_request_hash_bytes)
    .bind(expires_at)
    .execute(&mut **tx)
    .await;
    if insert_claim.is_err() {
        return Ok(ClaimStoreOutcome::StoreUnavailable);
    }

    let owner_snapshot = serde_json::json!([]);
    let insert_action = sqlx::query(
        "INSERT INTO delegation_actions \
         (community_id, delegation_id, action_seq, approval_event_id, immutable_request_hash, \
          remaining_turns_before, committed_cost_before, token_budget_at_dispatch, owner_snapshot) \
         VALUES ($1,$2,1,$3,$4,$5,0,$6,$7)",
    )
    .bind(community_id.as_uuid())
    .bind(claim.delegation_id())
    .bind(&approval_event_id_bytes)
    .bind(&immutable_request_hash_bytes)
    .bind(pending.max_turns as i32)
    .bind(pending.token_budget as i64)
    .bind(&owner_snapshot)
    .execute(&mut **tx)
    .await;
    if insert_action.is_err() {
        return Ok(ClaimStoreOutcome::StoreUnavailable);
    }

    Ok(ClaimStoreOutcome::AcquiredAndEnqueued)
}

/// Postgres-backed [`DelegationActionStore`] bound to one open transaction.
pub struct PgDelegationActionStore<'a> {
    tx: &'a mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    run_id: Uuid,
}

impl<'a> PgDelegationActionStore<'a> {
    /// Bind an action-store adapter to an already-open transaction.
    pub fn new(
        tx: &'a mut Transaction<'static, Postgres>,
        community_id: CommunityId,
        run_id: Uuid,
    ) -> Self {
        Self {
            tx,
            community_id,
            run_id,
        }
    }
}

impl DelegationActionStore for PgDelegationActionStore<'_> {
    fn cas_and_record(
        &mut self,
        action: &ValidatedDelegationAction,
        token_budget_check: bool,
        transaction_now: u64,
    ) -> std::result::Result<ActionStoreOutcome, DelegationError> {
        block_on_current(cas_and_record_tx(
            self.tx,
            self.community_id,
            self.run_id,
            action,
            token_budget_check,
            transaction_now,
        ))
        .map_err(|_| DelegationError::AuthorityUnavailable)
    }
}

async fn cas_and_record_tx(
    tx: &mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    run_id: Uuid,
    action: &ValidatedDelegationAction,
    token_budget_check: bool,
    transaction_now: u64,
) -> Result<ActionStoreOutcome> {
    // Lock every path owner row so a concurrent ownership change cannot race
    // this CAS; the caller's `ensure_owner_snapshot` compares against what we
    // observe here.
    let mut agent_bytes = Vec::with_capacity(action.owner_snapshot().len());
    for owner in action.owner_snapshot() {
        agent_bytes.push(hex_decode("owner_snapshot agent", &owner.agent_pubkey)?);
    }
    let _locked = sqlx::query(
        "SELECT pubkey FROM users WHERE community_id=$1 AND pubkey = ANY($2) FOR SHARE",
    )
    .bind(community_id.as_uuid())
    .bind(&agent_bytes)
    .fetch_all(&mut **tx)
    .await?;

    let current = resolve_agent_owners_tx(
        tx,
        community_id,
        &action
            .owner_snapshot()
            .iter()
            .map(|o| o.agent_pubkey.clone())
            .collect::<Vec<_>>(),
    )
    .await?;
    if action.ensure_owner_snapshot(&current).is_err() {
        return Ok(ActionStoreOutcome::AuthorityConflict);
    }

    let now: DateTime<Utc> = sqlx::query_scalar("SELECT NOW()").fetch_one(&mut **tx).await?;
    let _ = transaction_now;
    if action.ensure_fresh_at(now.timestamp().max(0) as u64).is_err() {
        return Ok(ActionStoreOutcome::StateConflict);
    }

    let approval_event_id_bytes = hex_decode("approval_event_id", action.approval_event_id())?;
    let immutable_request_hash_bytes =
        hex_decode("immutable_request_hash", action.immutable_request_hash())?;

    let updated = sqlx::query(
        "UPDATE delegation_records SET remaining_turns = remaining_turns - 1, updated_at = NOW() \
         WHERE community_id=$1 AND delegation_id=$2 AND operator_approval_event_id=$3 \
           AND immutable_request_hash=$4 AND remaining_turns=$5 AND state='approved' \
           AND ($6 = FALSE OR token_budget_remaining > 0) \
         RETURNING token_budget_remaining",
    )
    .bind(community_id.as_uuid())
    .bind(action.delegation_id())
    .bind(&approval_event_id_bytes)
    .bind(&immutable_request_hash_bytes)
    .bind(action.remaining_turns_before() as i32)
    .bind(token_budget_check)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = updated else {
        // Distinguish claim-missing vs. state-stale vs. budget-exhausted.
        let existing = sqlx::query(
            "SELECT remaining_turns, token_budget_remaining FROM delegation_records \
             WHERE community_id=$1 AND delegation_id=$2 AND operator_approval_event_id=$3 \
               AND immutable_request_hash=$4 AND state='approved' FOR UPDATE",
        )
        .bind(community_id.as_uuid())
        .bind(action.delegation_id())
        .bind(&approval_event_id_bytes)
        .bind(&immutable_request_hash_bytes)
        .fetch_optional(&mut **tx)
        .await?;
        return Ok(match existing {
            None => ActionStoreOutcome::ClaimConflict,
            Some(row) => {
                let remaining_turns: i32 = row.get("remaining_turns");
                let token_budget_remaining: i64 = row.get("token_budget_remaining");
                if token_budget_remaining == 0 && remaining_turns == action.remaining_turns_before() as i32
                {
                    ActionStoreOutcome::BudgetExhausted
                } else {
                    ActionStoreOutcome::StateConflict
                }
            }
        });
    };

    let token_budget_remaining: i64 = row.get("token_budget_remaining");
    let owner_snapshot_json = owner_snapshot_to_json(action.owner_snapshot());
    let next_seq: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(action_seq), 0) + 1 FROM delegation_actions \
         WHERE community_id=$1 AND delegation_id=$2",
    )
    .bind(community_id.as_uuid())
    .bind(action.delegation_id())
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO delegation_actions \
         (community_id, delegation_id, action_seq, approval_event_id, immutable_request_hash, \
          remaining_turns_before, committed_cost_before, token_budget_at_dispatch, owner_snapshot) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(community_id.as_uuid())
    .bind(action.delegation_id())
    .bind(next_seq)
    .bind(&approval_event_id_bytes)
    .bind(&immutable_request_hash_bytes)
    .bind(action.remaining_turns_before() as i32)
    .bind(action.cost_committed_before_microusd().map(|c| c as i64).unwrap_or(0))
    .bind(token_budget_remaining)
    .bind(&owner_snapshot_json)
    .execute(&mut **tx)
    .await?;

    Ok(ActionStoreOutcome::AppliedAndRecorded {
        action_seq: next_seq as u32,
        run_id,
        remaining_turns_after: (action.remaining_turns_before() - 1).max(0),
        token_budget_remaining: token_budget_remaining.max(0) as u64,
    })
}

async fn resolve_agent_owners_tx(
    tx: &mut Transaction<'static, Postgres>,
    community_id: CommunityId,
    agents: &[String],
) -> Result<Vec<ResolvedAgentOwner>> {
    let agent_bytes = agents
        .iter()
        .map(|hex_pubkey| hex_decode("agent_path entry", hex_pubkey))
        .collect::<Result<Vec<Vec<u8>>>>()?;
    let rows = sqlx::query(
        "SELECT pubkey, agent_owner_pubkey, deactivated_at FROM users \
         WHERE community_id=$1 AND pubkey = ANY($2)",
    )
    .bind(community_id.as_uuid())
    .bind(&agent_bytes)
    .fetch_all(&mut **tx)
    .await?;
    let mut found = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let pubkey: Vec<u8> = row.get("pubkey");
        let owner: Option<Vec<u8>> = row.get("agent_owner_pubkey");
        let deactivated_at: Option<DateTime<Utc>> = row.get("deactivated_at");
        found.insert(
            hex_encode(&pubkey),
            ResolvedAgentOwner {
                agent_pubkey: hex_encode(&pubkey),
                owner_pubkey: if deactivated_at.is_none() {
                    owner.as_deref().map(hex_encode)
                } else {
                    None
                },
                ownership_revision: 1,
            },
        );
    }
    Ok(agents
        .iter()
        .map(|agent| {
            found.get(agent).cloned().unwrap_or_else(|| ResolvedAgentOwner {
                agent_pubkey: agent.clone(),
                owner_pubkey: None,
                ownership_revision: 1,
            })
        })
        .collect())
}

/// Mark an action's wake as dispatched.
#[datastore_span(name = "mark_action_dispatched", system = "postgresql")]
pub async fn mark_action_dispatched(
    pool: &PgPool,
    community_id: CommunityId,
    delegation_id: Uuid,
    action_seq: u32,
    wake_event_id: &[u8],
) -> Result<()> {
    sqlx::query(
        "UPDATE delegation_actions SET wake_event_id=$1, dispatched_at=NOW() \
         WHERE community_id=$2 AND delegation_id=$3 AND action_seq=$4",
    )
    .bind(wake_event_id)
    .bind(community_id.as_uuid())
    .bind(delegation_id)
    .bind(action_seq as i32)
    .execute(pool)
    .await?;
    Ok(())
}

/// Result of settling one action, telling the caller whether a notice is due
/// and, if this was a nested hop, where to continue the parent.
#[derive(Debug, Clone)]
pub struct DelegationSettlement {
    /// Delegation record state after this settlement.
    pub state_after: String,
    /// Remaining turns after this settlement.
    pub remaining_turns: i32,
    /// Remaining token budget after this settlement.
    pub token_budget_remaining: i64,
    /// Origin channel the delegation was drafted in.
    pub origin_channel_id: Uuid,
    /// Encrypted originating message event id.
    pub origin_event_id: Vec<u8>,
    /// Target agent that executed this action.
    pub target_agent: Vec<u8>,
    /// Parent `(delegation_id, approval_event_id)`, if this was a nested hop.
    pub parent: Option<(Uuid, Vec<u8>)>,
    /// Whether a failure/expiry notice is due and has not yet been posted.
    pub notice_due: bool,
}

/// Settle one action's outcome and transition the parent record accordingly.
#[datastore_span(name = "settle_action", system = "postgresql")]
pub async fn settle_action(
    pool: &PgPool,
    community_id: CommunityId,
    delegation_id: Uuid,
    action_seq: u32,
    outcome: &str,
    tokens_used: Option<i64>,
    detail: Option<&str>,
) -> Result<DelegationSettlement> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "UPDATE delegation_actions SET settled_at=NOW(), outcome=$1, tokens_used=$2 \
         WHERE community_id=$3 AND delegation_id=$4 AND action_seq=$5",
    )
    .bind(outcome)
    .bind(tokens_used)
    .bind(community_id.as_uuid())
    .bind(delegation_id)
    .bind(action_seq as i32)
    .execute(&mut *tx)
    .await?;

    let record = sqlx::query(
        "SELECT remaining_turns, token_budget_remaining, expires_at, origin_channel_id, \
         origin_event_id, target_agent, parent_approval_event_id, failure_notice_event_id, state \
         FROM delegation_records WHERE community_id=$1 AND delegation_id=$2 FOR UPDATE",
    )
    .bind(community_id.as_uuid())
    .bind(delegation_id)
    .fetch_one(&mut *tx)
    .await?;

    let remaining_turns: i32 = record.get("remaining_turns");
    let token_budget_before: i64 = record.get("token_budget_remaining");
    let expires_at: DateTime<Utc> = record.get("expires_at");
    let origin_channel_id: Uuid = record.get("origin_channel_id");
    let origin_event_id: Vec<u8> = record.get("origin_event_id");
    let target_agent: Vec<u8> = record.get("target_agent");
    let prior_notice: Option<Vec<u8>> = record.get("failure_notice_event_id");

    let token_budget_after = (token_budget_before - tokens_used.unwrap_or(0)).max(0);
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT NOW()").fetch_one(&mut *tx).await?;

    let (state_after, failure_detail): (&str, Option<&str>) = match outcome {
        "delivered" => ("delivered", None),
        "delegated" => ("approved", None),
        "budget_exceeded" => ("failed", Some("budget")),
        "failed" => ("failed", detail.or(Some("refused"))),
        "timeout" => {
            if remaining_turns > 0 && now < expires_at {
                ("approved", None)
            } else {
                ("failed", Some("timeout"))
            }
        }
        _ => ("failed", Some("refused")),
    };

    sqlx::query(
        "UPDATE delegation_records SET state=$1, failure_detail=$2, \
         token_budget_remaining=$3, answer_event_id = CASE WHEN $1='delivered' THEN $4 ELSE answer_event_id END, \
         updated_at=NOW() WHERE community_id=$5 AND delegation_id=$6",
    )
    .bind(state_after)
    .bind(failure_detail)
    .bind(token_budget_after)
    .bind(Option::<Vec<u8>>::None)
    .bind(community_id.as_uuid())
    .bind(delegation_id)
    .execute(&mut *tx)
    .await?;

    let parent_approval_event_id: Option<Vec<u8>> = record.get("parent_approval_event_id");
    let parent = if let Some(parent_approval_id) = parent_approval_event_id {
        let parent_row = sqlx::query(
            "SELECT delegation_id FROM delegation_records \
             WHERE community_id=$1 AND operator_approval_event_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(&parent_approval_id)
        .fetch_optional(&mut *tx)
        .await?;
        parent_row.map(|r| (r.get::<Uuid, _>("delegation_id"), parent_approval_id))
    } else {
        None
    };

    let notice_due = matches!(state_after, "failed" | "expired") && prior_notice.is_none();

    tx.commit().await?;

    Ok(DelegationSettlement {
        state_after: state_after.to_owned(),
        remaining_turns,
        token_budget_remaining: token_budget_after,
        origin_channel_id,
        origin_event_id,
        target_agent,
        parent,
        notice_due,
    })
}

/// Sweep open actions whose deadline has passed. Returns the
/// `(community_id, delegation_id, action_seq)` triples the caller must
/// settle `timeout`.
#[datastore_span(name = "expire_open_actions", system = "postgresql")]
pub async fn expire_open_actions(
    pool: &PgPool,
    deadline: DateTime<Utc>,
) -> Result<Vec<(CommunityId, Uuid, u32)>> {
    let rows = sqlx::query(
        "SELECT community_id, delegation_id, action_seq FROM delegation_actions \
         WHERE settled_at IS NULL AND created_at < $1",
    )
    .bind(deadline)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                CommunityId::from_uuid(r.get("community_id")),
                r.get::<Uuid, _>("delegation_id"),
                r.get::<i32, _>("action_seq") as u32,
            )
        })
        .collect())
}

/// Expire approved records past their signed deadline. Returns the
/// `(community_id, delegation_id)` pairs the caller must notice `expired`.
#[datastore_span(name = "expire_records", system = "postgresql")]
pub async fn expire_records(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<(CommunityId, Uuid)>> {
    let rows = sqlx::query(
        "UPDATE delegation_records SET state='expired', failure_detail='expired', updated_at=NOW() \
         WHERE state='approved' AND expires_at <= $1 \
         RETURNING community_id, delegation_id",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                CommunityId::from_uuid(r.get("community_id")),
                r.get::<Uuid, _>("delegation_id"),
            )
        })
        .collect())
}

/// Idempotently record the summary-notice event id (once per delegation).
#[datastore_span(name = "record_summary_notice", system = "postgresql")]
pub async fn record_summary_notice(
    pool: &PgPool,
    community_id: CommunityId,
    delegation_id: Uuid,
    event_id: &[u8],
) -> Result<()> {
    sqlx::query(
        "UPDATE delegation_records SET summary_event_id=$1 \
         WHERE community_id=$2 AND delegation_id=$3 AND summary_event_id IS NULL",
    )
    .bind(event_id)
    .bind(community_id.as_uuid())
    .bind(delegation_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Idempotently record the failure-notice event id (once per delegation).
#[datastore_span(name = "record_failure_notice", system = "postgresql")]
pub async fn record_failure_notice(
    pool: &PgPool,
    community_id: CommunityId,
    delegation_id: Uuid,
    event_id: &[u8],
) -> Result<()> {
    sqlx::query(
        "UPDATE delegation_records SET failure_notice_event_id=$1 \
         WHERE community_id=$2 AND delegation_id=$3 AND failure_notice_event_id IS NULL",
    )
    .bind(event_id)
    .bind(community_id.as_uuid())
    .bind(delegation_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delegations with a pending (not-yet-dispatched) next action.
#[datastore_span(name = "next_pending_action", system = "postgresql")]
pub async fn next_pending_action(pool: &PgPool, community_id: CommunityId) -> Result<Vec<(Uuid, u32)>> {
    let rows = sqlx::query(
        "SELECT delegation_id, action_seq FROM delegation_actions \
         WHERE community_id=$1 AND wake_event_id IS NULL AND settled_at IS NULL",
    )
    .bind(community_id.as_uuid())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<Uuid, _>("delegation_id"), r.get::<i32, _>("action_seq") as u32))
        .collect())
}

impl Db {
    /// See [`resolve_agent_owners`].
    #[datastore_span(name = "resolve_agent_owners", system = "postgresql")]
    pub async fn resolve_agent_owners(
        &self,
        community_id: CommunityId,
        agents: &[String],
    ) -> Result<Vec<ResolvedAgentOwner>> {
        resolve_agent_owners(&self.pool, community_id, agents).await
    }

    /// See [`open_action_as_target`].
    #[datastore_span(name = "open_action_as_target", system = "postgresql")]
    pub async fn open_action_as_target(
        &self,
        community_id: CommunityId,
        agent_hex: &str,
    ) -> Result<Option<(Uuid, String)>> {
        open_action_as_target(&self.pool, community_id, agent_hex).await
    }

    /// See [`is_channel_member`].
    #[datastore_span(name = "is_channel_member", system = "postgresql")]
    pub async fn is_delegation_target_member(
        &self,
        community_id: CommunityId,
        channel_id: Uuid,
        pubkey_hex: &str,
    ) -> Result<bool> {
        is_channel_member(&self.pool, community_id, channel_id, pubkey_hex).await
    }

    /// See [`mark_action_dispatched`].
    #[datastore_span(name = "mark_action_dispatched", system = "postgresql")]
    pub async fn mark_delegation_action_dispatched(
        &self,
        community_id: CommunityId,
        delegation_id: Uuid,
        action_seq: u32,
        wake_event_id: &[u8],
    ) -> Result<()> {
        mark_action_dispatched(&self.pool, community_id, delegation_id, action_seq, wake_event_id).await
    }

    /// See [`settle_action`].
    #[datastore_span(name = "settle_action", system = "postgresql")]
    pub async fn settle_delegation_action(
        &self,
        community_id: CommunityId,
        delegation_id: Uuid,
        action_seq: u32,
        outcome: &str,
        tokens_used: Option<i64>,
        detail: Option<&str>,
    ) -> Result<DelegationSettlement> {
        settle_action(
            &self.pool,
            community_id,
            delegation_id,
            action_seq,
            outcome,
            tokens_used,
            detail,
        )
        .await
    }

    /// See [`expire_open_actions`].
    #[datastore_span(name = "expire_open_actions", system = "postgresql")]
    pub async fn expire_open_delegation_actions(
        &self,
        deadline: DateTime<Utc>,
    ) -> Result<Vec<(CommunityId, Uuid, u32)>> {
        expire_open_actions(&self.pool, deadline).await
    }

    /// See [`expire_records`].
    #[datastore_span(name = "expire_records", system = "postgresql")]
    pub async fn expire_delegation_records(&self, now: DateTime<Utc>) -> Result<Vec<(CommunityId, Uuid)>> {
        expire_records(&self.pool, now).await
    }

    /// See [`record_summary_notice`].
    #[datastore_span(name = "record_summary_notice", system = "postgresql")]
    pub async fn record_delegation_summary_notice(
        &self,
        community_id: CommunityId,
        delegation_id: Uuid,
        event_id: &[u8],
    ) -> Result<()> {
        record_summary_notice(&self.pool, community_id, delegation_id, event_id).await
    }

    /// See [`record_failure_notice`].
    #[datastore_span(name = "record_failure_notice", system = "postgresql")]
    pub async fn record_delegation_failure_notice(
        &self,
        community_id: CommunityId,
        delegation_id: Uuid,
        event_id: &[u8],
    ) -> Result<()> {
        record_failure_notice(&self.pool, community_id, delegation_id, event_id).await
    }

    /// See [`next_pending_action`].
    #[datastore_span(name = "next_pending_action", system = "postgresql")]
    pub async fn next_pending_delegation_action(&self, community_id: CommunityId) -> Result<Vec<(Uuid, u32)>> {
        next_pending_action(&self.pool, community_id).await
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use buzz_core::delegation::{
        claim_and_enqueue, DelegationExecutionContext, DelegationRecord, DelegationRequest,
        DelegationState, ResolvedDelegationFacts, ResolvedDelegationLineage,
        ValidatedDelegationContext, DEFAULT_HOP_BUDGET,
    };
    use nostr::{EventBuilder, Keys, Kind};

    const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz"; // sadscan:disable np.postgres.1 -- local test-only credentials

    async fn setup_db() -> Db {
        let database_url =
            std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| TEST_DB_URL.into());
        let pool = PgPool::connect(&database_url)
            .await
            .expect("connect to test DB");
        crate::migration::run_migrations(&pool)
            .await
            .expect("apply migrations through 0046_delegation");
        Db::from_pool(pool)
    }

    async fn make_community(pool: &PgPool) -> CommunityId {
        let id = Uuid::new_v4();
        let host = format!("delegation-tests-{}.example", id.simple());
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(id)
            .bind(&host)
            .execute(pool)
            .await
            .expect("insert community");
        CommunityId::from_uuid(id)
    }

    async fn make_user(pool: &PgPool, community_id: CommunityId, pubkey: &[u8], owner: Option<&[u8]>) {
        sqlx::query(
            "INSERT INTO users (community_id, pubkey, agent_owner_pubkey) VALUES ($1, $2, $3) \
             ON CONFLICT (community_id, pubkey) DO NOTHING",
        )
        .bind(community_id.as_uuid())
        .bind(pubkey)
        .bind(owner)
        .execute(pool)
        .await
        .expect("insert user");
    }

    struct Fixture {
        community_id: CommunityId,
        owner_hex: String,
        target_pubkey: [u8; 32],
        source_hex: String,
        target_hex: String,
        origin_event_id: String,
        origin_channel_id: Uuid,
        request: DelegationRequest,
        record: DelegationRecord,
        context: DelegationExecutionContext,
        approval_event: nostr::Event,
        now: u64,
    }

    async fn build_fixture(pool: &PgPool, token_budget: u64, max_turns: u32) -> Fixture {
        let community_id = make_community(pool).await;
        build_fixture_with(
            pool,
            community_id,
            token_budget,
            max_turns,
            Uuid::new_v4(),
            None,
            None,
            None,
        )
        .await
    }

    /// Like [`build_fixture`], but lets a caller pin `community_id`,
    /// `delegation_id`, the operator signer, the source agent, and/or the
    /// idempotency key — needed to construct two otherwise-independent
    /// fixtures that collide on exactly one claim key within the same tenant.
    #[allow(clippy::too_many_arguments)]
    async fn build_fixture_with(
        pool: &PgPool,
        community_id: CommunityId,
        token_budget: u64,
        max_turns: u32,
        delegation_id: Uuid,
        shared_owner: Option<&Keys>,
        shared_source: Option<&Keys>,
        idempotency_key: Option<String>,
    ) -> Fixture {
        let owner_keys = shared_owner.cloned().unwrap_or_else(Keys::generate);
        let owner_hex = owner_keys.public_key().to_hex();
        let owner_pubkey: [u8; 32] = owner_keys.public_key().to_bytes();
        let source_keys = shared_source.cloned().unwrap_or_else(Keys::generate);
        let target_keys = Keys::generate();
        let source_pubkey: [u8; 32] = source_keys.public_key().to_bytes();
        let target_pubkey: [u8; 32] = target_keys.public_key().to_bytes();
        let source_hex = source_keys.public_key().to_hex();
        let target_hex = target_keys.public_key().to_hex();

        // The owner must itself be a `users` row before agent_owner_pubkey's
        // FK can reference it (no owner of its own).
        make_user(pool, community_id, &owner_pubkey, None).await;
        make_user(pool, community_id, &source_pubkey, Some(&owner_pubkey)).await;
        make_user(pool, community_id, &target_pubkey, Some(&owner_pubkey)).await;

        let origin_channel_id = Uuid::new_v4();
        let origin_event = EventBuilder::new(Kind::Custom(9), "delegate this task")
            .sign_with_keys(&source_keys)
            .expect("sign origin event");
        let origin_event_id = origin_event.id.to_hex();

        let now = 2_000_000_000u64;
        let request = DelegationRequest {
            delegation_id,
            origin_event_id: origin_event_id.clone(),
            parent_approval_event_id: None,
            source_agent: source_hex.clone(),
            target_agent: target_hex.clone(),
            agent_path: vec![source_hex.clone(), target_hex.clone()],
            hop_budget: DEFAULT_HOP_BUDGET,
            max_turns,
            cost_cap_microusd: None,
            token_budget,
            idempotency_key: idempotency_key.unwrap_or_else(|| format!("test-{}", Uuid::new_v4())),
            expires_at: now + 300,
        };
        let record = DelegationRecord::new_offered(community_id, request.clone(), now)
            .expect("valid offered record");
        let approval_event = buzz_core::delegation::build_operator_approval_event(
            &owner_keys,
            community_id,
            &record.request,
            now + 1,
        )
        .expect("valid approval event");
        let mut record = record;
        record.operator_approval_event_id = Some(approval_event.id.to_hex());
        record.state = DelegationState::Approved;
        record.updated_at = now + 1;
        let context =
            DelegationExecutionContext::from_approved_record(community_id, &record, max_turns)
                .expect("valid execution context");

        Fixture {
            community_id,
            owner_hex,
            target_pubkey,
            source_hex,
            target_hex,
            origin_event_id,
            origin_channel_id,
            request,
            record,
            context,
            approval_event,
            now,
        }
    }

    fn root_facts(fixture: &Fixture, now: u64) -> ResolvedDelegationFacts {
        ResolvedDelegationFacts {
            community_id: fixture.community_id,
            now,
            agent_owners: vec![
                ResolvedAgentOwner {
                    agent_pubkey: fixture.source_hex.clone(),
                    owner_pubkey: Some(fixture.owner_hex.clone()),
                    ownership_revision: 1,
                },
                ResolvedAgentOwner {
                    agent_pubkey: fixture.target_hex.clone(),
                    owner_pubkey: Some(fixture.owner_hex.clone()),
                    ownership_revision: 1,
                },
            ],
            lineage: ResolvedDelegationLineage::unavailable(),
        }
    }

    fn pending_record(fixture: &Fixture) -> PendingDelegationRecord {
        PendingDelegationRecord {
            origin_event_id: fixture.origin_event_id.clone(),
            parent_approval_event_id: None,
            source_agent: fixture.source_hex.clone(),
            target_agent: fixture.target_hex.clone(),
            agent_path: fixture.request.agent_path.clone(),
            hop_budget: fixture.request.hop_budget,
            max_turns: fixture.request.max_turns,
            cost_cap_microusd: fixture.request.cost_cap_microusd,
            token_budget: fixture.request.token_budget,
            operator_pubkey: fixture.owner_hex.clone(),
            approval_event_json: serde_json::json!({"id": fixture.approval_event.id.to_hex()}),
            immutable_request_hash: fixture.record.immutable_request_hash.clone(),
            origin_channel_id: fixture.origin_channel_id,
        }
    }

    /// Build a `ValidatedDelegationContext` for the fixture using the crate's
    /// own root lineage proof path, exactly as the relay would inside a
    /// two-stage validate (stateless pre-check, then transaction-scoped).
    async fn validated_context(pool: &PgPool, fixture: &Fixture) -> ValidatedDelegationContext {
        let community_id = fixture.community_id;
        let source_hex = fixture.source_hex.clone();
        let owner_hex = fixture.owner_hex.clone();
        let expires_at = fixture.request.expires_at;
        let mut tx = pool.begin().await.expect("begin tx for lineage proof");
        let mut store = PgDelegationClaimStore::new(&mut tx, community_id);
        let proof = store
            .prove_no_open_parent(community_id, &source_hex)
            .expect("prove root lineage")
            .expect("fixture source has no open action; must prove root");
        let lineage = ResolvedDelegationLineage::root_for_run(
            community_id,
            Uuid::new_v4(),
            &source_hex,
            &owner_hex,
            expires_at,
            proof,
        );
        tx.commit().await.expect("commit lineage-proof tx");

        let mut facts = root_facts(fixture, fixture.now + 1);
        facts.lineage = lineage;
        buzz_core::delegation::validate_for_claim(
            &fixture.record,
            Some(&fixture.context),
            Some(&fixture.approval_event),
            &facts,
        )
        .expect("fixture must validate for claim")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — delegation three-key atomic claim + outbox"]
    async fn claim_three_keys_atomic_and_outbox() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 50_000, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;
        let run_id = Uuid::new_v4();

        let mut tx = db.pool.begin().await.expect("begin claim tx");
        let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
        store.pending_record = Some(pending_record(&fixture));
        let disposition =
            claim_and_enqueue(&mut store, &validated, run_id, fixture.now + 1).expect("claim");
        tx.commit().await.expect("commit claim tx");

        assert!(
            matches!(disposition, buzz_core::delegation::ClaimDisposition::Permit(_)),
            "first claim must acquire a permit"
        );

        let claim_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delegation_claims WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("count claims");
        assert_eq!(claim_count, 1, "exactly one claim row for the three keys");

        let action_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delegation_actions WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("count actions");
        assert_eq!(
            action_count, 1,
            "the first delegation_actions row must be committed in the same transaction as the claim"
        );

        let record_state: String = sqlx::query_scalar(
            "SELECT state FROM delegation_records WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("read record state");
        assert_eq!(record_state, "approved");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — exact duplicate claim collapses to a suppressed outcome"]
    async fn claim_exact_duplicate_collapses_pending_and_completed() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 50_000, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;
        let run_id = Uuid::new_v4();

        {
            let mut tx = db.pool.begin().await.expect("begin first claim tx");
            let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
            store.pending_record = Some(pending_record(&fixture));
            claim_and_enqueue(&mut store, &validated, run_id, fixture.now + 1)
                .expect("first claim acquires");
            tx.commit().await.expect("commit first claim");
        }

        // Retry with the exact same claim identity while the action is still
        // pending: must collapse to ExactDuplicatePending, no new rows.
        let mut tx = db.pool.begin().await.expect("begin retry tx");
        let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
        store.pending_record = Some(pending_record(&fixture));
        let disposition = claim_and_enqueue(&mut store, &validated, Uuid::new_v4(), fixture.now + 2)
            .expect("retry collapses");
        tx.commit().await.expect("commit retry tx (no-op)");
        assert!(
            matches!(
                disposition,
                buzz_core::delegation::ClaimDisposition::DuplicateSuppressed(_)
            ),
            "exact duplicate while pending must suppress, not re-execute"
        );

        let action_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delegation_actions WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("count actions");
        assert_eq!(action_count, 1, "duplicate must not create a second action row");

        // Settle the record delivered, then retry again: must collapse to
        // ExactDuplicateCompleted.
        db.settle_delegation_action(
            fixture.community_id,
            fixture.request.delegation_id,
            1,
            "delivered",
            Some(100),
            None,
        )
        .await
        .expect("settle delivered");

        let mut tx = db.pool.begin().await.expect("begin post-settle retry tx");
        let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
        store.pending_record = Some(pending_record(&fixture));
        let disposition =
            claim_and_enqueue(&mut store, &validated, Uuid::new_v4(), fixture.now + 3)
                .expect("post-settle retry collapses");
        tx.commit().await.expect("commit post-settle retry (no-op)");
        assert!(matches!(
            disposition,
            buzz_core::delegation::ClaimDisposition::DuplicateSuppressed(_)
        ));
    }

    /// Claim `fixture` and commit; panics on anything but a fresh permit.
    async fn claim_committed(pool: &PgPool, fixture: &Fixture) {
        let validated = validated_context(pool, fixture).await;
        let mut tx = pool.begin().await.expect("begin claim tx");
        let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
        store.pending_record = Some(pending_record(fixture));
        let disposition =
            claim_and_enqueue(&mut store, &validated, Uuid::new_v4(), fixture.now + 1)
                .expect("claim acquires");
        tx.commit().await.expect("commit claim");
        assert!(matches!(
            disposition,
            buzz_core::delegation::ClaimDisposition::Permit(_)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — each of the three claim keys conflicts independently"]
    async fn claim_conflict_on_each_key() {
        let db = setup_db().await;

        // delegation_id conflict: same tenant + delegation_id, everything
        // else (approval signer, agents, idempotency key) different.
        {
            let community_id = make_community(&db.pool).await;
            let shared_delegation_id = Uuid::new_v4();
            let fixture_a = build_fixture_with(
                &db.pool,
                community_id,
                50_000,
                8,
                shared_delegation_id,
                None,
                None,
                None,
            )
            .await;
            claim_committed(&db.pool, &fixture_a).await;

            let fixture_b = build_fixture_with(
                &db.pool,
                community_id,
                50_000,
                8,
                shared_delegation_id,
                None,
                None,
                None,
            )
            .await;
            let validated_b = validated_context(&db.pool, &fixture_b).await;
            let mut tx = db.pool.begin().await.expect("begin tx b");
            let mut store = PgDelegationClaimStore::new(&mut tx, community_id);
            store.pending_record = Some(pending_record(&fixture_b));
            let result =
                claim_and_enqueue(&mut store, &validated_b, Uuid::new_v4(), fixture_b.now + 1);
            tx.commit().await.expect("commit b (conflict recorded, no crash)");
            assert!(
                matches!(result, Err(DelegationError::ApprovalReplay)),
                "delegation_id collision with a different approval must be a replay refusal"
            );
        }

        // approval_event_id conflict is exercised implicitly: an approval
        // event id is a hash of the signed envelope, which embeds the
        // delegation id, so two honestly-signed approvals cannot collide on
        // approval_event_id without also colliding on delegation_id (covered
        // above) or being the exact same approval (covered by the
        // exact-duplicate test).

        // idempotency-key conflict: same tenant + operator + source agent +
        // idempotency key, but a different delegation_id (and therefore a
        // different approval and origin event).
        {
            let community_id = make_community(&db.pool).await;
            let owner_keys = Keys::generate();
            let source_keys = Keys::generate();
            let idempotency_key = format!("shared-idem-{}", Uuid::new_v4());

            let fixture_a = build_fixture_with(
                &db.pool,
                community_id,
                50_000,
                8,
                Uuid::new_v4(),
                Some(&owner_keys),
                Some(&source_keys),
                Some(idempotency_key.clone()),
            )
            .await;
            claim_committed(&db.pool, &fixture_a).await;

            let fixture_b = build_fixture_with(
                &db.pool,
                community_id,
                50_000,
                8,
                Uuid::new_v4(),
                Some(&owner_keys),
                Some(&source_keys),
                Some(idempotency_key),
            )
            .await;
            let validated_b = validated_context(&db.pool, &fixture_b).await;
            let mut tx = db.pool.begin().await.expect("begin tx idem-conflict");
            let mut store = PgDelegationClaimStore::new(&mut tx, community_id);
            store.pending_record = Some(pending_record(&fixture_b));
            let result =
                claim_and_enqueue(&mut store, &validated_b, Uuid::new_v4(), fixture_b.now + 1);
            tx.commit()
                .await
                .expect("commit idem-conflict (conflict recorded, no crash)");
            assert!(
                matches!(result, Err(DelegationError::ApprovalReplay)),
                "reusing (operator, source_agent, idempotency_key) with a different delegation_id \
                 must be a replay refusal"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — the claim transaction rechecks its own clock before inserting"]
    async fn claim_rechecks_time_before_insert() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 50_000, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;

        // transaction_now at/after the claim's expiry must refuse before any insert.
        let mut tx = db.pool.begin().await.expect("begin tx");
        let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
        store.pending_record = Some(pending_record(&fixture));
        let result = claim_and_enqueue(
            &mut store,
            &validated,
            Uuid::new_v4(),
            fixture.request.expires_at,
        );
        tx.commit().await.expect("commit (no rows expected)");
        assert!(
            matches!(result, Err(DelegationError::Expired)),
            "claim at/after expiry must be refused before any row is inserted"
        );

        let row_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delegation_records WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("count records");
        assert_eq!(row_count, 0, "no record row when the freshness recheck fails");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — per-action CAS decrements exactly one turn and refuses stale state"]
    async fn cas_decrements_exact_turn_and_refuses_stale() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 50_000, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;
        let run_id = Uuid::new_v4();
        {
            let mut tx = db.pool.begin().await.expect("begin claim tx");
            let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
            store.pending_record = Some(pending_record(&fixture));
            claim_and_enqueue(&mut store, &validated, run_id, fixture.now + 1).expect("claim");
            tx.commit().await.expect("commit claim");
        }

        let action_facts = buzz_core::delegation::ResolvedDelegationActionFacts {
            community_id: fixture.community_id,
            now: fixture.now + 2,
            agent_owners: root_facts(&fixture, fixture.now + 2).agent_owners,
            remaining_turns: 8,
            cost_committed_microusd: None,
            action_cost_reservation_microusd: None,
        };
        let action = buzz_core::delegation::validate_next_action(&validated, &action_facts)
            .expect("validate first action");

        let mut tx = db.pool.begin().await.expect("begin cas tx");
        let mut store = PgDelegationActionStore::new(&mut tx, fixture.community_id, run_id);
        let permit = cas_action_for_test(&mut store, &action, fixture.now + 2).expect("cas applies");
        tx.commit().await.expect("commit cas");
        assert_eq!(permit.remaining_turns_after(), 7);

        let remaining_turns: i32 = sqlx::query_scalar(
            "SELECT remaining_turns FROM delegation_records WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("read remaining_turns");
        assert_eq!(remaining_turns, 7, "exactly one turn decremented");

        // Reusing the same (now-stale) expected turn count must be refused.
        let mut tx = db.pool.begin().await.expect("begin stale cas tx");
        let mut store = PgDelegationActionStore::new(&mut tx, fixture.community_id, run_id);
        let stale_result = cas_action_for_test(&mut store, &action, fixture.now + 3);
        tx.commit().await.expect("commit stale (no-op)");
        assert!(
            matches!(stale_result, Err(DelegationError::ActionConflict)),
            "reusing a stale expected turn count must be refused as a conflict"
        );
    }

    fn cas_action_for_test(
        store: &mut PgDelegationActionStore<'_>,
        action: &ValidatedDelegationAction,
        transaction_now: u64,
    ) -> std::result::Result<buzz_core::delegation::DelegationActionPermit, DelegationError> {
        buzz_core::delegation::cas_action(store, action, transaction_now)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — CAS refuses when an owner row was deactivated before the action"]
    async fn cas_refuses_after_owner_row_deactivated() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 50_000, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;
        let run_id = Uuid::new_v4();
        {
            let mut tx = db.pool.begin().await.expect("begin claim tx");
            let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
            store.pending_record = Some(pending_record(&fixture));
            claim_and_enqueue(&mut store, &validated, run_id, fixture.now + 1).expect("claim");
            tx.commit().await.expect("commit claim");
        }

        // Deactivate the target's owner-visible row between claim and action.
        sqlx::query("UPDATE users SET deactivated_at = NOW() WHERE community_id=$1 AND pubkey=$2")
            .bind(fixture.community_id.as_uuid())
            .bind(fixture.target_pubkey.as_slice())
            .execute(&db.pool)
            .await
            .expect("deactivate target");

        let action_facts = buzz_core::delegation::ResolvedDelegationActionFacts {
            community_id: fixture.community_id,
            now: fixture.now + 2,
            agent_owners: root_facts(&fixture, fixture.now + 2).agent_owners,
            remaining_turns: 8,
            cost_committed_microusd: None,
            action_cost_reservation_microusd: None,
        };
        let action = buzz_core::delegation::validate_next_action(&validated, &action_facts)
            .expect("validate action against the pre-deactivation snapshot");

        let mut tx = db.pool.begin().await.expect("begin cas tx");
        let mut store = PgDelegationActionStore::new(&mut tx, fixture.community_id, run_id);
        let result = cas_action_for_test(&mut store, &action, fixture.now + 2);
        tx.commit().await.expect("commit (conflict recorded)");
        assert!(
            matches!(result, Err(DelegationError::ActionConflict)),
            "a deactivated owner row must fail the CAS as an authority conflict"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — CAS refuses when the durable token budget is already zero"]
    async fn cas_refuses_zero_budget() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 100, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;
        let run_id = Uuid::new_v4();
        {
            let mut tx = db.pool.begin().await.expect("begin claim tx");
            let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
            store.pending_record = Some(pending_record(&fixture));
            claim_and_enqueue(&mut store, &validated, run_id, fixture.now + 1).expect("claim");
            tx.commit().await.expect("commit claim");
        }

        // Drain the budget to zero directly (simulating a prior turn that
        // consumed it all).
        sqlx::query(
            "UPDATE delegation_records SET token_budget_remaining = 0 \
             WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .execute(&db.pool)
        .await
        .expect("drain budget");

        let action_facts = buzz_core::delegation::ResolvedDelegationActionFacts {
            community_id: fixture.community_id,
            now: fixture.now + 2,
            agent_owners: root_facts(&fixture, fixture.now + 2).agent_owners,
            remaining_turns: 8,
            cost_committed_microusd: None,
            action_cost_reservation_microusd: None,
        };
        let action = buzz_core::delegation::validate_next_action(&validated, &action_facts)
            .expect("validate action");

        let mut tx = db.pool.begin().await.expect("begin cas tx");
        let mut store = PgDelegationActionStore::new(&mut tx, fixture.community_id, run_id);
        let result = cas_action_for_test(&mut store, &action, fixture.now + 2);
        tx.commit().await.expect("commit (budget-exhausted recorded)");
        assert!(
            matches!(result, Err(DelegationError::BudgetExhausted)),
            "a zero durable token budget must refuse the action as budget_exhausted"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — a nested child's claim reserves budget from the parent and refuses overdraw"]
    async fn child_claim_reserves_parent_budget_and_refuses_overdraw() {
        let db = setup_db().await;
        let parent = build_fixture(&db.pool, 10_000, 8).await;
        let parent_validated = validated_context(&db.pool, &parent).await;
        let parent_run_id = Uuid::new_v4();
        {
            let mut tx = db.pool.begin().await.expect("begin parent claim tx");
            let mut store = PgDelegationClaimStore::new(&mut tx, parent.community_id);
            store.pending_record = Some(pending_record(&parent));
            claim_and_enqueue(&mut store, &parent_validated, parent_run_id, parent.now + 1)
                .expect("claim parent");
            tx.commit().await.expect("commit parent claim");
        }

        // A child claim overdrawing the parent's remaining budget must be
        // refused and the parent's remaining budget must be unchanged.
        let overdraw_child_budget = 20_000u64;
        let mut tx = db.pool.begin().await.expect("begin overdraw tx");
        let overdraw_result: Result<()> = (async {
            let updated = sqlx::query(
                "UPDATE delegation_records SET token_budget_remaining = token_budget_remaining - $1 \
                 WHERE community_id=$2 AND delegation_id=$3 AND token_budget_remaining >= $1",
            )
            .bind(overdraw_child_budget as i64)
            .bind(parent.community_id.as_uuid())
            .bind(parent.request.delegation_id)
            .execute(&mut *tx)
            .await?;
            assert_eq!(
                updated.rows_affected(),
                0,
                "overdrawing the parent's remaining budget must affect zero rows"
            );
            Ok(())
        })
        .await;
        tx.rollback().await.expect("rollback overdraw attempt");
        overdraw_result.expect("overdraw check ran cleanly");

        let remaining_after_overdraw: i64 = sqlx::query_scalar(
            "SELECT token_budget_remaining FROM delegation_records WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(parent.community_id.as_uuid())
        .bind(parent.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("read parent remaining budget");
        assert_eq!(
            remaining_after_overdraw, 10_000,
            "parent's remaining budget must be unchanged after a refused overdraw"
        );

        // A within-budget child reservation succeeds and decrements the parent.
        let child_budget = 4_000i64;
        let mut tx = db.pool.begin().await.expect("begin reserve tx");
        let updated = sqlx::query(
            "UPDATE delegation_records SET token_budget_remaining = token_budget_remaining - $1 \
             WHERE community_id=$2 AND delegation_id=$3 AND token_budget_remaining >= $1",
        )
        .bind(child_budget)
        .bind(parent.community_id.as_uuid())
        .bind(parent.request.delegation_id)
        .execute(&mut *tx)
        .await
        .expect("reserve child budget");
        assert_eq!(updated.rows_affected(), 1);
        tx.commit().await.expect("commit reservation");

        let remaining_after_reserve: i64 = sqlx::query_scalar(
            "SELECT token_budget_remaining FROM delegation_records WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(parent.community_id.as_uuid())
        .bind(parent.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("read parent remaining budget after reserve");
        assert_eq!(remaining_after_reserve, 6_000);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres — settlement subtracts token usage with saturating arithmetic"]
    async fn settle_subtracts_tokens_saturating() {
        let db = setup_db().await;
        let fixture = build_fixture(&db.pool, 1_000, 8).await;
        let validated = validated_context(&db.pool, &fixture).await;
        let run_id = Uuid::new_v4();
        {
            let mut tx = db.pool.begin().await.expect("begin claim tx");
            let mut store = PgDelegationClaimStore::new(&mut tx, fixture.community_id);
            store.pending_record = Some(pending_record(&fixture));
            claim_and_enqueue(&mut store, &validated, run_id, fixture.now + 1).expect("claim");
            tx.commit().await.expect("commit claim");
        }

        // Settling with usage far exceeding the remaining budget must saturate
        // at zero, never go negative (the CHECK constraint would reject a
        // negative value outright, but this proves the application-level
        // arithmetic never attempts it).
        let settlement = db
            .settle_delegation_action(
                fixture.community_id,
                fixture.request.delegation_id,
                1,
                "budget_exceeded",
                Some(5_000),
                None,
            )
            .await
            .expect("settle over-budget outcome");
        assert_eq!(settlement.token_budget_remaining, 0, "usage exceeding remaining must saturate at zero");
        assert_eq!(settlement.state_after, "failed");

        let stored_remaining: i64 = sqlx::query_scalar(
            "SELECT token_budget_remaining FROM delegation_records WHERE community_id=$1 AND delegation_id=$2",
        )
        .bind(fixture.community_id.as_uuid())
        .bind(fixture.request.delegation_id)
        .fetch_one(&db.pool)
        .await
        .expect("read stored remaining budget");
        assert_eq!(stored_remaining, 0);
    }
}
