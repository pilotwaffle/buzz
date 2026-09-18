use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    app_state::AppState,
    relay::{get_relay_json, relay_api_base_url_with_override, submit_signed_event_at_with_keys},
};

// ── Wire shapes (snake_case, consumed by tauriDelegations.ts) ────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApproveDelegationWire {
    pub event_id: String,
}

#[derive(Debug, Deserialize)]
struct TenantResponse {
    community_id: String,
}

/// Approve a drafted delegation and publish the operator approval.
///
/// `origin_event_id` is the id of the message containing the `buzz-delegation`
/// fenced block. `request_json` is that block's exact content, a
/// `DelegationRequestDraft`-shaped JSON that omits `origin_event_id` (D-4):
/// the drafting agent cannot know its own message's id before signing, so the
/// desktop fills it in here — from the message actually containing the block,
/// never from a value the block itself claims — before hashing and signing.
/// A block that already carries a **different** `origin_event_id` is refused
/// as a binding mismatch rather than silently overwritten.
#[tauri::command]
pub async fn approve_delegation(
    origin_event_id: String,
    request_json: String,
    state: State<'_, AppState>,
) -> Result<ApproveDelegationWire, String> {
    let draft: buzz_core_pkg::delegation::DelegationRequestDraft =
        serde_json::from_str(&request_json)
            .map_err(|e| format!("delegation block failed to parse: {e}"))?;
    if let Some(ref claimed) = draft.origin_event_id {
        if claimed != &origin_event_id {
            return Err("delegation block names a different origin_event_id than the message it was posted in".to_string());
        }
    }
    let request = draft.into_request(origin_event_id);

    let tenant: TenantResponse = get_relay_json(&state, "/delegations/tenant").await?;
    let community_id = buzz_core_pkg::tenant::CommunityId::from_uuid(
        uuid::Uuid::parse_str(&tenant.community_id)
            .map_err(|_| "relay returned an invalid community id".to_string())?,
    );

    let keys = state.signing_keys()?;
    let now = now_secs();
    let event = buzz_core_pkg::delegation::build_operator_approval_event(
        &keys, community_id, &request, now,
    )
    .map_err(|e| format!("failed to build delegation approval: {e:?}"))?;

    let api_base_url = relay_api_base_url_with_override(&state);
    let result = submit_signed_event_at_with_keys(&event, &state, &api_base_url, &keys)
        .await
        .map_err(|e| verbatim_blocked_reason(&e))?;

    Ok(ApproveDelegationWire {
        event_id: result.event_id,
    })
}

/// `submit_signed_event_at_with_keys` wraps every relay rejection as
/// `"relay rejected event: <message>"`. When the underlying message is one of
/// the relay's machine-readable `blocked:` refusals, surface it verbatim
/// instead of the wrapper — the operator needs the exact reason (e.g.
/// `blocked: delegation refused`), not a generic prefix (build_spec.md 6.2,
/// Q5).
fn verbatim_blocked_reason(error: &str) -> String {
    match error.find("blocked:") {
        Some(index) => error[index..].to_string(),
        None => error.to_string(),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_blocked_reason_strips_the_wrapper_prefix() {
        assert_eq!(
            verbatim_blocked_reason("relay rejected event: blocked: delegation refused"),
            "blocked: delegation refused"
        );
    }

    #[test]
    fn verbatim_blocked_reason_passes_through_non_blocked_errors_unchanged() {
        assert_eq!(
            verbatim_blocked_reason("relay rejected event: restricted: unknown event kind"),
            "relay rejected event: restricted: unknown event kind"
        );
    }

    #[test]
    fn verbatim_blocked_reason_passes_through_non_relay_errors_unchanged() {
        assert_eq!(
            verbatim_blocked_reason("network error: connection refused"),
            "network error: connection refused"
        );
    }
}
