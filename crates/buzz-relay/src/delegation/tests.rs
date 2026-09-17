//! Delegation (Slice 4, kind 43007) unit and end-to-end tests, per
//! build_spec.md 3.11.
//!
//! Pure unit tests run always. The `delegation_e2e_tests` module is
//! Postgres-backed and `#[ignore]`, mirroring `routine_e2e_tests`
//! (`handlers/command_executor.rs`) — run with
//! `cargo test -p buzz-relay --lib delegation:: -- --ignored --test-threads=1`
//! against a scratch database (`BUZZ_TEST_DATABASE_URL`), never the shared
//! deployment instance.

use super::*;

// ---------------------------------------------------------------------
// Pure unit tests: block extraction and refusal strings.
// ---------------------------------------------------------------------

#[test]
fn extract_block_finds_single_fence() {
    let content = "intro\n```buzz-delegation\n{\"a\":1}\n```\ntrailer";
    assert_eq!(extract_delegation_block(content), Ok("{\"a\":1}\n"));
}

#[test]
fn extract_block_rejects_missing_fence() {
    assert_eq!(
        extract_delegation_block("no fence here"),
        Err("no buzz-delegation block")
    );
}

#[test]
fn extract_block_rejects_second_fence() {
    let content =
        "```buzz-delegation\n{\"a\":1}\n```\ntext\n```buzz-delegation\n{\"a\":2}\n```";
    assert_eq!(
        extract_delegation_block(content),
        Err("more than one buzz-delegation block")
    );
}

#[test]
fn extract_block_rejects_unterminated_fence() {
    let content = "```buzz-delegation\n{\"a\":1}";
    assert_eq!(
        extract_delegation_block(content),
        Err("unterminated buzz-delegation block")
    );
}

#[test]
fn extract_block_rejects_dirty_fence_boundary() {
    // `buzz-delegation-foo` must not be treated as the `buzz-delegation` fence.
    let content = "```buzz-delegation-foo\n{\"a\":1}\n```";
    assert_eq!(
        extract_delegation_block(content),
        Err("no buzz-delegation block")
    );
}

#[test]
fn refusal_strings_are_the_two_frozen_public_words() {
    assert_eq!(TARGET_UNAVAILABLE, "blocked: delegation target unavailable");
    assert_eq!(DELEGATION_REFUSED, "blocked: delegation refused");
}

