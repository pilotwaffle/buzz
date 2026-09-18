//! Authorized read for the delegation tenant binding (Slice 4, spec 3.9).
//!
//! The desktop needs the relay's resolved `community_id` to compute the
//! delegation-approval hash before it can build and sign a 43007 event —
//! this is the only piece of tenant state it cannot derive locally.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::Value;

use crate::{
    api::{api_error, bridge},
    state::AppState,
};

/// `GET /delegations/tenant` — member-only, `{"community_id": "<uuid>"}`.
///
/// Registered only when `state.delegation_enabled` (see `router.rs`), so the
/// off state 404s identically to an unknown route rather than this handler
/// running and refusing internally.
pub async fn tenant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let raw_host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let tenant = crate::tenant::bind_community(&state.db, raw_host)
        .await
        .map_err(|_| {
            api_error(
                StatusCode::NOT_FOUND,
                "relay: no community is configured for this host",
            )
        })?;

    let url = bridge::nip98_expected_url(&state.config.relay_url, &tenant, "/delegations/tenant");
    let bridge::VerifiedBridgeAuth {
        pubkey,
        event_id_bytes,
        signed_created_at,
    } = bridge::verify_bridge_auth(&headers, "GET", &url, None, state.config.require_auth_token)?;
    bridge::enforce_http_admission(&state, &tenant, &pubkey).await?;
    bridge::check_nip98_replay(&state, &tenant, event_id_bytes).await?;

    let pubkey_bytes = pubkey.to_bytes().to_vec();
    let auth_tag = super::relay_members::extract_auth_tag_header(&headers);
    super::relay_members::enforce_relay_membership(
        &state,
        tenant.community(),
        &pubkey_bytes,
        auth_tag,
        signed_created_at,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "community_id": tenant.community().to_string(),
    })))
}
