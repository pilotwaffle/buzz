//! Operator-approved agent delegation (kind 43007, `BUZZ_DELEGATION`).
//!
//! [`handle_approval_event`] is the shared WS/HTTP ingest branch for a
//! delegation approval. It never inserts the 43007 event into `events`; it
//! either claims a fresh delegation durably (posting the summary notice and
//! dispatching the first action) or returns one of the two enumeration-safe
//! refusal strings (I-13).

use std::sync::Arc;

use nostr::{Event, JsonUtil};
use uuid::Uuid;

use buzz_core::delegation::{
    validate_for_claim, ClaimDisposition, DelegationApproval, DelegationClaimStore,
    DelegationError, DelegationRecord, DelegationRequest, ResolvedDelegationFacts,
    ResolvedDelegationLineage,
};
use buzz_core::tenant::TenantContext;
use buzz_db::delegation::{PendingDelegationRecord, PgDelegationClaimStore};

use crate::handlers::ingest::{IngestAuth, IngestError, IngestResult};
use crate::state::AppState;

pub mod dispatch;
pub mod notices;
pub mod sweeper;

#[cfg(test)]
mod tests;

/// Enumeration-safe public refusal for an unresolvable/foreign/non-member target.
pub const TARGET_UNAVAILABLE: &str = "blocked: delegation target unavailable";
/// Enumeration-safe public refusal for every other failure.
pub const DELEGATION_REFUSED: &str = "blocked: delegation refused";

/// Extract the single `buzz-delegation` fenced code block from `content`.
///
/// Returns `Err` when there is no such block, or more than one (a second
/// block is refused rather than silently taking the first — an agent or
/// attacker must not be able to smuggle a second, differently-shaped block
/// past a reviewer skimming only the first).
fn extract_delegation_block(content: &str) -> Result<&str, &'static str> {
    const FENCE_OPEN: &str = "```buzz-delegation";
    const FENCE_CLOSE: &str = "```";

    let mut remaining = content;
    let mut found: Option<&str> = None;
    loop {
        let Some(start) = remaining.find(FENCE_OPEN) else {
            break;
        };
        let after_open = &remaining[start + FENCE_OPEN.len()..];
        // The line the fence opens on must end right after the info string
        // (optionally followed by a newline) — not `buzz-delegation-foo`.
        let after_open = match after_open.strip_prefix('\n') {
            Some(rest) => rest,
            None if after_open.is_empty() => after_open,
            None => {
                // Not a clean fence boundary; keep scanning past this match.
                remaining = &remaining[start + FENCE_OPEN.len()..];
                continue;
            }
        };
        let Some(close_rel) = after_open.find(FENCE_CLOSE) else {
            return Err("unterminated buzz-delegation block");
        };
        let block = &after_open[..close_rel];
        if found.is_some() {
            return Err("more than one buzz-delegation block");
        }
        found = Some(block);
        remaining = &after_open[close_rel + FENCE_CLOSE.len()..];
    }
    found.ok_or("no buzz-delegation block")
}

/// Strict shape check for the five approval tags: exactly one value each,
/// no duplicates, no extras beyond the five NIP-DG names.
struct ApprovalTags {
    delegation_id: String,
    origin_event_id: String,
    target_agent: String,
    immutable_request_hash: String,
    expires_at: String,
}

fn validate_approval_tags(event: &Event) -> Result<ApprovalTags, &'static str> {
    let mut d = None;
    let mut e = None;
    let mut p = None;
    let mut request = None;
    let mut expiration = None;
    for tag in event.tags.iter() {
        let slice = tag.as_slice();
        let (Some(name), Some(value)) = (slice.first(), slice.get(1)) else {
            continue;
        };
        let name = name.as_str();
        if slice.len() != 2 {
            // Every NIP-DG approval tag carries exactly one value.
            if matches!(name, "d" | "e" | "p" | "request" | "expiration") {
                return Err("approval tag has extra elements");
            }
            continue;
        }
        match name {
            "d" if d.is_none() => d = Some(value.as_str().to_owned()),
            "d" => return Err("duplicate d tag"),
            "e" if e.is_none() => e = Some(value.as_str().to_owned()),
            "e" => return Err("duplicate e tag"),
            "p" if p.is_none() => p = Some(value.as_str().to_owned()),
            "p" => return Err("duplicate p tag"),
            "request" if request.is_none() => request = Some(value.as_str().to_owned()),
            "request" => return Err("duplicate request tag"),
            "expiration" if expiration.is_none() => expiration = Some(value.as_str().to_owned()),
            "expiration" => return Err("duplicate expiration tag"),
            _ => {}
        }
    }
    Ok(ApprovalTags {
        delegation_id: d.ok_or("missing d tag")?,
        origin_event_id: e.ok_or("missing e tag")?,
        target_agent: p.ok_or("missing p tag")?,
        immutable_request_hash: request.ok_or("missing request tag")?,
        expires_at: expiration.ok_or("missing expiration tag")?,
    })
}

