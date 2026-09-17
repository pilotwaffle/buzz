//! Delegation action dispatcher: builds and signs the wake for a
//! delegation's next turn, per build_spec.md 3.6.

use std::sync::Arc;

use chrono::Utc;
use nostr::{EventBuilder, Kind, Tag};
use uuid::Uuid;

use buzz_core::delegation::{
    cas_action, validate_for_claim, validate_next_action, DelegationError,
    ResolvedDelegationActionFacts, ResolvedDelegationFacts,
};
use buzz_core::kind::KIND_STREAM_MESSAGE;
use buzz_core::CommunityId;
use buzz_db::delegation::{DelegationRecordRow, PgDelegationActionStore};

use crate::state::AppState;

/// Load the record, revalidate, and dispatch the next action for
/// `delegation_id`, if it is still `approved`.
///
/// `child_answer_event_id` is `Some` only when this call is the direct
/// result of a child delegation reaching a terminal state (build_spec.md
/// 3.7); it is attached to the continuation wake and durably recorded on the
/// dispatched action row so a sweeper retry of the same action (3.8) can
/// recover it without the caller having to remember it.
pub async fn dispatch_next(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    child_answer_event_id: Option<String>,
) {
    let Ok(Some(row)) = state.db.load_delegation_record(community_id, delegation_id).await else {
        return;
    };
    if row.state != "approved" {
        return;
    }
    // A sweeper retry (or any redispatch that lost the caller-supplied value)
    // recovers a continuation's child-answer id from the durable column on
    // the action that was already inserted for this delegation but never
    // dispatched, if one was recorded.
    let child_answer_event_id = match child_answer_event_id {
        Some(id) => Some(id),
        None => state
            .db
            .pending_delegation_child_answer(community_id, delegation_id)
            .await
            .ok()
            .flatten(),
    };
    // D-L2 (Slice 4.1): a turn-exhausted approved record now loads with
    // context = None instead of being hidden as Ok(None) -- settle it here,
    // before ever trying to rebuild a context that cannot exist for it.
    if row.remaining_turns == 0 {
        settle_current_and_notice(state, community_id, delegation_id, &row, "turns").await;
        return;
    }
    let Some(context) = row.context.clone() else {
        return;
    };

    // a. Rebuild facts and revalidate (NIP-DG "callers must first rebuild").
    // The lineage this delegation was claimed under is immutable once
    // claimed, but `ValidatedDelegationContext`'s only public constructor
    // (`validate_for_claim`) requires re-deriving it, exactly as the initial
    // approval did: `open_action_as_target` tells us root vs. parent, and
    // `super::resolve_lineage` proves/reopens it read-only.
    let Ok(agent_owners) = state
        .db
        .resolve_agent_owners(community_id, &row.record.request.agent_path)
        .await
    else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    // `event_operator_pubkey` always returns `Ok` (it reads a field off an
    // already-deserialized event); audited for symmetry with the rest of
    // this match chain per Slice 4.1 I-9, even though unreachable.
    let Ok(operator_pubkey) = event_operator_pubkey(&row) else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let open = match state
        .db
        .open_action_as_target(community_id, &row.record.request.source_agent)
        .await
    {
        Ok(open) => open,
        Err(_) => {
            audit_denied(delegation_id, "authority_unavailable");
            return;
        }
    };
    let Ok(lineage) = super::resolve_lineage(
        state,
        community_id,
        &row.record.request,
        &operator_pubkey,
        &open,
    )
    .await
    else {
        // Lineage loss is not owner loss: the root-vs-parent proof failed,
        // not an owner check, so this is `refused`, not `cancelled`.
        audit_denied(delegation_id, "parent_binding_mismatch");
        settle_record_and_notice(state, community_id, delegation_id, &row, "refused").await;
        return;
    };
    let facts = ResolvedDelegationFacts {
        community_id,
        now: Utc::now().timestamp().max(0) as u64,
        agent_owners,
        lineage,
    };
    match validate_for_claim(&row.record, Some(&context), Some(&row.approval_event), &facts) {
        Ok(validated) => {
            dispatch_action(
                state,
                community_id,
                delegation_id,
                &row,
                &validated,
                child_answer_event_id,
            )
            .await;
        }
        Err(DelegationError::OwnerMismatch) | Err(DelegationError::TenantMismatch) => {
            cancel_owner_unavailable(state, community_id, delegation_id, &row, None).await;
        }
        // The sweeper's own `expire_records` sweep notices an expired
        // approved record within one tick; leaving it un-settled here
        // avoids a duplicate settlement race with that sweep (Risk 5).
        Err(DelegationError::Expired) => {
            audit_denied(delegation_id, "expired");
        }
        Err(other) => {
            audit_denied(delegation_id, other.code());
            settle_record_and_notice(state, community_id, delegation_id, &row, "refused").await;
        }
    }
}

