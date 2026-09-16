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
        return;
    };
    let Ok(operator_pubkey) = event_operator_pubkey(&row) else {
        return;
    };
    let open = match state
        .db
        .open_action_as_target(community_id, &row.record.request.source_agent)
        .await
    {
        Ok(open) => open,
        Err(_) => return,
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
        return;
    };
    let facts = ResolvedDelegationFacts {
        community_id,
        now: Utc::now().timestamp().max(0) as u64,
        agent_owners,
        lineage,
    };
    let Ok(validated) =
        validate_for_claim(&row.record, Some(&context), Some(&row.approval_event), &facts)
    else {
        return;
    };

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

    // c. Turn ceiling.
    if row.remaining_turns == 0 {
        settle_current_and_notice(state, community_id, delegation_id, row, "turns").await;
        return;
    }

    let Ok(agent_owners) = state
        .db
        .resolve_agent_owners(community_id, &row.record.request.agent_path)
        .await
    else {
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
        Err(_) => return,
    };

    // d. Per-action compare-and-swap.
    let tx_now = Utc::now().timestamp().max(0) as u64;
    let Ok(mut tx) = state.db.begin_event_write_transaction().await else {
        return;
    };
    let cas_result = {
        let mut store = PgDelegationActionStore::new(&mut tx, community_id, row.run_id);
        cas_action(&mut store, &action, tx_now)
    };
    let permit = match cas_result {
        Ok(permit) => {
            if tx.commit().await.is_err() {
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

    // e. Re-verify ownership immediately before signing.
    let Ok(current_owners) = state
        .db
        .resolve_agent_owners(community_id, &row.record.request.agent_path)
        .await
    else {
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
        return;
    };
    let target_agent_hex = row.record.request.target_agent.clone();
    let wake_context = context_for_wake(row, &permit);
    let context_json = serde_json::to_string(&wake_context).unwrap_or_default();

    let mut tag_results = vec![
        Tag::parse(["p", &owner_pubkey_hex]),
        Tag::parse(["h", &row.origin_channel_id.to_string()]),
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
    let Ok(tags) = tag_results.into_iter().collect::<Result<Vec<_>, _>>() else {
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
        return;
    };
    let event_id_hex = event.id.to_hex();
    let event_id_bytes = event.id.as_bytes().to_vec();
    let event_created_at = {
        let ts = event.created_at.as_secs() as i64;
        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
    };

    let Ok(Some(host)) = state.db.lookup_community_host(community_id).await else {
        return;
    };
    let tenant = buzz_core::tenant::TenantContext::resolved(community_id, host);

    let thread_meta = Some(buzz_db::event::ThreadMetadataParams {
        event_id: &event_id_bytes,
        event_created_at,
        channel_id: row.origin_channel_id,
        parent_event_id: None,
        parent_event_created_at: None,
        root_event_id: None,
        root_event_created_at: None,
        depth: 0,
        broadcast: false,
    });
    let Ok((stored_event, was_inserted)) = state
        .db
        .insert_event_with_thread_metadata(community_id, &event, Some(row.origin_channel_id), thread_meta)
        .await
    else {
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
async fn settle_current_and_notice(
    state: &Arc<AppState>,
    community_id: CommunityId,
    delegation_id: Uuid,
    row: &DelegationRecordRow,
    detail: &str,
) {
    // The first action row was already inserted atomically with the claim
    // (Step 2, I-5); every subsequent action row is inserted only by a
    // successful CAS. So the row to settle is always `latest_action_seq`,
    // which `load_delegation_record` computed as the max recorded action_seq
    // (at least 1, since the claim transaction always inserts action_seq=1).
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
        return;
    };
    if !settlement.notice_due {
        return;
    }
    let Ok(Some(host)) = state.db.lookup_community_host(community_id).await else {
        return;
    };
    let tenant = buzz_core::tenant::TenantContext::resolved(community_id, host);
    let target_agent_hex = hex::encode(&settlement.target_agent);
    if let Ok(notice_id) = super::notices::post_failure_notice(
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
        // Persist the notice id so settle_delegation_action's notice_due
        // guard is durable (I-7/I-15: at most one failed notice).
        let _ = state
            .db
            .record_delegation_failure_notice(community_id, delegation_id, &notice_id)
            .await;
    }
}