/// Handle one inbound kind-43007 approval event from the shared WS/HTTP
/// ingest seam. Never inserts the event into `events`.
pub async fn handle_approval_event(
    state: &Arc<AppState>,
    tenant: &TenantContext,
    event: Event,
    auth: &IngestAuth,
) -> Result<IngestResult, IngestError> {
    let event_id_hex = event.id.to_hex();
    let community_id = tenant.community();

    macro_rules! refuse {
        ($reason:expr, $public:expr) => {{
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = tracing::field::Empty,
                reason = $reason,
                "delegation_refused"
            );
            return Err(IngestError::Rejected($public.to_string()));
        }};
    }

    // a. Strict envelope shape.
    if event.pubkey != *auth.pubkey() {
        refuse!("signer_mismatch", DELEGATION_REFUSED);
    }
    let tags = match validate_approval_tags(&event) {
        Ok(tags) => tags,
        Err(_) => refuse!("invalid_envelope", DELEGATION_REFUSED),
    };
    let approval: DelegationApproval = match serde_json::from_str(&event.content) {
        Ok(approval) => approval,
        Err(_) => refuse!("invalid_envelope", DELEGATION_REFUSED),
    };
    if approval.expires_at.to_string() != tags.expires_at {
        refuse!("invalid_envelope", DELEGATION_REFUSED);
    }
    if approval.delegation_id.to_string() != tags.delegation_id {
        refuse!("invalid_envelope", DELEGATION_REFUSED);
    }

    // b. Origin binding.
    let origin = match state
        .db
        .get_event_by_id(community_id, &hex_decode_or_refuse(&tags.origin_event_id)?)
        .await
    {
        Ok(Some(origin)) => origin,
        Ok(None) => refuse!("origin_not_found", DELEGATION_REFUSED),
        Err(_) => refuse!("store_unavailable", DELEGATION_REFUSED),
    };
    if origin.event.kind.as_u16() as u32 != buzz_core::kind::KIND_STREAM_MESSAGE {
        refuse!("origin_wrong_kind", DELEGATION_REFUSED);
    }
    let Some(origin_channel_id) = origin.channel_id else {
        refuse!("origin_no_channel", DELEGATION_REFUSED);
    };
    // The drafting agent cannot know the id of the message it is about to
    // post, so the block it signs omits `origin_event_id` (build_spec.md
    // 6.6, deviation D-4) — parse the as-drafted shape and fill in the real
    // id here, before anything downstream (the hash check included) ever
    // sees the request. A draft that names a *different* id than the
    // origin's own is refused, not silently overwritten.
    let draft = match extract_delegation_block(&origin.event.content) {
        Ok(block) => match serde_json::from_str::<buzz_core::delegation::DelegationRequestDraft>(block) {
            Ok(draft) => draft,
            Err(_) => refuse!("origin_block_invalid", DELEGATION_REFUSED),
        },
        Err(_) => refuse!("origin_block_invalid", DELEGATION_REFUSED),
    };
    let origin_event_id_hex = origin.event.id.to_hex();
    if draft
        .origin_event_id
        .as_deref()
        .is_some_and(|id| id != origin_event_id_hex)
    {
        refuse!("origin_binding_mismatch", DELEGATION_REFUSED);
    }
    let request = draft.into_request(origin_event_id_hex);
    if origin.event.pubkey.to_hex() != request.source_agent {
        refuse!("origin_binding_mismatch", DELEGATION_REFUSED);
    }

    // c. Record shape + hash binding.
    let record = match DelegationRecord::new_offered(
        community_id,
        request.clone(),
        origin.event.created_at.as_secs(),
    ) {
        Ok(record) => record,
        Err(_) => refuse!("invalid_request", DELEGATION_REFUSED),
    };
    if record.immutable_request_hash != approval.immutable_request_hash
        || record.immutable_request_hash != tags.immutable_request_hash
    {
        refuse!("hash_mismatch", DELEGATION_REFUSED);
    }
    if tags.target_agent != request.target_agent {
        refuse!("target_tag_mismatch", DELEGATION_REFUSED);
    }
    let mut record = record;
    record.operator_approval_event_id = Some(event_id_hex.clone());
    record.state = buzz_core::delegation::DelegationState::Approved;
    record.updated_at = chrono::Utc::now().timestamp().max(0) as u64;
    let context = match buzz_core::delegation::DelegationExecutionContext::from_approved_record(
        community_id,
        &record,
        request.max_turns,
    ) {
        Ok(context) => context,
        Err(_) => refuse!("invalid_context", DELEGATION_REFUSED),
    };

    // d. Lineage: root vs. nested-hop binding, proven under a short read-only
    // transaction (re-proven under the claim transaction's own snapshot in h).
    let open = match state
        .db
        .open_action_as_target(community_id, &request.source_agent)
        .await
    {
        Ok(open) => open,
        Err(_) => refuse!("store_unavailable", DELEGATION_REFUSED),
    };
    match (&request.parent_approval_event_id, &open) {
        (None, Some(_)) => refuse!("parent_binding_mismatch", DELEGATION_REFUSED),
        (Some(parent_id), open) => {
            let matches_open = open
                .as_ref()
                .is_some_and(|(_, open_parent_id)| open_parent_id == parent_id);
            if !matches_open {
                refuse!("parent_binding_mismatch", DELEGATION_REFUSED);
            }
        }
        (None, None) => {}
    }
    let operator_pubkey = event.pubkey.to_hex();
    let lineage = match resolve_lineage(state, community_id, &request, &operator_pubkey, &open).await
    {
        Ok(lineage) => lineage,
        Err(_) => refuse!("parent_binding_mismatch", DELEGATION_REFUSED),
    };

    // e. Owners.
    let agent_owners = match state.db.resolve_agent_owners(community_id, &request.agent_path).await
    {
        Ok(owners) => owners,
        Err(_) => refuse!("store_unavailable", DELEGATION_REFUSED),
    };
    let facts = ResolvedDelegationFacts {
        community_id,
        now: chrono::Utc::now().timestamp().max(0) as u64,
        agent_owners,
        lineage,
    };

    // f. Membership: the target must be a member of the origin channel.
    let target_member = match state
        .is_member_cached(
            community_id,
            origin_channel_id,
            &hex_decode_or_refuse(&request.target_agent)?,
        )
        .await
    {
        Ok(is_member) => is_member,
        Err(_) => refuse!("store_unavailable", DELEGATION_REFUSED),
    };
    if !target_member {
        refuse!("owner_mismatch", TARGET_UNAVAILABLE);
    }

    // g. Stateless pre-validation (spawn_blocking; Schnorr verification is CPU-bound).
    let validated = {
        let record = record.clone();
        let context = context.clone();
        let event = event.clone();
        match tokio::task::spawn_blocking(move || {
            validate_for_claim(&record, Some(&context), Some(&event), &facts)
        })
        .await
        {
            Ok(Ok(validated)) => validated,
            Ok(Err(DelegationError::OwnerMismatch | DelegationError::TenantMismatch)) => {
                refuse!("owner_mismatch", TARGET_UNAVAILABLE)
            }
            Ok(Err(e)) => refuse!(e.code(), DELEGATION_REFUSED),
            Err(_) => refuse!("store_unavailable", DELEGATION_REFUSED),
        }
    };

    // h. Durable atomic claim.
    let run_id = Uuid::now_v7();
    let mut tx = match state.db.begin_event_write_transaction().await {
        Ok(tx) => tx,
        Err(_) => refuse!("store_unavailable", DELEGATION_REFUSED),
    };
    let tx_now = chrono::Utc::now().timestamp().max(0) as u64;
    let disposition = {
        let mut store = PgDelegationClaimStore::new(&mut tx, community_id);
        store.pending_record = Some(PendingDelegationRecord {
            origin_event_id: request.origin_event_id.clone(),
            parent_approval_event_id: request.parent_approval_event_id.clone(),
            source_agent: request.source_agent.clone(),
            target_agent: request.target_agent.clone(),
            agent_path: request.agent_path.clone(),
            hop_budget: request.hop_budget,
            max_turns: request.max_turns,
            cost_cap_microusd: request.cost_cap_microusd,
            token_budget: request.token_budget,
            operator_pubkey: operator_pubkey.clone(),
            approval_event_json: serde_json::from_str(&event.as_json())
                .unwrap_or_else(|_| serde_json::json!({"id": event_id_hex})),
            immutable_request_hash: record.immutable_request_hash.clone(),
            origin_channel_id,
            created_at: record.created_at,
        });
        buzz_core::delegation::claim_and_enqueue(&mut store, &validated, run_id, tx_now)
    };
    let disposition = match disposition {
        Ok(disposition) => disposition,
        Err(DelegationError::ApprovalReplay) => {
            let _ = tx.rollback().await;
            refuse!("approval_replay", DELEGATION_REFUSED)
        }
        Err(_) => {
            let _ = tx.rollback().await;
            refuse!("store_unavailable", DELEGATION_REFUSED)
        }
    };
    if tx.commit().await.is_err() {
        refuse!("store_unavailable", DELEGATION_REFUSED);
    }

    match disposition {
        ClaimDisposition::DuplicateSuppressed(_) => {
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %request.delegation_id,
                reason = "duplicate_suppressed",
                "delegation_approved"
            );
            return Ok(IngestResult {
                event_id: event_id_hex,
                accepted: true,
                message: String::new(),
            });
        }
        ClaimDisposition::Permit(_) => {
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %request.delegation_id,
                "delegation_approved"
            );
            metrics::counter!("buzz_delegation_outcomes_total", "outcome" => "approved")
                .increment(1);
        }
    }

    // i. Summary notice, then dispatch the first action.
    if let Ok(notice_id) = notices::post_summary_notice(
        state,
        tenant,
        &request,
        origin_channel_id,
        origin.event.id.to_hex(),
    )
    .await
    {
        let _ = state
            .db
            .record_delegation_summary_notice(community_id, request.delegation_id, &notice_id)
            .await;
    }

    dispatch::dispatch_next(state, community_id, request.delegation_id, None).await;

    Ok(IngestResult {
        event_id: event_id_hex,
        accepted: true,
        message: String::new(),
    })
}