fn event_operator_pubkey(row: &DelegationRecordRow) -> Result<String, ()> {
    Ok(row.approval_event.pubkey.to_hex())
}

async fn dispatch_action(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    row: &DelegationRecordRow,
    validated: &buzz_core::delegation::ValidatedDelegationContext,
    child_answer_event_id: Option<String>,
) {
    // b. Cost cap set → every action refused cost_unknown (no cost oracle yet).
    if row.cost_cap_microusd.is_some() {
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %delegation_id,
            reason = "cost_unknown",
            "delegation_context_denied"
        );
        settle_current_and_notice(state, community_id, delegation_id, row, "cost_unknown").await;
        return;
    }

    // c. Turn ceiling. Defensive since Slice 4.1: `dispatch_next` now
    // settles a turn-exhausted record before ever calling `dispatch_action`,
    // so this branch should be unreachable in practice -- left in place
    // rather than removed, to keep the diff minimal.
    if row.remaining_turns == 0 {
        settle_current_and_notice(state, community_id, delegation_id, row, "turns").await;
        return;
    }

    let Ok(agent_owners) = state
        .db
        .resolve_agent_owners(community_id, &row.record.request.agent_path)
        .await
    else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let action_facts = ResolvedDelegationActionFacts {
        community_id,
        now: Utc::now().timestamp().max(0) as u64,
        agent_owners,
        remaining_turns: row.remaining_turns,
        cost_committed_microusd: None,
        action_cost_reservation_microusd: None,
    };
    let action = match validate_next_action(validated, &action_facts) {
        Ok(action) => action,
        Err(DelegationError::TurnLimitExceeded) => {
            settle_current_and_notice(state, community_id, delegation_id, row, "turns").await;
            return;
        }
        Err(DelegationError::OwnerMismatch) | Err(DelegationError::TenantMismatch) => {
            cancel_owner_unavailable(state, community_id, delegation_id, row, None).await;
            return;
        }
        Err(DelegationError::Expired) => {
            audit_denied(delegation_id, "expired");
            return;
        }
        Err(other) => {
            audit_denied(delegation_id, other.code());
            settle_record_and_notice(state, community_id, delegation_id, row, "refused").await;
            return;
        }
    };

    // d. Per-action compare-and-swap.
    let tx_now = Utc::now().timestamp().max(0) as u64;
    let Ok(mut tx) = state.db.begin_event_write_transaction().await else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let cas_result = {
        let mut store = PgDelegationActionStore::new(&mut tx, community_id, row.run_id);
        cas_action(&mut store, &action, tx_now)
    };
    let permit = match cas_result {
        Ok(permit) => {
            if tx.commit().await.is_err() {
                audit_denied(delegation_id, "authority_unavailable");
                return;
            }
            permit
        }
        Err(DelegationError::ActionConflict) => {
            let _ = tx.rollback().await;
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %delegation_id,
                reason = "action_conflict",
                "delegation_context_denied"
            );
            return;
        }
        Err(DelegationError::BudgetExhausted) => {
            let _ = tx.rollback().await;
            settle_current_and_notice(state, community_id, delegation_id, row, "budget").await;
            return;
        }
        Err(_) => {
            let _ = tx.rollback().await;
            tracing::info!(
                target: "buzz_relay::delegation",
                delegation_id = %delegation_id,
                reason = "store_unavailable",
                "delegation_context_denied"
            );
            return;
        }
    };

    // e. Re-verify ownership immediately before signing. A store error here
    // is audit-only, not settled: the CAS'd action stays open and the
    // sweeper times it out and retries (Slice 4.1 D-L3 table).
    let Ok(current_owners) = state
        .db
        .resolve_agent_owners(community_id, &row.record.request.agent_path)
        .await
    else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    if action.ensure_owner_snapshot(&current_owners).is_err() {
        let _ = state
            .db
            .settle_delegation_action(
                community_id,
                delegation_id,
                permit.action_seq(),
                "cancelled",
                None,
                Some("owner_changed"),
            )
            .await;
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %delegation_id,
            reason = "action_conflict",
            "delegation_context_denied"
        );
        return;
    }

    // f. Build and dispatch the wake.
    let Some(owner_pubkey_hex) = current_owners.first().and_then(|o| o.owner_pubkey.clone()) else {
        cancel_owner_unavailable(state, community_id, delegation_id, row, Some(permit.action_seq())).await;
        return;
    };
    let target_agent_hex = row.record.request.target_agent.clone();
    let wake_context = context_for_wake(row, &permit);
    let context_json = serde_json::to_string(&wake_context).unwrap_or_default();

    let origin_event_id_hex = &row.record.request.origin_event_id;
    let mut tag_results = vec![
        Tag::parse(["p", &owner_pubkey_hex]),
        Tag::parse(["h", &row.origin_channel_id.to_string()]),
        Tag::parse(["e", origin_event_id_hex, "", "root"]),
        Tag::parse(["e", origin_event_id_hex, "", "reply"]),
        Tag::parse(["buzz:workflow", "true"]),
        Tag::parse(["buzz:workflow-owner", &owner_pubkey_hex]),
        Tag::parse(["p", &target_agent_hex]),
        Tag::parse(["buzz:workflow-mention", &target_agent_hex]),
        Tag::parse(["buzz:delegation-run", &row.run_id.to_string()]),
        Tag::parse(["buzz:delegation", &delegation_id.to_string()]),
        Tag::parse(["buzz:delegation-context", &context_json]),
        Tag::parse([
            "buzz:delegation-budget",
            &permit.token_budget_remaining().to_string(),
        ]),
    ];
    if let Some(child_id) = &child_answer_event_id {
        tag_results.push(Tag::parse(["buzz:delegation-child-answer", child_id]));
    }
    // Post-CAS (Slice 4.1 D-L3, I-9): the CAS already committed -- turn
    // decremented, action row inserted -- so every bail from here on is
    // audit-only, never settled; the sweeper owns the retry.
    let Ok(tags) = tag_results.into_iter().collect::<Result<Vec<_>, _>>() else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };

    let content = if let Some(child_id) = &child_answer_event_id {
        format!(
            "Delegated task continued: the sub-delegation you requested has completed; \
             read its answer in this thread and finish the task.\n\ndelegation-run: {}\nchild-answer: {}",
            row.run_id, child_id
        )
    } else {
        format!(
            "Delegated task: read the originating message in this thread and complete it.\n\ndelegation-run: {}",
            row.run_id
        )
    };

    let Ok(event) = EventBuilder::new(Kind::from(KIND_STREAM_MESSAGE as u16), &content)
        .tags(tags)
        .sign_with_keys(&state.relay_keypair)
    else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let event_id_hex = event.id.to_hex();
    let event_id_bytes = event.id.as_bytes().to_vec();
    let event_created_at = {
        let ts = event.created_at.as_secs() as i64;
        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
    };

    let Ok(Some(host)) = state.db.lookup_community_host(community_id).await else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let tenant = buzz_core::tenant::TenantContext::resolved(community_id, host);

    let Ok(origin_bytes) = hex::decode(origin_event_id_hex) else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let Ok(Some(origin_stored)) = state
        .db
        .get_event_by_id_for_event_write(community_id, &origin_bytes)
        .await
    else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    let origin_created_at =
        chrono::DateTime::from_timestamp(origin_stored.event.created_at.as_secs() as i64, 0)
            .unwrap_or(event_created_at);

    let thread_meta = Some(buzz_db::event::ThreadMetadataParams {
        event_id: &event_id_bytes,
        event_created_at,
        channel_id: row.origin_channel_id,
        parent_event_id: Some(&origin_bytes),
        parent_event_created_at: Some(origin_created_at),
        root_event_id: Some(&origin_bytes),
        root_event_created_at: Some(origin_created_at),
        depth: 1,
        broadcast: false,
    });
    let Ok((stored_event, was_inserted)) = state
        .db
        .insert_event_with_thread_metadata(community_id, &event, Some(row.origin_channel_id), thread_meta)
        .await
    else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    if was_inserted {
        let _ = crate::handlers::event::dispatch_persistent_event(
            &tenant,
            state,
            &stored_event,
            KIND_STREAM_MESSAGE,
            &owner_pubkey_hex,
            None,
        )
        .await;
    }
    let child_answer_bytes = child_answer_event_id
        .as_deref()
        .and_then(|hex_id| hex::decode(hex_id).ok());
    let _ = state
        .db
        .mark_delegation_action_dispatched(
            community_id,
            delegation_id,
            permit.action_seq(),
            &event_id_bytes,
            child_answer_bytes.as_deref(),
        )
        .await;
    tracing::debug!(
        target: "buzz_relay::delegation",
        event_id = %event_id_hex,
        run_id = %row.run_id,
        "delegation wake dispatched"
    );
}

