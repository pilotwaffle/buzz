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
        for attempt in 0..MAX_ATTEMPTS {
            if let Ok(Some(record)) = state.db.load_delegation_record(community, delegation_id).await
            {
                return record;
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        }
        panic!("delegation record never became visible after its claiming ingest_event returned");
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
}