fn hex_decode_or_refuse(value: &str) -> Result<Vec<u8>, IngestError> {
    hex::decode(value).map_err(|_| IngestError::Rejected(DELEGATION_REFUSED.to_string()))
}

/// Resolve lineage per 3.4: a read-only proof/reopen on a short transaction,
/// stateless pre-check only — re-proven under the claim transaction's own
/// snapshot inside `handle_approval_event`'s step h via `claim_and_enqueue`
/// (which itself does not re-derive lineage; the durable claim's atomicity
/// is what actually enforces root-ness/parent-liveness at commit time — see
/// `PgDelegationClaimStore::claim_and_enqueue_tx`, which re-checks the
/// parent's open-action-as-target existence implicitly by requiring the
/// exact claim keys and, for a child, the budget-reservation UPDATE against
/// the parent's live row).
pub(crate) async fn resolve_lineage(
    state: &Arc<AppState>,
    community_id: buzz_core::CommunityId,
    request: &DelegationRequest,
    operator_pubkey: &str,
    open: &Option<(Uuid, String)>,
) -> Result<ResolvedDelegationLineage, DelegationError> {
    let mut tx = state
        .db
        .begin_event_write_transaction()
        .await
        .map_err(|_| DelegationError::AuthorityUnavailable)?;
    let mut store = PgDelegationClaimStore::new(&mut tx, community_id);
    let lineage = match open {
        None => {
            let proof = store
                .prove_no_open_parent(community_id, &request.source_agent)?
                .ok_or(DelegationError::ParentBindingMismatch)?;
            ResolvedDelegationLineage::root_for_run(
                community_id,
                Uuid::new_v4(),
                &request.source_agent,
                operator_pubkey,
                request.expires_at,
                proof,
            )
        }
        Some((parent_delegation_id, _)) => {
            let permit = store
                .reopen_live_permit(community_id, *parent_delegation_id)?
                .ok_or(DelegationError::ParentBindingMismatch)?;
            ResolvedDelegationLineage::parent_from_permit(&permit)
        }
    };
    let _ = tx.rollback().await;
    Ok(lineage)
}