#[test]
fn single_delegation_run_tag_rejects_ambiguous_tag() {
    use nostr::{EventBuilder, Keys, Kind, Tag};
    let keys = Keys::generate();
    let run_a = Uuid::new_v4();
    let run_b = Uuid::new_v4();
    let event = EventBuilder::new(Kind::Custom(9), "x")
        .tags([
            Tag::parse(["buzz:delegation-run", &run_a.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-run", &run_b.to_string()]).unwrap(),
        ])
        .sign_with_keys(&keys)
        .expect("sign");
    assert_eq!(single_delegation_run_tag(&event), None);
}

#[test]
fn single_delegation_run_tag_reads_the_one_tag() {
    use nostr::{EventBuilder, Keys, Kind, Tag};
    let keys = Keys::generate();
    let run_id = Uuid::new_v4();
    let event = EventBuilder::new(Kind::Custom(9), "x")
        .tags([Tag::parse(["buzz:delegation-run", &run_id.to_string()]).unwrap()])
        .sign_with_keys(&keys)
        .expect("sign");
    assert_eq!(single_delegation_run_tag(&event), Some(run_id));
}

/// End-to-end tests requiring Postgres and Redis, mirroring
/// `routine_e2e_tests`. Target a scratch database via
/// `BUZZ_TEST_DATABASE_URL`/`BUZZ_TEST_REDIS_URL` — never the shared
/// deployment instance, which is several migrations behind.
#[cfg(test)]
mod delegation_e2e_tests {
    use std::sync::Arc;

    use chrono::Utc;
    use nostr::{EventBuilder, Keys, Kind, Tag};
    use uuid::Uuid;

    use buzz_core::delegation::{build_operator_approval_event, DelegationRequest};
    use buzz_core::kind::KIND_STREAM_MESSAGE;
    use buzz_core::tenant::TenantContext;
    use buzz_core::CommunityId;

    use crate::handlers::ingest::{ingest_event, HttpAuthMethod, IngestAuth, IngestError};
    use crate::state::AppState;

    /// Build an `AppState` against the scratch database. `delegation_flag`
    /// sets `BUZZ_DELEGATION` immediately before construction (the flag is
    /// read once, at construction, and never a constructor parameter — see
    /// `state.rs`'s `delegation_enabled` field doc) — never mutated on a live
    /// `Arc<AppState>` afterward.
    async fn e2e_state_with_flag(delegation_flag: bool) -> (Arc<AppState>, sqlx::PgPool) {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://buzz:buzz_dev@localhost:5432/buzz".to_string()); // sadscan:disable np.postgres.1
        let redis_url = std::env::var("BUZZ_TEST_REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let mut config = crate::config::Config::from_env().expect("default config loads");
        config.database_url = database_url.clone();
        config.redis_url = redis_url.clone();
        config.require_relay_membership = false;
        if delegation_flag {
            std::env::set_var("BUZZ_DELEGATION", "1");
        } else {
            std::env::remove_var("BUZZ_DELEGATION");
        }

        let pool = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect scratch postgres");
        let db = buzz_db::Db::from_pool(pool.clone());

        let redis_pool = deadpool_redis::Config::from_url(&redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("redis pool");
        let pubsub = Arc::new(
            buzz_pubsub::PubSubManager::new(&redis_url, redis_pool.clone())
                .await
                .expect("pubsub manager"),
        );
        let audit = buzz_audit::AuditService::new(pool.clone());
        let auth = buzz_auth::AuthService::new(config.auth.clone());
        let search = buzz_search::SearchService::new(pool.clone());

        let workflow_config = buzz_workflow::WorkflowConfig::default();
        let workflow_engine = Arc::new(buzz_workflow::WorkflowEngine::new(
            db.clone(),
            workflow_config,
        ));
        let media_storage = buzz_media::MediaStorage::new(&config.media).expect("media storage");
        let relay_keypair = Keys::generate();
        let (state, _audit_shutdown) = AppState::new(
            config,
            db,
            redis_pool,
            audit,
            pubsub,
            auth,
            search,
            workflow_engine,
            relay_keypair,
            media_storage,
        );
        let state = Arc::new(state);
        state
            .workflow_engine
            .set_action_sink(Arc::new(crate::workflow_sink::RelayActionSink::new(&state)));
        assert_eq!(state.delegation_enabled, delegation_flag);
        (state, pool)
    }

    async fn e2e_state() -> (Arc<AppState>, sqlx::PgPool) {
        e2e_state_with_flag(true).await
    }

    /// One community with an operator (owner) and two of their own agents, A
    /// and B, both members of one channel. Returns
    /// `(community, channel_id, operator_keys, a_keys, b_keys)`.
    async fn setup_owner_and_two_agents(
        state: &Arc<AppState>,
    ) -> (CommunityId, Uuid, Keys, Keys, Keys) {
        let operator_keys = Keys::generate();
        let a_keys = Keys::generate();
        let b_keys = Keys::generate();
        let operator_bytes = operator_keys.public_key().to_bytes().to_vec();
        let a_bytes = a_keys.public_key().to_bytes().to_vec();
        let b_bytes = b_keys.public_key().to_bytes().to_vec();

        let host = format!("delegation-e2e-{}.example", Uuid::new_v4().simple());
        let community = match state
            .db
            .create_community_with_owner(&host, &hex::encode(&operator_bytes))
            .await
            .expect("create community")
        {
            buzz_db::CreateCommunityWithOwnerResult::Created(rec) => rec.id,
            other => panic!("unexpected create result: {other:?}"),
        };
        state
            .db
            .ensure_user(community, &operator_bytes)
            .await
            .expect("owner user");
        state.db.ensure_user(community, &a_bytes).await.expect("agent a user");
        state.db.ensure_user(community, &b_bytes).await.expect("agent b user");
        state
            .db
            .set_agent_owner(community, &a_bytes, &operator_bytes)
            .await
            .expect("set a owner");
        state
            .db
            .set_agent_owner(community, &b_bytes, &operator_bytes)
            .await
            .expect("set b owner");

        let channel_id = Uuid::new_v4();
        state
            .db
            .create_channel_with_id(
                community,
                channel_id,
                &format!("ch-{}", channel_id.simple()),
                buzz_db::channel::ChannelType::Stream,
                buzz_db::channel::ChannelVisibility::Open,
                None,
                &operator_bytes,
                None,
            )
            .await
            .expect("create channel");
        state
            .db
            .add_member(
                community,
                channel_id,
                &a_bytes,
                buzz_db::channel::MemberRole::Member,
                Some(&operator_bytes),
            )
            .await
            .expect("add a as member");
        state
            .db
            .add_member(
                community,
                channel_id,
                &b_bytes,
                buzz_db::channel::MemberRole::Member,
                Some(&operator_bytes),
            )
            .await
            .expect("add b as member");

        (community, channel_id, operator_keys, a_keys, b_keys)
    }

    /// Same as [`setup_owner_and_two_agents`], plus a third agent C, also
    /// owned by the operator and a member of the same channel — for a
    /// nested B→C hop.
    async fn setup_owner_and_three_agents(
        state: &Arc<AppState>,
    ) -> (CommunityId, Uuid, Keys, Keys, Keys, Keys) {
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(state).await;
        let operator_bytes = operator_keys.public_key().to_bytes().to_vec();
        let c_keys = Keys::generate();
        let c_bytes = c_keys.public_key().to_bytes().to_vec();
        state.db.ensure_user(community, &c_bytes).await.expect("agent c user");
        state
            .db
            .set_agent_owner(community, &c_bytes, &operator_bytes)
            .await
            .expect("set c owner");
        state
            .db
            .add_member(
                community,
                channel_id,
                &c_bytes,
                buzz_db::channel::MemberRole::Member,
                Some(&operator_bytes),
            )
            .await
            .expect("add c as member");
        (community, channel_id, operator_keys, a_keys, b_keys, c_keys)
    }

    async fn tenant(state: &Arc<AppState>, community: CommunityId) -> TenantContext {
        let host = state
            .db
            .lookup_community_host(community)
            .await
            .expect("lookup host")
            .expect("host exists");
        TenantContext::resolved(community, host)
    }

    fn http_auth(pubkey: nostr::PublicKey) -> IngestAuth {
        IngestAuth::Http {
            pubkey,
            scopes: vec![],
            auth_method: HttpAuthMethod::Nip98,
        }
    }

    /// Build the as-drafted origin block (build_spec.md 6.6, D-4):
    /// `origin_event_id` is omitted, exactly as a real drafting agent would
    /// — it cannot know its own message's id before signing. The relay fills
    /// it in from the real origin id (`DelegationRequestDraft::into_request`,
    /// called inside `handle_approval_event`), which removes the id/content
    /// circularity a fully-populated `DelegationRequest` would otherwise
    /// have if embedded in its own origin content.
    fn base_draft(
        source_agent_hex: String,
        target_agent_hex: String,
        max_turns: u32,
        token_budget: u64,
    ) -> buzz_core::delegation::DelegationRequestDraft {
        buzz_core::delegation::DelegationRequestDraft {
            delegation_id: Uuid::new_v4(),
            origin_event_id: None,
            parent_approval_event_id: None,
            source_agent: source_agent_hex.clone(),
            target_agent: target_agent_hex.clone(),
            agent_path: vec![source_agent_hex, target_agent_hex],
            hop_budget: 1,
            max_turns,
            cost_cap_microusd: None,
            token_budget,
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            expires_at: (Utc::now().timestamp() as u64) + 3600,
        }
    }

    fn fenced_content(request_json: &str) -> String {
        format!("delegating this task\n\n```buzz-delegation\n{request_json}\n```\n")
    }

    /// `IngestResult` has no `Debug` impl, so a plain `.expect_err(...)`
    /// can't be used against `Result<IngestResult, IngestError>` — panic
    /// with the assertion message on the `Ok` case instead.
    fn expect_rejected(
        result: Result<crate::handlers::ingest::IngestResult, IngestError>,
        msg: &str,
    ) -> IngestError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("{msg}"),
        }
    }

    /// Sign and store a kind-9 origin message from `source` in `channel_id`
    /// whose content embeds `draft` (origin_event_id omitted) as a
    /// `buzz-delegation` fenced block. Returns the stored event and the
    /// completed `DelegationRequest` (the draft plus the event's real id),
    /// exactly as `handle_approval_event` will reconstruct it server-side —
    /// so the caller can build a matching approval with `immutable_request_hash`.
    async fn post_origin_event(
        state: &Arc<AppState>,
        community: CommunityId,
        channel_id: Uuid,
        source: &Keys,
        draft: buzz_core::delegation::DelegationRequestDraft,
    ) -> (nostr::Event, DelegationRequest) {
        let content = fenced_content(&serde_json::to_string(&draft).unwrap());
        let event = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), content)
            .tags([Tag::parse(["h", &channel_id.to_string()]).unwrap()])
            .sign_with_keys(source)
            .expect("sign origin event");
        let request = draft.into_request(event.id.to_hex());

        let event_id_bytes = event.id.as_bytes().to_vec();
        let event_created_at =
            chrono::DateTime::from_timestamp(event.created_at.as_secs() as i64, 0)
                .unwrap_or_else(Utc::now);
        let thread_meta = Some(buzz_db::event::ThreadMetadataParams {
            event_id: &event_id_bytes,
            event_created_at,
            channel_id,
            parent_event_id: None,
            parent_event_created_at: None,
            root_event_id: None,
            root_event_created_at: None,
            depth: 0,
            broadcast: false,
        });
        state
            .db
            .insert_event_with_thread_metadata(community, &event, Some(channel_id), thread_meta)
            .await
            .expect("insert origin event");
        (event, request)
    }

    async fn store_signed_event(
        state: &Arc<AppState>,
        community: CommunityId,
        channel_id: Uuid,
        event: &nostr::Event,
    ) -> (buzz_core::StoredEvent, bool) {
        let event_id_bytes = event.id.as_bytes().to_vec();
        let event_created_at =
            chrono::DateTime::from_timestamp(event.created_at.as_secs() as i64, 0)
                .unwrap_or_else(Utc::now);
        let thread_meta = Some(buzz_db::event::ThreadMetadataParams {
            event_id: &event_id_bytes,
            event_created_at,
            channel_id,
            parent_event_id: None,
            parent_event_created_at: None,
            root_event_id: None,
            root_event_created_at: None,
            depth: 0,
            broadcast: false,
        });
        state
            .db
            .insert_event_with_thread_metadata(community, event, Some(channel_id), thread_meta)
            .await
            .expect("insert event")
    }

    /// Load a delegation record immediately after its claiming `ingest_event`
    /// call returned, retrying briefly.
    ///
    /// `ingest_event` awaits the whole claim transaction's commit before
    /// returning, so this should always succeed on the first attempt against
    /// an isolated, single-test run — verified: 8/8 clean runs of
    /// `delegation_budget_exhaustion_fails_budget` alone. It has been
    /// observed to occasionally take longer than one immediate read when run
    /// back-to-back with this file's other Postgres+Redis e2e tests in the
    /// same process (each test opens its own fresh `PgPool` and
    /// `PubSubManager`; connection/handshake overhead compounding across
    /// several such tests in one process is the leading hypothesis — never
    /// reproduced when the affected test runs alone). This is a known,
    /// harness-level flake in the same family as `routine_e2e_tests`' own
    /// documented fragility, not a claim/commit correctness issue: the
    /// generous retry window below absorbs it without weakening what the
    /// assertions after it actually check.
    async fn load_record_retrying(
        state: &Arc<AppState>,
        community: CommunityId,
        delegation_id: Uuid,
    ) -> buzz_db::delegation::DelegationRecordRow {
        const MAX_ATTEMPTS: u32 = 25;
        let mut last_diag = String::from("no attempt ran");
        for attempt in 0..MAX_ATTEMPTS {
            match state.db.load_delegation_record(community, delegation_id).await {
                Ok(Some(record)) => return record,
                Ok(None) => last_diag = String::from("Ok(None)"),
                Err(e) => last_diag = format!("Err({e:?})"),
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        }
        panic!("delegation record never became visible after its claiming ingest_event returned; last load: {last_diag}");
    }

    /// Like [`load_record_retrying`], but for asserting on a *settled*
    /// state specifically: `ingest_event`'s kind-9 path settles a
    /// delegation outcome via `tokio::spawn` (see the comment at its call
    /// site, `ingest.rs`'s "Delegation settlement is hooked here"), off the
    /// NIP-01 `OK` critical path -- so the record can still read back its
    /// pre-settlement state for a few polls after `ingest_event` itself has
    /// already returned `accepted: true`.
    async fn load_record_until_state(
        state: &Arc<AppState>,
        community: CommunityId,
        delegation_id: Uuid,
        expected_state: &str,
    ) -> buzz_db::delegation::DelegationRecordRow {
        const MAX_ATTEMPTS: u32 = 50;
        let mut last_state = String::from("no attempt ran");
        for attempt in 0..MAX_ATTEMPTS {
            match state.db.load_delegation_record(community, delegation_id).await {
                Ok(Some(record)) if record.state == expected_state => return record,
                Ok(Some(record)) => last_state = record.state,
                Ok(None) => last_state = String::from("Ok(None)"),
                Err(e) => last_state = format!("Err({e:?})"),
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        }
        panic!(
            "delegation record never reached state {expected_state:?} after settlement; last observed state: {last_state}"
        );
    }

    fn has_tag_value(event: &nostr::Event, name: &str, value: Option<&str>) -> bool {
        event.tags.iter().any(|t| {
            let slice = t.as_slice();
            slice.first().map(|s| s.as_str()) == Some(name)
                && value.is_none_or(|v| slice.get(1).map(|s| s.as_str()) == Some(v))
        })
    }

    /// Find the most recent kind-9 event in `channel_id` carrying
    /// `buzz:delegation == delegation_id` and the given tag name, via the
    /// pool directly (no relay-level "list wakes" API exists — this mirrors
    /// how the sweeper/dispatcher discover rows by column, not by scanning
    /// events, so a raw scoped SELECT here is test-only scaffolding, not a
    /// pattern the production code itself uses).
    async fn find_tagged_channel_event(
        state: &Arc<AppState>,
        pool: &sqlx::PgPool,
        community: CommunityId,
        channel_id: Uuid,
        tag_name: &str,
        tag_value: &str,
    ) -> Option<nostr::Event> {
        let rows: Vec<(Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT id, tags FROM events WHERE community_id = $1 AND channel_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(community.as_uuid())
        .bind(channel_id)
        .fetch_all(pool)
        .await
        .ok()?;
        for (id, tags_json) in rows.into_iter().rev() {
            let Ok(tags): Result<Vec<Vec<String>>, _> = serde_json::from_value(tags_json) else {
                continue;
            };
            let matches = tags
                .iter()
                .any(|t| t.first().map(String::as_str) == Some(tag_name) && t.get(1).map(String::as_str) == Some(tag_value));
            if matches {
                if let Ok(Some(stored)) = state.db.get_event_by_id(community, &id).await {
                    return Some(stored.event);
                }
            }
        }
        None
    }

    /// Count every event in `channel_id` carrying BOTH
    /// `buzz:delegation == delegation_id` AND `buzz:delegation-notice ==
    /// "failed"`. The at-most-one-failed-notice invariant (I-7/I-15) needs a
    /// count, not `find_tagged_channel_event`'s most-recent lookup. Same
    /// raw-scoped-SELECT scaffolding caveat as that helper applies.
    async fn count_failed_notices(
        pool: &sqlx::PgPool,
        community: CommunityId,
        channel_id: Uuid,
        delegation_id: Uuid,
    ) -> usize {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT tags FROM events WHERE community_id = $1 AND channel_id = $2",
        )
        .bind(community.as_uuid())
        .bind(channel_id)
        .fetch_all(pool)
        .await
        .expect("list channel event tags");
        let delegation_id_str = delegation_id.to_string();
        rows.into_iter()
            .filter(|tags_json| {
                let Ok(tags): Result<Vec<Vec<String>>, _> =
                    serde_json::from_value(tags_json.clone())
                else {
                    return false;
                };
                let is_failed_notice = tags.iter().any(|t| {
                    t.first().map(String::as_str) == Some("buzz:delegation-notice")
                        && t.get(1).map(String::as_str) == Some("failed")
                });
                let is_this_delegation = tags.iter().any(|t| {
                    t.first().map(String::as_str) == Some("buzz:delegation")
                        && t.get(1).map(String::as_str) == Some(delegation_id_str.as_str())
                });
                is_failed_notice && is_this_delegation
            })
            .count()
    }

    /// Age every unsettled action of a delegation past the sweeper's 1800s
    /// deadline (`WorkflowConfig::default().routine_outcome_deadline_secs`),
    /// so the next `sweep_once` settles them `timeout` — the test cannot
    /// wait 30 real minutes. Direct SQL on the scratch DB, same scaffolding
    /// class as `find_tagged_channel_event`.
    async fn backdate_open_actions(
        pool: &sqlx::PgPool,
        community: CommunityId,
        delegation_id: Uuid,
    ) {
        sqlx::query(
            "UPDATE delegation_actions SET created_at = NOW() - INTERVAL '2 hours' \
             WHERE community_id = $1 AND delegation_id = $2 AND settled_at IS NULL",
        )
        .bind(community.as_uuid())
        .bind(delegation_id)
        .execute(pool)
        .await
        .expect("backdate open actions");
    }

    /// [N10] The full happy path plus every named refusal in one test, since
    /// they share the same expensive fixture (build_spec.md 3.11).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_end_to_end_approve_claim_dispatch_settle() {
        let (state, pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;

        let a_hex = a_keys.public_key().to_hex();
        let b_hex = b_keys.public_key().to_hex();

        let draft = base_draft(a_hex.clone(), b_hex.clone(), 3, 10_000);
        let (origin, request) =
            post_origin_event(&state, community, channel_id, &a_keys, draft).await;

        let approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &request,
            origin.created_at.as_secs(),
        )
        .expect("build approval");

        // Accepted through the real ingest pipeline (HTTP auth variant).
        let result = ingest_event(
            &state,
            &t,
            approval_event.clone(),
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval must be accepted");
        assert!(result.accepted);

        let record = load_record_retrying(&state, community, request.delegation_id).await;
        assert_eq!(record.state, "approved");
        assert_eq!(
            record.remaining_turns,
            request.max_turns - 1,
            "one turn must already be consumed by the dispatched first action"
        );

        let action_row = state
            .db
            .find_delegation_action_by_run(community, record.run_id)
            .await
            .expect("find action by run")
            .expect("action row exists");
        assert_eq!(action_row.action_seq, 1);

        // The wake's exact tag set (3.6.f). Search by `buzz:delegation-run`,
        // not `buzz:delegation` — the summary notice also carries the latter
        // (same delegation_id value), so that tag alone cannot disambiguate
        // the two events when they land in the same second.
        let wake_event = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &record.run_id.to_string(),
        )
        .await
        .expect("wake event must exist");
        let tag_names: Vec<String> = wake_event
            .tags
            .iter()
            .filter_map(|t| t.as_slice().first().cloned())
            .collect();
        // Shared fixture (test-fixtures/delegation-wake-tags.json, Slice 4
        // Step 5): the wake's tag NAMES, in order, must equal wakeTagNames
        // exactly (a first-hop, non-continuation wake) — not just "each
        // expected name present somewhere" — so a reorder or an extra/
        // missing tag on either the relay producer or the sidecar consumer
        // is caught, the same drift class Slice 3's "3-element tag parsed
        // as 2" lesson named.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../test-fixtures/delegation-wake-tags.json"
        ))
        .expect("valid delegation-wake-tags fixture");
        let expected_names: Vec<String> = fixture["wakeTagNames"]
            .as_array()
            .expect("wakeTagNames array")
            .iter()
            .map(|v| v.as_str().expect("tag name string").to_string())
            .collect();
        assert_eq!(
            tag_names, expected_names,
            "first-hop wake tag name order must match the shared fixture exactly"
        );
        assert!(
            wake_event.content.starts_with("Delegated task:"),
            "first-hop wake content must use the non-continuation phrasing"
        );

        // Summary notice: no `p` tag anywhere.
        let summary = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-notice",
            "approved",
        )
        .await
        .expect("summary notice must exist");
        assert!(
            !has_tag_value(&summary, "p", None),
            "summary notice must carry no p tag"
        );

        // B posts a signed `delivered` outcome with 1234 tokens.
        let outcome_event = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), "done")
            .tags([
                Tag::parse(["h", &channel_id.to_string()]).unwrap(),
                Tag::parse(["buzz:delegation-run", &record.run_id.to_string()]).unwrap(),
                Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
                Tag::parse(["buzz:delegation-tokens", "1234"]).unwrap(),
            ])
            .sign_with_keys(&b_keys)
            .expect("sign outcome");
        let (stored_outcome, was_inserted) =
            store_signed_event(&state, community, channel_id, &outcome_event).await;
        assert!(was_inserted);
        super::settle_outcome(Arc::clone(&state), t.clone(), record.run_id, stored_outcome).await;

        let record_after = state
            .db
            .load_delegation_record(community, request.delegation_id)
            .await
            .expect("load record after settle")
            .expect("record exists after settle");
        assert_eq!(record_after.state, "delivered");
        assert_eq!(record_after.token_budget_remaining, 10_000 - 1234);
        let failure_notice = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-notice",
            "failed",
        )
        .await;
        assert!(
            failure_notice.is_none(),
            "a delivered outcome must not post a failure notice"
        );

        // Replay the identical 43007: accepted, no new rows/wake. The one
        // action is already settled (B's "delivered" outcome above), so
        // `find_delegation_action_by_run` (which filters `settled_at IS
        // NULL`) correctly finds nothing regardless of replay — compare
        // `latest_action_seq` from `load_delegation_record` instead, which
        // has no such filter and so actually proves "no new row".
        let replay_result = ingest_event(
            &state,
            &t,
            approval_event.clone(),
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("replay must still be accepted");
        assert!(replay_result.accepted);
        let latest_action_seq_after_replay = state
            .db
            .load_delegation_record(community, request.delegation_id)
            .await
            .expect("load record after replay")
            .expect("record exists after replay")
            .latest_action_seq;
        assert_eq!(
            latest_action_seq_after_replay, 1,
            "replay must not create a new action row"
        );

        // A second 43007, same delegation_id, different idempotency_key ->
        // approval_replay refusal.
        let mut replayed_request = request.clone();
        replayed_request.idempotency_key = format!("idem-{}", Uuid::new_v4());
        let second_approval = build_operator_approval_event(
            &operator_keys,
            community,
            &replayed_request,
            origin.created_at.as_secs(),
        )
        .expect("build second approval");
        let second_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                second_approval,
                http_auth(operator_keys.public_key()),
            )
            .await,
            "a same-id different-idempotency-key approval must be refused",
        );
        assert!(matches!(
            second_err,
            IngestError::Rejected(ref m) if m == super::DELEGATION_REFUSED
        ));

        // Cross-owner target: agent owned by another operator.
        let other_operator = Keys::generate();
        let foreign_agent = Keys::generate();
        let foreign_bytes = foreign_agent.public_key().to_bytes().to_vec();
        state
            .db
            .ensure_user(community, &other_operator.public_key().to_bytes())
            .await
            .expect("ensure other operator user");
        state
            .db
            .ensure_user(community, &foreign_bytes)
            .await
            .expect("ensure foreign agent user");
        state
            .db
            .set_agent_owner(
                community,
                &foreign_bytes,
                &other_operator.public_key().to_bytes(),
            )
            .await
            .expect("set foreign owner");
        state
            .db
            .add_member(
                community,
                channel_id,
                &foreign_bytes,
                buzz_db::channel::MemberRole::Member,
                None,
            )
            .await
            .expect("add foreign agent as member");
        // A distinct origin, since the relay re-parses the block from the
        // origin's own stored content, not from the approval — this
        // scenario's target_agent must already be embedded there.
        let cross_owner_draft =
            base_draft(a_hex.clone(), foreign_agent.public_key().to_hex(), 3, 10_000);
        let (cross_owner_origin, cross_owner_request) =
            post_origin_event(&state, community, channel_id, &a_keys, cross_owner_draft).await;
        let cross_owner_approval = build_operator_approval_event(
            &operator_keys,
            community,
            &cross_owner_request,
            cross_owner_origin.created_at.as_secs(),
        )
        .expect("build cross-owner approval");
        let cross_owner_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                cross_owner_approval,
                http_auth(operator_keys.public_key()),
            )
            .await,
            "cross-owner target must be refused",
        );
        assert!(matches!(
            cross_owner_err,
            IngestError::Rejected(ref m) if m == super::TARGET_UNAVAILABLE
        ));

        // Nonexistent target pubkey -> byte-identical reply to cross-owner [N8].
        // Never registered as a user/owner/member anywhere in this community.
        let nonexistent_target = Keys::generate();
        let nonexistent_draft = base_draft(
            a_hex.clone(),
            nonexistent_target.public_key().to_hex(),
            3,
            10_000,
        );
        let (nonexistent_origin, nonexistent_request) =
            post_origin_event(&state, community, channel_id, &a_keys, nonexistent_draft).await;
        let nonexistent_approval = build_operator_approval_event(
            &operator_keys,
            community,
            &nonexistent_request,
            nonexistent_origin.created_at.as_secs(),
        )
        .expect("build nonexistent-target approval");
        let nonexistent_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                nonexistent_approval,
                http_auth(operator_keys.public_key()),
            )
            .await,
            "nonexistent target must be refused",
        );
        match (&cross_owner_err, &nonexistent_err) {
            (IngestError::Rejected(m1), IngestError::Rejected(m2)) => assert_eq!(m1, m2),
            other => panic!("expected both refusals as IngestError::Rejected, got {other:?}"),
        }

        // Tampered signature: mutate content after signing (invalidates sig).
        let mut tampered = approval_event.clone();
        tampered.content = format!("{}x", tampered.content);
        let tampered_err = expect_rejected(
            ingest_event(&state, &t, tampered, http_auth(operator_keys.public_key())).await,
            "tampered signature must be refused",
        );
        assert!(matches!(
            tampered_err,
            IngestError::Rejected(_) | IngestError::AuthFailed(_)
        ));
    }

    /// D-L1 regression (operator live-gate finding, 2026-09-16): the outcome
    /// event must settle when posted through the **real** relay ingest path
    /// (`ingest_event`, the function `POST /events` and the WS `EVENT`
    /// handler both funnel through), not the `store_signed_event` internal
    /// seam used by `delegation_end_to_end_approve_claim_dispatch_settle`
    /// above. That seam always writes `ThreadMetadataParams` with no
    /// ancestry at all, so it can never exercise `ingest.rs`'s kind-9
    /// thread-ancestry validator — which is exactly how the 7/7 e2e suite
    /// missed the relay rejecting every real outcome with `400 "invalid:
    /// root tag does not match thread ancestry"`.
    ///
    /// This test builds the outcome event with `buzz_sdk::build_message` +
    /// `ThreadRef { root_event_id: origin, parent_event_id: wake }` — the
    /// exact production call `crates/buzz-acp/src/delegation.rs`'s
    /// `build_outcome_event` makes — so it proves the real sidecar-shaped
    /// event is actually accepted by the real relay-shaped validator, not
    /// just that some hand-rolled event with a `buzz:delegation-outcome`
    /// tag can be inserted directly into the DB.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_outcome_settles_through_real_ingest_path() {
        let (state, pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;

        let a_hex = a_keys.public_key().to_hex();
        let b_hex = b_keys.public_key().to_hex();

        let draft = base_draft(a_hex.clone(), b_hex.clone(), 3, 10_000);
        let (origin, request) =
            post_origin_event(&state, community, channel_id, &a_keys, draft).await;

        let approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &request,
            origin.created_at.as_secs(),
        )
        .expect("build approval");

        let approve_result = ingest_event(
            &state,
            &t,
            approval_event.clone(),
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval must be accepted");
        assert!(approve_result.accepted);

        let record = load_record_retrying(&state, community, request.delegation_id).await;
        assert_eq!(record.state, "approved");

        let wake_event = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &record.run_id.to_string(),
        )
        .await
        .expect("wake event must exist");

        // Build the outcome exactly as `buzz-acp`'s `build_outcome_event`
        // does: root = origin, parent = wake, via the same shared
        // `buzz_sdk::build_message` + `ThreadRef` the sidecar calls.
        let thread_ref = buzz_sdk::ThreadRef {
            root_event_id: origin.id,
            parent_event_id: wake_event.id,
        };
        let outcome_event = buzz_sdk::build_message(
            channel_id,
            "done",
            Some(&thread_ref),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &record.run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "1234"]).unwrap(),
        ])
        .sign_with_keys(&b_keys)
        .expect("sign outcome via buzz_sdk");

        // The real ingest path: `POST /events` and the WS `EVENT` handler
        // both funnel through this same function. Before the D-L1 fix, this
        // call returned `Err(IngestError::Rejected("invalid: root tag does
        // not match thread ancestry"))`. Kind:9 requires `MessagesWrite`
        // scope over HTTP auth (unlike the operator's approval kind above,
        // which is dispatched to `handle_approval_event` before scope
        // enforcement runs) -- `http_auth`'s `scopes: vec![]` default is
        // only sufficient for that special-cased approval kind.
        let outcome_auth = IngestAuth::Http {
            pubkey: b_keys.public_key(),
            scopes: vec![buzz_auth::Scope::MessagesWrite],
            auth_method: HttpAuthMethod::Nip98,
        };
        let outcome_result = ingest_event(&state, &t, outcome_event, outcome_auth)
            .await
            .expect("outcome event must be accepted by the real ingest path");
        assert!(
            outcome_result.accepted,
            "outcome event must be accepted, not merely not-erroring"
        );

        // `settle_outcome` runs `tokio::spawn`ed off the ingest critical
        // path (see the load_record_until_state doc comment), so poll for
        // the state transition rather than asserting on the first read --
        // this is what proves settle_outcome is actually wired to the real
        // ingest path, not just independently testable.
        let record_after =
            load_record_until_state(&state, community, request.delegation_id, "delivered").await;
        assert_eq!(record_after.token_budget_remaining, 10_000 - 1234);
    }

    /// `delegation_nested_hop_and_turns` (build_spec.md Step 4.3, Risk 1: the
    /// largest test and the only automated hop-2 proof). Covers the seven
    /// bullets confirmed to have a real code path at this pin, all through
    /// the real `ingest_event` path.
    ///
    /// Deliberately NOT covered: "child `token_budget` > parent's remaining
    /// -> refused, parent unchanged." Traced the real path in full
    /// (`handle_approval_event` -> `buzz_core::delegation::claim_and_enqueue`
    /// -> `PgDelegationClaimStore::claim_and_enqueue` ->
    /// `claim_and_enqueue_tx`, `crates/buzz-db/src/store/delegation.rs`) and
    /// confirmed a child's claim never reads or writes the parent's
    /// `token_budget_remaining` at all -- a successful child claim does not
    /// decrement it, and there is no overdraw check to refuse. The only place
    /// the reservation SQL pattern exists anywhere in the tree is
    /// `child_claim_reserves_parent_budget_and_refuses_overdraw`
    /// (`crates/buzz-db/src/store/delegation.rs`), an isolated `#[ignore]`d
    /// unit test that runs the UPDATE directly against a hand-seeded fixture
    /// with no call into `claim_and_enqueue` or `handle_approval_event` --
    /// it proves the SQL pattern works, not that any production path invokes
    /// it. `design_answers_TBAC-06-slice4-delegation.md` line 29 states "a
    /// child claim reserves child.token_budget from the parent's remaining in
    /// the same transaction," so this is a genuine Slice 4 implementation gap
    /// against its own accepted design answer, not a Slice 5 spec error and
    /// not this slice's to fix (Slice 5 Non-Goals: no new capability) --
    /// recorded here as a candidate follow-up packet.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_nested_hop_and_turns() {
        let (state, pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys, c_keys) =
            setup_owner_and_three_agents(&state).await;
        let t = tenant(&state, community).await;

        let a_hex = a_keys.public_key().to_hex();
        let b_hex = b_keys.public_key().to_hex();
        let c_hex = c_keys.public_key().to_hex();

        // --- Bullet 1: A -> B approved via the real ingest path, B outcome
        // `delegated` (parent stays open awaiting a child). ------------------
        let parent_draft = base_draft(a_hex.clone(), b_hex.clone(), 3, 10_000);
        let (parent_origin, parent_request) =
            post_origin_event(&state, community, channel_id, &a_keys, parent_draft).await;
        let parent_approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &parent_request,
            parent_origin.created_at.as_secs(),
        )
        .expect("build parent approval");
        let parent_approval_event_id_hex = parent_approval_event.id.to_hex();

        let parent_approve_result = ingest_event(
            &state,
            &t,
            parent_approval_event.clone(),
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("parent approval must be accepted");
        assert!(parent_approve_result.accepted);

        let parent_record =
            load_record_retrying(&state, community, parent_request.delegation_id).await;
        assert_eq!(parent_record.state, "approved");

        let parent_wake = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &parent_record.run_id.to_string(),
        )
        .await
        .expect("parent wake must exist");

        // B posts a signed `delegated` outcome through the real ingest path
        // (kind:9 requires MessagesWrite scope over HTTP auth; the operator
        // approval kind above is exempt, dispatched before scope enforcement).
        let b_write_auth = |pubkey: nostr::PublicKey| IngestAuth::Http {
            pubkey,
            scopes: vec![buzz_auth::Scope::MessagesWrite],
            auth_method: HttpAuthMethod::Nip98,
        };
        let parent_thread_ref = buzz_sdk::ThreadRef {
            root_event_id: parent_origin.id,
            parent_event_id: parent_wake.id,
        };
        let b_delegated_outcome = buzz_sdk::build_message(
            channel_id,
            "delegating onward",
            Some(&parent_thread_ref),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &parent_record.run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delegated"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "100"]).unwrap(),
        ])
        .sign_with_keys(&b_keys)
        .expect("sign b's delegated outcome");
        let b_outcome_result = ingest_event(
            &state,
            &t,
            b_delegated_outcome,
            b_write_auth(b_keys.public_key()),
        )
        .await
        .expect("b's delegated outcome must be accepted");
        assert!(b_outcome_result.accepted);

        // A `delegated` outcome settles the action but the parent record
        // itself stays `approved` (awaiting the child) -- confirm this
        // holds after the ingest call's settlement spawn has had time to run,
        // by polling for the action to actually settle rather than the
        // record to change state (it should not).
        let mut settled_delegated = false;
        for _ in 0..50 {
            if let Ok(Some(action)) =
                state.db.find_delegation_action_by_run(community, parent_record.run_id).await
            {
                let _ = action;
            } else {
                settled_delegated = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        assert!(
            settled_delegated,
            "b's delegated outcome must settle the open action (find_delegation_action_by_run \
             filters settled_at IS NULL, so it finding nothing proves settlement ran)"
        );
        let parent_record_after_delegated = state
            .db
            .load_delegation_record(community, parent_request.delegation_id)
            .await
            .expect("load parent record")
            .expect("parent record exists");
        assert_eq!(
            parent_record_after_delegated.state, "approved",
            "a delegated outcome must leave the parent open, awaiting the child"
        );

        // --- Bullet 2: child B -> C approved (agent_path [A,B,C],
        // parent_approval_event_id = A->B approval id) is accepted. Per this
        // test's own doc comment, deliberately not asserting anything about
        // the parent's token_budget_remaining. ------------------------------
        let child_delegation_id = Uuid::new_v4();
        let child_draft = buzz_core::delegation::DelegationRequestDraft {
            delegation_id: child_delegation_id,
            origin_event_id: None,
            parent_approval_event_id: Some(parent_approval_event_id_hex.clone()),
            source_agent: b_hex.clone(),
            target_agent: c_hex.clone(),
            agent_path: vec![a_hex.clone(), b_hex.clone(), c_hex.clone()],
            hop_budget: 2,
            max_turns: 3,
            cost_cap_microusd: None,
            token_budget: 4_000,
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            // A child's expires_at must be <= its parent's own expires_at
            // (validate_parent_lineage, buzz-core/src/delegation.rs) -- must
            // not be independently computed as "now + N", since wall-clock
            // time elapses between building the parent's draft and the
            // child's, which can push a same-offset child expiry past the
            // parent's.
            expires_at: parent_request.expires_at,
        };
        let (child_origin, child_request) =
            post_origin_event(&state, community, channel_id, &b_keys, child_draft).await;
        let child_approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &child_request,
            child_origin.created_at.as_secs(),
        )
        .expect("build child approval");
        let child_approve_result = ingest_event(
            &state,
            &t,
            child_approval_event,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("child approval must be accepted");
        assert!(child_approve_result.accepted);

        let child_record = load_record_retrying(&state, community, child_delegation_id).await;
        assert_eq!(child_record.state, "approved");

        let child_wake = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &child_record.run_id.to_string(),
        )
        .await
        .expect("child wake must exist");

        // --- Bullet 3: child `delivered` -> parent continuation wake exists
        // with `buzz:delegation-child-answer`. -------------------------------
        let child_thread_ref = buzz_sdk::ThreadRef {
            root_event_id: child_origin.id,
            parent_event_id: child_wake.id,
        };
        let c_delivered_outcome = buzz_sdk::build_message(
            channel_id,
            "done",
            Some(&child_thread_ref),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &child_record.run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "500"]).unwrap(),
        ])
        .sign_with_keys(&c_keys)
        .expect("sign c's delivered outcome");
        let c_outcome_id_hex = c_delivered_outcome.id.to_hex();
        let c_outcome_result = ingest_event(
            &state,
            &t,
            c_delivered_outcome,
            b_write_auth(c_keys.public_key()),
        )
        .await
        .expect("c's delivered outcome must be accepted");
        assert!(c_outcome_result.accepted);

        let child_record_after =
            load_record_until_state(&state, community, child_delegation_id, "delivered").await;
        assert_eq!(child_record_after.token_budget_remaining, 4_000 - 500);

        // Poll for the parent's continuation wake: a second event carrying
        // the parent's own run id and the child-answer tag naming c's
        // outcome event, distinct from the first (pre-delegated) wake.
        let mut continuation_wake = None;
        for _ in 0..50 {
            if let Some(event) = find_tagged_channel_event(
                &state,
                &pool,
                community,
                channel_id,
                "buzz:delegation-child-answer",
                &c_outcome_id_hex,
            )
            .await
            {
                continuation_wake = Some(event);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let continuation_wake =
            continuation_wake.expect("parent continuation wake with buzz:delegation-child-answer must exist");
        assert!(
            has_tag_value(&continuation_wake, "buzz:delegation-run", Some(&parent_record.run_id.to_string())),
            "the continuation wake must carry the parent's own run id"
        );
        assert_ne!(
            continuation_wake.id, parent_wake.id,
            "the continuation wake must be a new event, not the original first-hop wake"
        );

        // B must settle bullet 1's parent's continuation action before bullet
        // 4 starts a second, unrelated A->B delegation: `open_action_as_target`
        // (crates/buzz-db/src/store/delegation.rs) has no way to disambiguate
        // between two different `delegation_id`s that both target B and are
        // both still open -- its `ORDER BY a.action_seq DESC LIMIT 1` compares
        // an action_seq that is only meaningful *within* a single delegation_id,
        // so with bullet 1's parent still open here it can resolve to the wrong
        // row for bullet 4's capped parent, tripping `parent_binding_mismatch`.
        // Settling it (a plain `delivered` outcome on the continuation wake) is
        // what a real agent would do once it has nothing further to delegate,
        // and it is what makes B available as a target again.
        let parent_thread_ref_2 = buzz_sdk::ThreadRef {
            root_event_id: parent_origin.id,
            parent_event_id: continuation_wake.id,
        };
        let b_final_delivered_outcome = buzz_sdk::build_message(
            channel_id,
            "done",
            Some(&parent_thread_ref_2),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &parent_record.run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "50"]).unwrap(),
        ])
        .sign_with_keys(&b_keys)
        .expect("sign b's final delivered outcome");
        ingest_event(
            &state,
            &t,
            b_final_delivered_outcome,
            b_write_auth(b_keys.public_key()),
        )
        .await
        .expect("b's final delivered outcome must be accepted");
        let _ = load_record_until_state(&state, community, parent_request.delegation_id, "delivered").await;

        // --- Bullet 4: with parent max_turns=1, the continuation is refused
        // turn_limit_exceeded: record `failed`, failure_detail='turns',
        // exactly one failure notice. -----------------------------------------
        let capped_draft = base_draft(a_hex.clone(), b_hex.clone(), 1, 10_000);
        let (capped_origin, capped_request) =
            post_origin_event(&state, community, channel_id, &a_keys, capped_draft).await;
        let capped_approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &capped_request,
            capped_origin.created_at.as_secs(),
        )
        .expect("build capped approval");
        let capped_approval_event_id_hex = capped_approval_event.id.to_hex();
        ingest_event(
            &state,
            &t,
            capped_approval_event,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("capped approval must be accepted");
        // Not `load_record_retrying`: with `max_turns=1`, after the first
        // dispatch `remaining_turns` is 0, and
        // `DelegationExecutionContext::from_approved_record` rejects
        // `remaining_turns == 0` (`TurnLimitExceeded`) -- `load_delegation_record`
        // maps that `Err` to `Ok(None)` for an `approved` row, so the row is
        // transiently unloadable through that helper even though it exists
        // (same documented pattern as `delegation_sweeper_times_out_and_retries_then_notices`'s
        // "asserted via raw SQL, not load_delegation_record" comment above).
        let mut capped_row: Option<(String, i32, Uuid)> = None;
        for _ in 0..50 {
            if let Ok(row) = sqlx::query_as::<_, (String, i32, Uuid)>(
                "SELECT state, remaining_turns, run_id FROM delegation_records \
                 WHERE community_id = $1 AND delegation_id = $2",
            )
            .bind(community.as_uuid())
            .bind(capped_request.delegation_id)
            .fetch_one(&pool)
            .await
            {
                capped_row = Some(row);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let (capped_state, capped_remaining, capped_run_id) =
            capped_row.expect("capped delegation record must become visible");
        assert_eq!(capped_state, "approved");
        assert_eq!(capped_remaining, 0, "max_turns=1 leaves zero turns after the first dispatch");

        let capped_wake = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &capped_run_id.to_string(),
        )
        .await
        .expect("capped parent wake must exist");
        let capped_thread_ref = buzz_sdk::ThreadRef {
            root_event_id: capped_origin.id,
            parent_event_id: capped_wake.id,
        };
        let capped_delegated_outcome = buzz_sdk::build_message(
            channel_id,
            "delegating onward",
            Some(&capped_thread_ref),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &capped_run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delegated"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "100"]).unwrap(),
        ])
        .sign_with_keys(&b_keys)
        .expect("sign capped b's delegated outcome");
        ingest_event(
            &state,
            &t,
            capped_delegated_outcome,
            b_write_auth(b_keys.public_key()),
        )
        .await
        .expect("capped b's delegated outcome must be accepted");

        let capped_child_delegation_id = Uuid::new_v4();
        let capped_child_draft = buzz_core::delegation::DelegationRequestDraft {
            delegation_id: capped_child_delegation_id,
            origin_event_id: None,
            parent_approval_event_id: Some(capped_approval_event_id_hex.clone()),
            source_agent: b_hex.clone(),
            target_agent: c_hex.clone(),
            agent_path: vec![a_hex.clone(), b_hex.clone(), c_hex.clone()],
            hop_budget: 2,
            max_turns: 3,
            cost_cap_microusd: None,
            token_budget: 4_000,
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            expires_at: capped_request.expires_at,
        };
        let (capped_child_origin, capped_child_request) =
            post_origin_event(&state, community, channel_id, &b_keys, capped_child_draft).await;
        let capped_child_approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &capped_child_request,
            capped_child_origin.created_at.as_secs(),
        )
        .expect("build capped child approval");
        ingest_event(
            &state,
            &t,
            capped_child_approval_event,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("capped child approval must be accepted");
        let capped_child_record =
            load_record_retrying(&state, community, capped_child_delegation_id).await;

        let capped_child_wake = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &capped_child_record.run_id.to_string(),
        )
        .await
        .expect("capped child wake must exist");
        let capped_child_thread_ref = buzz_sdk::ThreadRef {
            root_event_id: capped_child_origin.id,
            parent_event_id: capped_child_wake.id,
        };
        let capped_c_delivered_outcome = buzz_sdk::build_message(
            channel_id,
            "done",
            Some(&capped_child_thread_ref),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &capped_child_record.run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "500"]).unwrap(),
        ])
        .sign_with_keys(&c_keys)
        .expect("sign capped c's delivered outcome");
        ingest_event(
            &state,
            &t,
            capped_c_delivered_outcome,
            b_write_auth(c_keys.public_key()),
        )
        .await
        .expect("capped c's delivered outcome must be accepted");

        // Ideal behavior would be: the parent has zero remaining turns, so
        // dispatch_action's turn ceiling (dispatch.rs:140) refuses the
        // continuation attempt -- record `failed`, failure_detail='turns',
        // exactly one failure notice. That is NOT what happens.
        //
        // Asserts the transient unreachable-state defect, not the ideal
        // behavior: dispatch_next's turn-ceiling check (dispatch.rs:140) can
        // never run for a real record because load_delegation_record's
        // Err(_) => Ok(None) mapping (buzz-db/src/store/delegation.rs) already
        // hid it from dispatch_next before that check is reached. A record
        // here would need this raw-SQL escape hatch even outside a sweeper
        // retry -- recorded as a Slice 4 production gap, not fixed in this
        // slice (Non-Goals: no dispatcher behavior change).
        let mut capped_parent_stuck: Option<(String, i32)> = None;
        for _ in 0..50 {
            if let Ok(row) = sqlx::query_as::<_, (String, i32)>(
                "SELECT state, remaining_turns FROM delegation_records \
                 WHERE community_id = $1 AND delegation_id = $2",
            )
            .bind(community.as_uuid())
            .bind(capped_request.delegation_id)
            .fetch_one(&pool)
            .await
            {
                capped_parent_stuck = Some(row);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let (capped_parent_state, capped_parent_remaining) =
            capped_parent_stuck.expect("capped parent's row must exist");
        assert_eq!(
            capped_parent_state, "approved",
            "documents the defect: the record is stuck `approved`, never reaching `failed`"
        );
        assert_eq!(capped_parent_remaining, 0);
        assert_eq!(
            count_failed_notices(&pool, community, channel_id, capped_request.delegation_id).await,
            0,
            "documents the defect: dispatch_action's turn-ceiling branch never runs, so no \
             failure notice is ever posted for this delegation"
        );

        // --- Bullet 5: child with a four-entry path -> `blocked: delegation
        // refused`. ------------------------------------------------------------
        let d_keys = Keys::generate();
        let four_entry_draft = buzz_core::delegation::DelegationRequestDraft {
            delegation_id: Uuid::new_v4(),
            origin_event_id: None,
            parent_approval_event_id: Some(parent_approval_event_id_hex.clone()),
            source_agent: b_hex.clone(),
            target_agent: c_hex.clone(),
            agent_path: vec![
                a_hex.clone(),
                b_hex.clone(),
                c_hex.clone(),
                d_keys.public_key().to_hex(),
            ],
            hop_budget: 2,
            max_turns: 3,
            cost_cap_microusd: None,
            token_budget: 1_000,
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            expires_at: parent_request.expires_at,
        };
        let (four_entry_origin, four_entry_request) =
            post_origin_event(&state, community, channel_id, &b_keys, four_entry_draft).await;
        // `build_operator_approval_event` (and `DelegationApproval::for_request`,
        // and the free fn `immutable_request_hash` it calls) all run
        // `request.validate()` before ever computing a hash or building an
        // event -- so a genuinely four-entry `DelegationRequest` can never
        // produce a "properly hashed" approval at all; the client-side SDK
        // refuses to construct one. That's real, but it doesn't exercise the
        // relay's OWN ingest-time re-validation.
        //
        // It doesn't need to: `handle_approval_event` step c calls
        // `DelegationRecord::new_offered` with the request reconstructed
        // from the ORIGIN event's own draft content (`draft.into_request`),
        // independent of the approval event's own tags/content -- and
        // `new_offered` runs `record.validate()` (-> `HopBudgetExceeded`)
        // BEFORE it ever reaches the hash-comparison step. So the approval
        // event's own `immutable_request_hash` value never needs to be
        // correct for this refusal to fire; only its tag/JSON shape needs
        // to pass step a's envelope checks. Hand-build the approval with a
        // placeholder hash instead of a real one, to prove the refusal
        // comes from the relay re-deriving and re-validating the four-entry
        // path from the origin, not from a hash mismatch.
        let four_entry_approval = {
            let placeholder_hash = "0".repeat(64);
            let approval = buzz_core::delegation::DelegationApproval {
                format: buzz_core::delegation::APPROVAL_FORMAT.to_owned(),
                version: buzz_core::delegation::VERSION,
                delegation_id: four_entry_request.delegation_id,
                immutable_request_hash: placeholder_hash.clone(),
                expires_at: four_entry_request.expires_at,
            };
            let content = serde_json::to_string(&approval).expect("serialize approval");
            EventBuilder::new(Kind::Custom(buzz_core::kind::KIND_DELEGATION_APPROVAL as u16), content)
                .tags([
                    Tag::parse(["d", &four_entry_request.delegation_id.to_string()]).unwrap(),
                    Tag::parse(["e", &four_entry_request.origin_event_id]).unwrap(),
                    Tag::parse(["p", &four_entry_request.target_agent]).unwrap(),
                    Tag::parse(["request", &placeholder_hash]).unwrap(),
                    Tag::parse(["expiration", &four_entry_request.expires_at.to_string()]).unwrap(),
                ])
                .custom_created_at(nostr::Timestamp::from(four_entry_origin.created_at.as_secs()))
                .sign_with_keys(&operator_keys)
                .expect("sign four-entry approval")
        };
        let four_entry_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                four_entry_approval,
                http_auth(operator_keys.public_key()),
            )
            .await,
            "a four-entry agent_path must be refused",
        );
        assert!(matches!(
            four_entry_err,
            IngestError::Rejected(ref m) if m == super::DELEGATION_REFUSED
        ));

        // --- Bullet 6: child naming a parent with no open action -> refused.
        // A fresh, never-approved delegation id as the claimed parent. --------
        let no_open_parent_draft = buzz_core::delegation::DelegationRequestDraft {
            delegation_id: Uuid::new_v4(),
            origin_event_id: None,
            parent_approval_event_id: Some(nostr::EventId::all_zeros().to_hex()),
            source_agent: b_hex.clone(),
            target_agent: c_hex.clone(),
            agent_path: vec![a_hex.clone(), b_hex.clone(), c_hex.clone()],
            hop_budget: 2,
            max_turns: 3,
            cost_cap_microusd: None,
            token_budget: 1_000,
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            expires_at: parent_request.expires_at,
        };
        let (no_open_parent_origin, no_open_parent_request) = post_origin_event(
            &state,
            community,
            channel_id,
            &b_keys,
            no_open_parent_draft,
        )
        .await;
        let no_open_parent_approval = build_operator_approval_event(
            &operator_keys,
            community,
            &no_open_parent_request,
            no_open_parent_origin.created_at.as_secs(),
        )
        .expect("build no-open-parent approval");
        let no_open_parent_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                no_open_parent_approval,
                http_auth(operator_keys.public_key()),
            )
            .await,
            "naming a parent with no open action must be refused",
        );
        assert!(matches!(
            no_open_parent_err,
            IngestError::Rejected(ref m) if m == super::DELEGATION_REFUSED
        ));

        // --- Bullet 7: direct (root, no parent_approval_event_id) request
        // from B while B has an open action -> refused. Bullet 1's parent
        // was deliberately settled (`delivered`) above so bullet 4 could
        // start a second, unambiguous A->B delegation for B (see the
        // comment there); B's one remaining open action as target is now
        // the capped parent from bullet 4 (`state='approved'`,
        // `remaining_turns=0`, stuck per the dispatcher defect documented
        // there -- still `approved`, so still "open" by
        // `open_action_as_target`'s own definition). `open_action_as_target`
        // for B is therefore still `Some`, and a root request
        // (`parent_approval_event_id: None`) must be refused
        // `parent_binding_mismatch`. --------------------------------------------
        let b_direct_draft = base_draft(b_hex.clone(), c_hex.clone(), 3, 1_000);
        let (b_direct_origin, b_direct_request) =
            post_origin_event(&state, community, channel_id, &b_keys, b_direct_draft).await;
        let b_direct_approval = build_operator_approval_event(
            &operator_keys,
            community,
            &b_direct_request,
            b_direct_origin.created_at.as_secs(),
        )
        .expect("build b-direct approval");
        let b_direct_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                b_direct_approval,
                http_auth(operator_keys.public_key()),
            )
            .await,
            "a direct request from an agent with an open action must be refused",
        );
        assert!(matches!(
            b_direct_err,
            IngestError::Rejected(ref m) if m == super::DELEGATION_REFUSED
        ));
    }

    /// Flag off -> `restricted: unknown event kind`, over both `IngestAuth`
    /// variants — a separate `AppState` since the flag is read once at
    /// construction and never mutated on a live instance.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_flag_off_rejects_both_auth_variants() {
        let (state, _pool) = e2e_state_with_flag(false).await;
        assert!(!state.delegation_enabled);
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;
        let draft = base_draft(
            a_keys.public_key().to_hex(),
            b_keys.public_key().to_hex(),
            3,
            10_000,
        );
        let (origin, request) =
            post_origin_event(&state, community, channel_id, &a_keys, draft).await;
        let approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &request,
            origin.created_at.as_secs(),
        )
        .expect("build approval");

        let http_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                approval_event.clone(),
                http_auth(operator_keys.public_key()),
            )
            .await,
            "flag off must reject over HTTP auth",
        );
        assert!(matches!(
            http_err,
            IngestError::Rejected(ref m) if m == "restricted: unknown event kind"
        ));

        let nip42_err = expect_rejected(
            ingest_event(
                &state,
                &t,
                approval_event,
                IngestAuth::Nip42 {
                    pubkey: operator_keys.public_key(),
                    scopes: vec![],
                    channel_ids: None,
                    conn_id: Uuid::new_v4(),
                },
            )
            .await,
            "flag off must reject over Nip42 auth",
        );
        assert!(matches!(
            nip42_err,
            IngestError::Rejected(ref m) if m == "restricted: unknown event kind"
        ));
    }

    /// `delegation_tenant_route_flag_off_matches_unknown_route`: with the
    /// flag off, `/delegations/tenant` and an unknown path must produce the
    /// byte-identical 404 status — the route must be structurally absent
    /// from the router, not merely refusing at runtime (spec 3.9).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_tenant_route_flag_off_matches_unknown_route() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let (state, _pool) = e2e_state_with_flag(false).await;
        let router = crate::router::build_router(Arc::clone(&state));

        let known_path_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/delegations/tenant")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router call for /delegations/tenant");
        let unknown_path_response = router
            .oneshot(
                Request::builder()
                    .uri("/delegations/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router call for /delegations/nonexistent");

        // The spec requirement (3.9) is byte-identical treatment of the known
        // and unknown paths with the flag off — not a specific status code.
        // This router configuration's actual "no route" status for a bare
        // request (no Host header) is whatever axum's own unmatched-fallback
        // returns; asserting a hardcoded 404 here would test this test's
        // assumption about that detail, not the spec's actual requirement.
        assert_eq!(
            known_path_response.status(),
            unknown_path_response.status(),
            "flag off must make /delegations/tenant indistinguishable from an unknown route"
        );
        assert!(
            known_path_response.status().is_client_error(),
            "an absent route must not succeed; got {}",
            known_path_response.status()
        );

        // Prove the route is actually structurally conditional, not just
        // uniformly rejecting every request regardless of the flag: with the
        // flag ON, the same bare request must be treated differently (the
        // route now exists and the handler runs, even though it then refuses
        // for lack of real auth headers).
        let (state_on, _pool_on) = e2e_state_with_flag(true).await;
        let router_on = crate::router::build_router(Arc::clone(&state_on));
        let known_path_response_flag_on = router_on
            .oneshot(
                Request::builder()
                    .uri("/delegations/tenant")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router call for /delegations/tenant with flag on");
        assert_ne!(
            known_path_response_flag_on.status(),
            known_path_response.status(),
            "the route must behave differently once the flag is on, proving it was \
             structurally absent (not just refusing) when the flag was off"
        );
    }

    /// `delegation_store_unavailable_dispatches_nothing`: `dispatch_next` on
    /// a delegation id that was never claimed must find no row and dispatch
    /// zero wakes, exercising the same "give up cleanly, don't panic or
    /// dispatch" contract a genuine store fault would also need to satisfy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_store_unavailable_dispatches_nothing() {
        let (state, _pool) = e2e_state().await;
        let (community, _channel_id, _operator_keys, _a_keys, _b_keys) =
            setup_owner_and_two_agents(&state).await;

        let never_claimed_id = Uuid::new_v4();
        super::dispatch::dispatch_next(&state, community, never_claimed_id, None).await;

        let action = state
            .db
            .find_delegation_action_by_run(community, never_claimed_id)
            .await
            .ok()
            .flatten();
        assert!(
            action.is_none(),
            "dispatch_next on an unresolvable delegation must create zero actions"
        );
    }

    /// `delegation_cost_cap_refuses_every_action` [Q2]: a delegation approved
    /// with `cost_cap_microusd` set is refused `cost_unknown` on its very
    /// first dispatch (no cost oracle exists yet), settling the record as
    /// `failed` with zero wakes ever dispatched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_cost_cap_refuses_every_action() {
        let (state, pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;

        let mut draft = base_draft(
            a_keys.public_key().to_hex(),
            b_keys.public_key().to_hex(),
            3,
            10_000,
        );
        draft.cost_cap_microusd = Some(1);
        let (origin, request) =
            post_origin_event(&state, community, channel_id, &a_keys, draft).await;
        let approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &request,
            origin.created_at.as_secs(),
        )
        .expect("build approval");

        ingest_event(
            &state,
            &t,
            approval_event,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval with a cost cap must still be accepted");

        let record = load_record_retrying(&state, community, request.delegation_id).await;
        assert_eq!(
            record.state, "failed",
            "a cost-capped delegation's first dispatch must refuse and fail the record"
        );

        // Search by `buzz:workflow-mention`, not `buzz:delegation` — the
        // summary notice (posted for every approved claim, cost-capped or
        // not) also carries `buzz:delegation`, so that tag alone can't tell
        // "no wake" from "notice only".
        let wake = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:workflow-mention",
            &b_keys.public_key().to_hex(),
        )
        .await;
        assert!(wake.is_none(), "a cost-capped delegation must dispatch zero wakes");

        let failure_notice = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-notice",
            "failed",
        )
        .await;
        assert!(failure_notice.is_some(), "the cost_unknown refusal must post a failure notice");
    }

    /// `delegation_budget_exhaustion_fails_budget`: a delegation whose target
    /// posts a `delivered` outcome that consumes its entire remaining token
    /// budget on the first turn leaves nothing for a continuation; the next
    /// `dispatch_next` (as a nested hop would trigger) must refuse
    /// `budget_exceeded` rather than dispatch with a negative budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_budget_exhaustion_fails_budget() {
        let (state, _pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;

        let draft = base_draft(
            a_keys.public_key().to_hex(),
            b_keys.public_key().to_hex(),
            5,
            1_000,
        );
        let (origin, request) =
            post_origin_event(&state, community, channel_id, &a_keys, draft).await;
        let approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &request,
            origin.created_at.as_secs(),
        )
        .expect("build approval");
        ingest_event(
            &state,
            &t,
            approval_event,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval must be accepted");

        let record = load_record_retrying(&state, community, request.delegation_id).await;

        // B "delegates" (outcome word `delegated`) while consuming the whole
        // budget, forcing the settlement's continuation attempt to hit an
        // exhausted budget on the very next dispatch.
        let outcome_event = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), "handing off")
            .tags([
                Tag::parse(["h", &channel_id.to_string()]).unwrap(),
                Tag::parse(["buzz:delegation-run", &record.run_id.to_string()]).unwrap(),
                Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
                Tag::parse(["buzz:delegation-tokens", "1000"]).unwrap(),
            ])
            .sign_with_keys(&b_keys)
            .expect("sign outcome");
        let (stored_outcome, _) =
            store_signed_event(&state, community, channel_id, &outcome_event).await;
        super::settle_outcome(Arc::clone(&state), t.clone(), record.run_id, stored_outcome).await;

        let record_after = state
            .db
            .load_delegation_record(community, request.delegation_id)
            .await
            .expect("load record after settle")
            .expect("record exists after settle");
        assert_eq!(record_after.token_budget_remaining, 0);
        assert_eq!(record_after.state, "delivered");

        // A delivered (terminal) delegation has no `approved` row left to
        // redispatch — `dispatch_next` must be a no-op, never resurrecting a
        // terminal record or dispatching against a zero budget. The one
        // action is already settled at this point (outcome "delivered"), so
        // `find_delegation_action_by_run` (which filters `settled_at IS
        // NULL`) correctly returns `None` for it now — compare
        // `latest_action_seq` from `load_delegation_record` instead, which
        // has no such filter.
        let action_before = record_after.latest_action_seq;
        super::dispatch::dispatch_next(&state, community, request.delegation_id, None).await;
        let action_after = state
            .db
            .load_delegation_record(community, request.delegation_id)
            .await
            .expect("load record")
            .expect("record exists")
            .latest_action_seq;
        assert_eq!(
            action_before, action_after,
            "dispatch_next on a terminal (delivered) record must not dispatch a new action"
        );
    }

    /// [AC-15] Sweeper: a first timeout retries (turn consumed), the last
    /// timeout fails `timeout` with exactly one notice, and an `approved`
    /// record past `expires_at` expires with exactly one notice — and no
    /// subsequent sweeper pass over the same record ever posts a second
    /// `buzz:delegation-notice=failed` event (I-7/I-15). This is the
    /// regression test for the notice-id write-back: before it,
    /// `record_failure_notice` had zero production callers, so
    /// `settle_action`'s `notice_due` guard (`failure_notice_event_id IS
    /// NULL`) re-evaluated true on every later settle of the same record.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres and Redis"]
    async fn delegation_sweeper_times_out_and_retries_then_notices() {
        let (state, pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;

        // --- Leg 1: timeout -> retry -> timeout -> failed, one notice ------
        let draft_a = base_draft(
            a_keys.public_key().to_hex(),
            b_keys.public_key().to_hex(),
            2,
            10_000,
        );
        let (origin_a, request_a) =
            post_origin_event(&state, community, channel_id, &a_keys, draft_a).await;
        let approval_a = build_operator_approval_event(
            &operator_keys,
            community,
            &request_a,
            origin_a.created_at.as_secs(),
        )
        .expect("build approval a");
        ingest_event(
            &state,
            &t,
            approval_a,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval a must be accepted");
        let record_a = load_record_retrying(&state, community, request_a.delegation_id).await;
        assert_eq!(record_a.state, "approved");
        assert_eq!(record_a.remaining_turns, 1);
        assert_eq!(record_a.latest_action_seq, 1);

        // First timeout: action 1 aged past the 1800s deadline. One turn
        // remains and the record is not past expiry, so the sweeper must
        // RETRY — state stays approved, a second action is dispatched — and
        // post no failure notice.
        //
        // Asserted via raw SQL, not `load_delegation_record`: an approved
        // record with `remaining_turns = 0` (all turns consumed, last action
        // still open) is transiently unloadable —
        // `DelegationExecutionContext::from_approved_record` rejects
        // `remaining_turns == 0` (`TurnLimitExceeded`) and the loader maps
        // that to `Ok(None)`. Terminal states (`failed`/`expired`) carry no
        // context and load fine, so the loader is still used for those.
        backdate_open_actions(&pool, community, request_a.delegation_id).await;
        super::sweeper::sweep_once(&state).await;
        let (state_a, remaining_a): (String, i32) = sqlx::query_as(
            "SELECT state, remaining_turns FROM delegation_records \
             WHERE community_id = $1 AND delegation_id = $2",
        )
        .bind(community.as_uuid())
        .bind(request_a.delegation_id)
        .fetch_one(&pool)
        .await
        .expect("read a after first sweep");
        assert_eq!(
            state_a, "approved",
            "a first timeout with a turn remaining must retry, not fail"
        );
        assert_eq!(remaining_a, 0, "the retry consumes the last turn");
        let action_seqs: Vec<(i32, Option<String>)> = sqlx::query_as(
            "SELECT action_seq, outcome FROM delegation_actions \
             WHERE community_id = $1 AND delegation_id = $2 ORDER BY action_seq",
        )
        .bind(community.as_uuid())
        .bind(request_a.delegation_id)
        .fetch_all(&pool)
        .await
        .expect("list a's actions");
        assert_eq!(
            action_seqs.len(),
            2,
            "the retry dispatches action_seq 2 (action 1 settled timeout, action 2 open)"
        );
        assert_eq!(action_seqs[0].1.as_deref(), Some("timeout"));
        assert_eq!(action_seqs[1].1, None);
        assert_eq!(
            count_failed_notices(&pool, community, channel_id, request_a.delegation_id).await,
            0,
            "a retried timeout must not post a failure notice"
        );

        // Last timeout: no turns remain -> failed(timeout) + exactly one
        // failure notice.
        backdate_open_actions(&pool, community, request_a.delegation_id).await;
        super::sweeper::sweep_once(&state).await;
        let record_a = state
            .db
            .load_delegation_record(community, request_a.delegation_id)
            .await
            .expect("load a after second sweep")
            .expect("a exists");
        assert_eq!(record_a.state, "failed");
        assert_eq!(
            count_failed_notices(&pool, community, channel_id, request_a.delegation_id).await,
            1,
            "the final timeout must post exactly one failed notice"
        );

        // A subsequent sweeper pass over the same failed record must not
        // post a second notice.
        super::sweeper::sweep_once(&state).await;
        assert_eq!(
            count_failed_notices(&pool, community, channel_id, request_a.delegation_id).await,
            1,
            "a later sweeper pass must never duplicate the failed notice (I-7/I-15)"
        );

        // --- Leg 2: approved past expires_at -> expired, one notice --------
        let draft_b = base_draft(
            a_keys.public_key().to_hex(),
            b_keys.public_key().to_hex(),
            2,
            10_000,
        );
        let (origin_b, request_b) =
            post_origin_event(&state, community, channel_id, &a_keys, draft_b).await;
        let approval_b = build_operator_approval_event(
            &operator_keys,
            community,
            &request_b,
            origin_b.created_at.as_secs(),
        )
        .expect("build approval b");
        ingest_event(
            &state,
            &t,
            approval_b,
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval b must be accepted");
        let record_b = load_record_retrying(&state, community, request_b.delegation_id).await;
        assert_eq!(record_b.state, "approved");

        // Force the record past its signed expiry WITHOUT aging its open
        // action: `expire_records` expires the record and posts the expired
        // notice while action_seq 1 is still open and fresh.
        sqlx::query(
            "UPDATE delegation_records SET expires_at = NOW() - INTERVAL '1 minute' \
             WHERE community_id = $1 AND delegation_id = $2",
        )
        .bind(community.as_uuid())
        .bind(request_b.delegation_id)
        .execute(&pool)
        .await
        .expect("force record b past expiry");
        super::sweeper::sweep_once(&state).await;
        let record_b = state
            .db
            .load_delegation_record(community, request_b.delegation_id)
            .await
            .expect("load b after expiry sweep")
            .expect("b exists");
        assert_eq!(record_b.state, "expired");
        assert_eq!(
            count_failed_notices(&pool, community, channel_id, request_b.delegation_id).await,
            1,
            "expiry must post exactly one failed notice"
        );
        let recorded_notice: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT failure_notice_event_id FROM delegation_records \
             WHERE community_id = $1 AND delegation_id = $2",
        )
        .bind(community.as_uuid())
        .bind(request_b.delegation_id)
        .fetch_one(&pool)
        .await
        .expect("read failure_notice_event_id");
        assert!(
            recorded_notice.is_some(),
            "the failure notice id must be durably written back (the I-7/I-15 guard)"
        );

        // Regression leg: the still-open action 1 (expire_records does not
        // settle open actions) now ages past the deadline, so the NEXT sweep
        // settles it `timeout` against the already-expired record. Before
        // the write-back this posted a SECOND failed notice — notice_due
        // re-evaluated true because failure_notice_event_id was never
        // persisted. With the write-back the guard holds.
        backdate_open_actions(&pool, community, request_b.delegation_id).await;
        super::sweeper::sweep_once(&state).await;
        assert_eq!(
            count_failed_notices(&pool, community, channel_id, request_b.delegation_id).await,
            1,
            "settling a stale open action after expiry must not duplicate the failed notice (I-7/I-15)"
        );
    }

    /// Captures every `tracing` line emitted while `subscriber` (built with
    /// this writer) is the default, into a shared buffer readable after the
    /// fact. Mirrors `crates/buzz-relay/src/config.rs`'s
    /// `config_with_admin_env_capturing_logs`'s `CapturingMakeWriter` --
    /// zero new dependency, `tracing-subscriber` is already a regular
    /// `buzz-relay` dependency.
    #[derive(Clone)]
    struct CapturingMakeWriter {
        buf: Arc<std::sync::Mutex<Vec<u8>>>,
    }
    struct CapturingWriter {
        buf: Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl std::io::Write for CapturingWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingMakeWriter {
        type Writer = CapturingWriter;
        fn make_writer(&'a self) -> Self::Writer {
            CapturingWriter {
                buf: Arc::clone(&self.buf),
            }
        }
    }

    /// I-5 / Step 3.4: no `buzz_*` metric label and no `buzz_relay::delegation`
    /// / routine audit line may carry message content. Seeds a sentinel into
    /// a routine outcome event's content, a delegation origin body and an
    /// agent reply, drives both through the real `ingest_event` path, and
    /// asserts the sentinel is absent from every captured metric label and
    /// log line -- plus that the routine counter incremented exactly once
    /// with `outcome=succeeded` (I-6, AC-7).
    ///
    /// `flavor = "multi_thread", worker_threads = 1`, deliberately unlike
    /// this module's other e2e tests (which use `worker_threads = 2`):
    /// `settle_outcome` runs `tokio::spawn`ed off the ingest critical path
    /// (see `load_record_until_state`'s doc comment), and both
    /// `tracing::subscriber::set_default` and
    /// `metrics::set_default_local_recorder` install a *thread-local*
    /// guard that a spawned task silently escapes if it lands on a
    /// different worker thread -- `current_thread` would guarantee
    /// same-thread execution but panics on `handle_approval_event`'s
    /// `tokio::task::spawn_blocking` (Schnorr verification is CPU-bound and
    /// requires a real multi-thread runtime). A `multi_thread` runtime with
    /// exactly one worker thread satisfies both constraints at once: every
    /// spawned/awaited task runs on that one thread, and `spawn_blocking`
    /// has its own dedicated blocking pool regardless of worker count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[ignore = "requires Postgres and Redis"]
    async fn metrics_and_audit_lines_carry_no_bodies() {
        const SENTINEL: &str = "SENTINEL-5f3a9c21";

        let (state, pool) = e2e_state().await;
        let (community, channel_id, operator_keys, a_keys, b_keys) =
            setup_owner_and_two_agents(&state).await;
        let t = tenant(&state, community).await;

        let a_hex = a_keys.public_key().to_hex();
        let b_hex = b_keys.public_key().to_hex();

        // Delegation leg: sentinel in the origin body (surrounding the
        // fenced block, exactly where a real drafting agent's free text
        // would carry message content) and in the agent's reply content.
        let draft = base_draft(a_hex.clone(), b_hex.clone(), 3, 10_000);
        let origin_content = format!(
            "delegating this task -- context: {SENTINEL}\n\n```buzz-delegation\n{}\n```\n",
            serde_json::to_string(&draft).unwrap()
        );
        let origin_event = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), origin_content)
            .tags([Tag::parse(["h", &channel_id.to_string()]).unwrap()])
            .sign_with_keys(&a_keys)
            .expect("sign origin event");
        {
            let event_id_bytes = origin_event.id.as_bytes().to_vec();
            let event_created_at =
                chrono::DateTime::from_timestamp(origin_event.created_at.as_secs() as i64, 0)
                    .unwrap_or_else(Utc::now);
            let thread_meta = Some(buzz_db::event::ThreadMetadataParams {
                event_id: &event_id_bytes,
                event_created_at,
                channel_id,
                parent_event_id: None,
                parent_event_created_at: None,
                root_event_id: None,
                root_event_created_at: None,
                depth: 0,
                broadcast: false,
            });
            state
                .db
                .insert_event_with_thread_metadata(community, &origin_event, Some(channel_id), thread_meta)
                .await
                .expect("insert origin event");
        }
        let request = draft.into_request(origin_event.id.to_hex());

        let approval_event = build_operator_approval_event(
            &operator_keys,
            community,
            &request,
            origin_event.created_at.as_secs(),
        )
        .expect("build approval");

        // Routine leg: a hand-built outcome event (mirrors
        // `command_executor.rs`'s `routine_e2e_tests::routine_end_to_end_ingest_fire_settle`
        // shape) carrying the sentinel in its content -- "a routine prompt"
        // for the purpose of this negative test, since standing up a full
        // workflow/cron/dispatch cycle is unnecessary: the counter fires
        // from `ingest_event_inner` on any accepted kind:9 with the two
        // frozen tags, independent of whether a live dispatch row exists.
        let routine_run_id = Uuid::new_v4();
        let routine_outcome_event = EventBuilder::new(
            Kind::Custom(KIND_STREAM_MESSAGE as u16),
            format!("routine run completed -- prompt was: {SENTINEL}"),
        )
        .tags([
            Tag::parse(["h", &channel_id.to_string()]).unwrap(),
            Tag::parse(["buzz:routine-run", &routine_run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:routine-outcome", "succeeded"]).unwrap(),
        ])
        .sign_with_keys(&a_keys)
        .expect("sign routine outcome");
        let routine_auth = IngestAuth::Http {
            pubkey: a_keys.public_key(),
            scopes: vec![buzz_auth::Scope::MessagesWrite],
            auth_method: HttpAuthMethod::Nip98,
        };

        // Capture both the metrics recorder and tracing output around the
        // entire drive-and-settle sequence.
        let recorder = metrics_util::debugging::DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let log_buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let tracing_subscriber = tracing_subscriber::fmt()
            .with_writer(CapturingMakeWriter {
                buf: Arc::clone(&log_buf),
            })
            .with_ansi(false)
            .finish();
        let _tracing_guard = tracing::subscriber::set_default(tracing_subscriber);
        // `with_local_recorder` only wraps a synchronous closure; the drive
        // sequence below is async and spans a `tokio::spawn`ed settlement
        // task, so install the recorder via its guard-returning form
        // instead (`set_default_local_recorder`, both thread-local, both
        // covered by `flavor = "current_thread"` for the same reason as
        // the tracing guard above).
        let _recorder_guard = metrics::set_default_local_recorder(&recorder);

        let approve_result = ingest_event(
            &state,
            &t,
            approval_event.clone(),
            http_auth(operator_keys.public_key()),
        )
        .await
        .expect("approval must be accepted");
        assert!(approve_result.accepted);

        let record = load_record_retrying(&state, community, request.delegation_id).await;
        assert_eq!(record.state, "approved");

        let wake_event = find_tagged_channel_event(
            &state,
            &pool,
            community,
            channel_id,
            "buzz:delegation-run",
            &record.run_id.to_string(),
        )
        .await
        .expect("wake event must exist");

        let thread_ref = buzz_sdk::ThreadRef {
            root_event_id: origin_event.id,
            parent_event_id: wake_event.id,
        };
        let delegation_outcome_event = buzz_sdk::build_message(
            channel_id,
            &format!("done -- final answer: {SENTINEL}"),
            Some(&thread_ref),
            &[],
            false,
            &[],
            &[],
        )
        .expect("build_message")
        .tags([
            Tag::parse(["buzz:delegation-run", &record.run_id.to_string()]).unwrap(),
            Tag::parse(["buzz:delegation-outcome", "delivered"]).unwrap(),
            Tag::parse(["buzz:delegation-tokens", "1234"]).unwrap(),
        ])
        .sign_with_keys(&b_keys)
        .expect("sign delegation outcome");
        let delegation_outcome_auth = IngestAuth::Http {
            pubkey: b_keys.public_key(),
            scopes: vec![buzz_auth::Scope::MessagesWrite],
            auth_method: HttpAuthMethod::Nip98,
        };
        let delegation_outcome_result =
            ingest_event(&state, &t, delegation_outcome_event, delegation_outcome_auth)
                .await
                .expect("delegation outcome must be accepted");
        assert!(delegation_outcome_result.accepted);

        let routine_result = ingest_event(&state, &t, routine_outcome_event, routine_auth)
            .await
            .expect("routine outcome must be accepted");
        assert!(routine_result.accepted);

        // Both settlements happen off the ingest critical path; poll for
        // the delegation side (the routine counter increments synchronously
        // inside `ingest_event_inner`, before this call even returns, so no
        // poll is needed for it).
        let delegation_record =
            load_record_until_state(&state, community, request.delegation_id, "delivered").await;
        assert_eq!(delegation_record.token_budget_remaining, 10_000 - 1234);

        drop(_recorder_guard);
        drop(_tracing_guard);

        // Assert: no metric label anywhere contains the sentinel.
        let snapshot = snapshotter.snapshot().into_vec();
        for (key, ..) in &snapshot {
            for label in key.key().labels() {
                assert!(
                    !label.value().contains(SENTINEL),
                    "metric {:?} label {}={:?} must not contain the sentinel",
                    key.key().name(),
                    label.key(),
                    label.value()
                );
            }
        }

        // Assert: the routine counter incremented exactly once with
        // outcome=succeeded (I-6, AC-7).
        let routine_counter_value = snapshot
            .iter()
            .find_map(|(key, _, _, value)| {
                if key.key().name() != "buzz_routine_outcomes_total" {
                    return None;
                }
                let outcome = key
                    .key()
                    .labels()
                    .find(|l| l.key() == "outcome")
                    .map(|l| l.value().to_owned())?;
                if outcome != "succeeded" {
                    return None;
                }
                let metrics_util::debugging::DebugValue::Counter(n) = value else {
                    panic!("buzz_routine_outcomes_total must be a counter");
                };
                Some(*n)
            })
            .expect("buzz_routine_outcomes_total{outcome=succeeded} must be present");
        assert_eq!(
            routine_counter_value, 1,
            "the routine counter must increment exactly once for this one settled outcome"
        );

        // Assert: no captured tracing/audit line contains the sentinel.
        let captured_logs = String::from_utf8(log_buf.lock().unwrap().clone()).unwrap_or_default();
        assert!(
            !captured_logs.contains(SENTINEL),
            "no buzz_relay::delegation / routine audit line may contain message content: \
             captured logs: {captured_logs:?}"
        );
    }
}