/// The wake's `buzz:delegation-context` payload: the row's execution context
/// with `remaining_turns` overridden to the permit's post-CAS count + 1 (the
/// target still has that many turns available including the one just
/// dispatched), per build_spec.md 3.6.f.
fn context_for_wake(
    row: &DelegationRecordRow,
    permit: &buzz_core::delegation::DelegationActionPermit,
) -> buzz_core::delegation::DelegationExecutionContext {
    let mut context = row
        .context
        .clone()
        .expect("dispatch_action only runs for an approved row with Some(context)");
    context.remaining_turns = permit.remaining_turns_after() + 1;
    context
}

/// Find the most recent agent-signed outcome event id for a `delegated`
/// action on this delegation, to attach as `buzz:delegation-child-answer` on
/// a continuation wake.
/// Settle the delegation's current open (or about-to-be-first) action and
/// post the failure notice, for a refusal that happens before any CAS is
/// attempted (cost cap, turn ceiling).
///
/// Slice 4.1 (D-L2, `2.2`): the settlement source depends on
/// `row.latest_action_open` -- an open action is settled through it
/// (unchanged path); when no action is open (a turn-exhausted parent whose
/// action 1 already settled `delegated`, say), the record itself is settled
/// directly via `fail_delegation_record`, never overwriting the already-
/// settled action row (`settle_action` has no `settled_at IS NULL` guard).
async fn settle_current_and_notice(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    row: &DelegationRecordRow,
    detail: &str,
) {
    if row.latest_action_open {
        // The first action row was already inserted atomically with the
        // claim (Step 2, I-5); every subsequent action row is inserted only
        // by a successful CAS. So the row to settle is always
        // `latest_action_seq`, which `load_delegation_record` computed as
        // the max recorded action_seq (at least 1, since the claim
        // transaction always inserts action_seq=1).
        let outcome = if detail == "budget" { "budget_exceeded" } else { "failed" };
        let Ok(settlement) = state
            .db
            .settle_delegation_action(
                community_id,
                delegation_id,
                row.latest_action_seq.max(1),
                outcome,
                None,
                Some(detail),
            )
            .await
        else {
            audit_denied(delegation_id, "authority_unavailable");
            return;
        };
        post_failure_notice_if_due(state, community_id, delegation_id, &settlement, detail).await;
    } else {
        let Ok(settlement) = state.db.fail_delegation_record(community_id, delegation_id, detail).await
        else {
            audit_denied(delegation_id, "authority_unavailable");
            return;
        };
        post_failure_notice_if_due(state, community_id, delegation_id, &settlement, detail).await;
    }
}