/// The single `buzz:delegation-run` tag on an agent-signed outcome event, or
/// `None` when the event carries no such tag (not a delegation outcome).
pub fn single_delegation_run_tag(event: &Event) -> Option<Uuid> {
    let mut found = None;
    for tag in event.tags.iter() {
        let slice = tag.as_slice();
        if slice.first().map(|s| s.as_str()) == Some("buzz:delegation-run") {
            let value = slice.get(1)?.as_str();
            if found.is_some() {
                return None; // more than one — never trust an ambiguous tag
            }
            found = Some(Uuid::parse_str(value).ok()?);
        }
    }
    found
}

/// Settle a delegation action from its agent-signed outcome event.
pub async fn settle_outcome(
    state: Arc<AppState>,
    tenant: TenantContext,
    run_id: Uuid,
    stored_event: buzz_core::StoredEvent,
) {
    let community_id = tenant.community();
    let action = match state.db.find_delegation_action_by_run(community_id, run_id).await {
        Ok(Some(action)) => action,
        Ok(None) | Err(_) => return,
    };
    if stored_event.event.pubkey.to_hex() != action.target_agent_hex {
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %action.delegation_id,
            reason = "signer_mismatch",
            "delegation_outcome_rejected"
        );
        return;
    }

    let mut outcome_word = None;
    let mut tokens: Option<i64> = None;
    for tag in stored_event.event.tags.iter() {
        let slice = tag.as_slice();
        match slice.first().map(|s| s.as_str()) {
            Some("buzz:delegation-outcome") => {
                if outcome_word.is_some() {
                    tracing::info!(
                        target: "buzz_relay::delegation",
                        delegation_id = %action.delegation_id,
                        reason = "bad_outcome",
                        "delegation_outcome_rejected"
                    );
                    return;
                }
                outcome_word = slice.get(1).map(|s| s.as_str().to_owned());
            }
            Some("buzz:delegation-tokens") => {
                if tokens.is_some() {
                    tracing::info!(
                        target: "buzz_relay::delegation",
                        delegation_id = %action.delegation_id,
                        reason = "bad_outcome",
                        "delegation_outcome_rejected"
                    );
                    return;
                }
                tokens = slice.get(1).and_then(|s| s.as_str().parse::<i64>().ok());
            }
            _ => {}
        }
    }
    let Some(outcome_word) = outcome_word else {
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %action.delegation_id,
            reason = "bad_outcome",
            "delegation_outcome_rejected"
        );
        return;
    };
    if !matches!(
        outcome_word.as_str(),
        "delivered" | "delegated" | "failed" | "budget_exceeded"
    ) {
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %action.delegation_id,
            reason = "bad_outcome",
            "delegation_outcome_rejected"
        );
        return;
    }
    if tokens.is_none() {
        tracing::debug!(
            target: "buzz_relay::delegation",
            delegation_id = %action.delegation_id,
            "tokens=unknown"
        );
    }

    let settlement = match state
        .db
        .settle_delegation_action(
            community_id,
            action.delegation_id,
            action.action_seq,
            &outcome_word,
            tokens,
            None,
        )
        .await
    {
        Ok(settlement) => settlement,
        Err(_) => return,
    };

    let tokens_str = tokens.map(|t| t.to_string()).unwrap_or_else(|| "unknown".into());
    match outcome_word.as_str() {
        "delivered" => {
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %action.delegation_id,
                tokens = %tokens_str,
                "delegation_delivered"
            );
            metrics::counter!("buzz_delegation_outcomes_total", "outcome" => "delivered")
                .increment(1);
        }
        "delegated" => {
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %action.delegation_id,
                reason = "awaiting_child",
                "delegation_approved"
            );
        }
        "failed" | "budget_exceeded" => {
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %action.delegation_id,
                reason = %settlement.state_after,
                tokens = %tokens_str,
                "delegation_failed"
            );
            metrics::counter!("buzz_delegation_outcomes_total", "outcome" => "failed")
                .increment(1);
        }
        _ => {}
    }

    if let Some((parent_delegation_id, _)) = &settlement.parent {
        if matches!(settlement.state_after.as_str(), "delivered" | "failed" | "expired") {
            let child_answer_event_id = Some(stored_event.event.id.to_hex());
            dispatch::dispatch_next(&state, community_id, *parent_delegation_id, child_answer_event_id)
                .await;
        }
    }

    if settlement.notice_due {
        let detail = settlement.failure_detail.as_deref().unwrap_or("refused");
        if let Ok(notice_id) = notices::post_failure_notice(
            &state,
            &tenant,
            action.delegation_id,
            &action.target_agent_hex,
            settlement.origin_channel_id,
            hex::encode(&settlement.origin_event_id),
            detail,
        )
        .await
        {
            // Persist the notice id so the notice_due guard is durable
            // (I-7/I-15: at most one failed notice), mirroring the
            // summary-notice path above.
            let _ = state
                .db
                .record_delegation_failure_notice(community_id, action.delegation_id, &notice_id)
                .await;
        }
    }
}

