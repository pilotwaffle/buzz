//! Relay-signed delegation notices (kind 9, no `p`/`buzz:workflow-mention`).
//!
//! Notices never carry the origin task body — only identifiers, states,
//! limits, and timestamps (I-14, Q3.3).

use std::sync::Arc;

use chrono::Utc;
use nostr::{EventBuilder, Kind, Tag};
use uuid::Uuid;

use buzz_core::delegation::DelegationRequest;
use buzz_core::kind::KIND_STREAM_MESSAGE;
use buzz_core::tenant::TenantContext;

use crate::state::AppState;

fn short8(hex_pubkey: &str) -> String {
    hex_pubkey.chars().take(8).collect()
}

fn short_id8(uuid: Uuid) -> String {
    uuid.simple().to_string().chars().take(8).collect()
}

/// Post the relay-signed "approved" summary notice into the origin thread.
pub async fn post_summary_notice(
    state: &Arc<AppState>,
    tenant: &TenantContext,
    request: &DelegationRequest,
    origin_channel_id: Uuid,
    origin_event_id_hex: String,
) -> Result<Vec<u8>, ()> {
    let expires_at = chrono::DateTime::from_timestamp(request.expires_at as i64, 0)
        .unwrap_or_else(Utc::now)
        .to_rfc3339();
    let content = format!(
        "Delegation {} approved: {} → {}, up to {} turns, {} tokens, expires {}",
        short_id8(request.delegation_id),
        short8(&request.source_agent),
        short8(&request.target_agent),
        request.max_turns,
        request.token_budget,
        expires_at,
    );
    post_notice(
        state,
        tenant,
        origin_channel_id,
        &origin_event_id_hex,
        request.delegation_id,
        "approved",
        &content,
    )
    .await
}

/// Post the relay-signed "failed" notice into the origin thread.
pub async fn post_failure_notice(
    state: &Arc<AppState>,
    tenant: &TenantContext,
    delegation_id: Uuid,
    target_agent_hex: &str,
    origin_channel_id: Uuid,
    origin_event_id_hex: String,
    detail: &str,
) -> Result<Vec<u8>, ()> {
    let content = format!(
        "Delegation {} to {} did not complete: {}",
        short_id8(delegation_id),
        short8(target_agent_hex),
        detail,
    );
    post_notice(
        state,
        tenant,
        origin_channel_id,
        &origin_event_id_hex,
        delegation_id,
        "failed",
        &content,
    )
    .await
}

async fn post_notice(
    state: &Arc<AppState>,
    tenant: &TenantContext,
    channel_id: Uuid,
    origin_event_id_hex: &str,
    delegation_id: Uuid,
    notice_word: &str,
    content: &str,
) -> Result<Vec<u8>, ()> {
    let tags = vec![
        Tag::parse(["h", &channel_id.to_string()]).map_err(|_| ())?,
        Tag::parse(["e", origin_event_id_hex, "", "root"]).map_err(|_| ())?,
        Tag::parse(["e", origin_event_id_hex, "", "reply"]).map_err(|_| ())?,
        Tag::parse(["buzz:workflow", "true"]).map_err(|_| ())?,
        Tag::parse(["buzz:delegation", &delegation_id.to_string()]).map_err(|_| ())?,
        Tag::parse(["buzz:delegation-notice", notice_word]).map_err(|_| ())?,
    ];
    let event = EventBuilder::new(Kind::from(KIND_STREAM_MESSAGE as u16), content)
        .tags(tags)
        .sign_with_keys(&state.relay_keypair)
        .map_err(|_| ())?;
    let event_id_bytes = event.id.as_bytes().to_vec();
    let event_created_at = {
        let ts = event.created_at.as_secs() as i64;
        chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
    };
    // Consistency fix alongside D-L1: this notice's own `e` tags (above)
    // claim root=origin, so its stored thread metadata must agree -- a
    // `None` root here is currently harmless (nothing replies to a notice
    // today) but is the same latent trap that made the delegation outcome
    // event unresolvable: any future reply would hit "root tag does not
    // match thread ancestry" because the relay resolves ancestry from this
    // stored row, not by re-parsing the notice's own tags.
    let origin_bytes = hex::decode(origin_event_id_hex).map_err(|_| ())?;
    let origin_created_at = state
        .db
        .get_event_by_id_for_event_write(tenant.community(), &origin_bytes)
        .await
        .ok()
        .flatten()
        .map(|stored| {
            chrono::DateTime::from_timestamp(stored.event.created_at.as_secs() as i64, 0)
                .unwrap_or(event_created_at)
        })
        .unwrap_or(event_created_at);
    let thread_meta = Some(buzz_db::event::ThreadMetadataParams {
        event_id: &event_id_bytes,
        event_created_at,
        channel_id,
        parent_event_id: Some(&origin_bytes),
        parent_event_created_at: Some(origin_created_at),
        root_event_id: Some(&origin_bytes),
        root_event_created_at: Some(origin_created_at),
        depth: 1,
        broadcast: false,
    });
    let (stored_event, was_inserted) = state
        .db
        .insert_event_with_thread_metadata(tenant.community(), &event, Some(channel_id), thread_meta)
        .await
        .map_err(|_| ())?;
    if was_inserted {
        let _ = crate::handlers::event::dispatch_persistent_event(
            tenant,
            state,
            &stored_event,
            KIND_STREAM_MESSAGE,
            &state.relay_keypair.public_key().to_hex(),
            None,
        )
        .await;
    }
    Ok(event_id_bytes)
}