/// Post the failure notice for a settlement, if one is due, and persist its
/// id so the `notice_due` guard stays durable. Shared by both branches of
/// [`settle_current_and_notice`].
async fn post_failure_notice_if_due(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    settlement: &buzz_db::delegation::DelegationSettlement,
    detail: &str,
) {
    if !settlement.notice_due {
        return;
    }
    let Ok(Some(host)) = state.db.lookup_community_host(community_id).await else {
        audit_denied(delegation_id, "notice_post_failed");
        return;
    };
    let tenant = buzz_core::tenant::TenantContext::resolved(community_id, host);
    let target_agent_hex = hex::encode(&settlement.target_agent);
    match super::notices::post_failure_notice(
        state,
        &tenant,
        delegation_id,
        &target_agent_hex,
        settlement.origin_channel_id,
        hex::encode(&settlement.origin_event_id),
        detail,
    )
    .await
    {
        Ok(notice_id) => {
            // Persist the notice id so settle_delegation_action's notice_due
            // guard is durable (I-7/I-15: at most one failed notice).
            let _ = state
                .db
                .record_delegation_failure_notice(community_id, delegation_id, &notice_id)
                .await;
        }
        Err(_) => audit_denied(delegation_id, "notice_post_failed"),
    }
}

/// Log a `delegation_context_denied` audit line with `reason` (Slice 4.1
/// D-L3, I-9): every silent `return` in the dispatch path is preceded by
/// this, a `settle_*`/`cancel_*` call, or is the successful-dispatch exit.
fn audit_denied(delegation_id: Uuid, reason: &'static str) {
    tracing::info!(
        target: "buzz_relay::delegation",
        delegation_id = %delegation_id,
        reason,
        "delegation_context_denied"
    );
}

