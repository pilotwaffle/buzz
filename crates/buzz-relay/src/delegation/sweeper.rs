//! Delegation sweeper: a 60s loop that times out stalled actions and
//! expires past-deadline records, per build_spec.md 3.8.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::state::AppState;

const TICK_INTERVAL: Duration = Duration::from_secs(60);

/// Run the delegation sweeper loop until the process exits. Only spawned
/// when `state.delegation_enabled` is true.
pub async fn run(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        sweep_once(&state).await;
    }
}

async fn sweep_once(state: &Arc<AppState>) {
    let deadline_secs = state.workflow_engine.config().routine_outcome_deadline_secs;
    let deadline_cutoff = Utc::now() - chrono::Duration::seconds(deadline_secs as i64);

    let Ok(expired_actions) = state.db.expire_open_delegation_actions(deadline_cutoff).await else {
        return;
    };
    for (community_id, delegation_id, action_seq) in expired_actions {
        let Ok(settlement) = state
            .db
            .settle_delegation_action(community_id, delegation_id, action_seq, "timeout", None, None)
            .await
        else {
            continue;
        };
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %delegation_id,
            reason = %settlement.state_after,
            "delegation_failed"
        );
        if settlement.state_after == "approved" {
            // Retry: the record is still approved and has turns/expiry
            // remaining — dispatch again, consuming a turn.
            super::dispatch::dispatch_next(state, community_id, delegation_id, None).await;
        } else if settlement.notice_due {
            let detail = settlement.failure_detail.as_deref().unwrap_or("timeout");
            if let Ok(Some(host)) = state.db.lookup_community_host(community_id).await {
                let tenant = buzz_core::tenant::TenantContext::resolved(community_id, host);
                let target_agent_hex = hex::encode(&settlement.target_agent);
                let _ = super::notices::post_failure_notice(
                    state,
                    &tenant,
                    delegation_id,
                    &target_agent_hex,
                    settlement.origin_channel_id,
                    hex::encode(&settlement.origin_event_id),
                    detail,
                )
                .await;
            }
        }
    }

    let Ok(expired_records) = state.db.expire_delegation_records(Utc::now()).await else {
        return;
    };
    for (community_id, delegation_id) in expired_records {
        tracing::info!(
            target: "buzz_relay::delegation",
            delegation_id = %delegation_id,
            reason = "expired",
            "delegation_failed"
        );
        let Ok(Some(row)) = state.db.load_delegation_record(community_id, delegation_id).await else {
            continue;
        };
        if let Ok(Some(host)) = state.db.lookup_community_host(community_id).await {
            let tenant = buzz_core::tenant::TenantContext::resolved(community_id, host);
            let _ = super::notices::post_failure_notice(
                state,
                &tenant,
                delegation_id,
                &row.record.request.target_agent,
                row.origin_channel_id,
                row.record.request.origin_event_id.clone(),
                "expired",
            )
            .await;
        }
    }
}