/// Cancel the open action (if any) as `owner_unavailable` and settle the
/// record `failed`/`cancelled` with one notice (Slice 4.1 D-L3): the shared
/// terminal path for "an agent in the path lost its owner" wherever that is
/// discovered -- before any action is open, or on an already-CAS'd one.
async fn cancel_owner_unavailable(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    row: &DelegationRecordRow,
    open_action_seq: Option<u32>,
) {
    audit_denied(delegation_id, "owner_unavailable");
    if let Some(seq) = open_action_seq {
        let _ = state
            .db
            .settle_delegation_action(community_id, delegation_id, seq, "cancelled", None, Some("owner_unavailable"))
            .await;
    }
    settle_record_and_notice(state, community_id, delegation_id, row, "cancelled").await;
}

/// Settle the delegation record itself (never an action row) as terminal
/// with one notice (Slice 4.1 D-L3) -- the not-open branch of `2.2`,
/// factored out so [`cancel_owner_unavailable`] and any other terminal
/// path that has no open action to settle share it.
async fn settle_record_and_notice(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    _row: &DelegationRecordRow,
    detail: &str,
) {
    let Ok(settlement) = state.db.fail_delegation_record(community_id, delegation_id, detail).await else {
        audit_denied(delegation_id, "authority_unavailable");
        return;
    };
    post_failure_notice_if_due(state, community_id, delegation_id, &settlement, detail).await;
}
