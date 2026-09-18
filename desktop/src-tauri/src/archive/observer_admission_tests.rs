//! Privacy boundaries exercised against real SQLite rows and archive commits.

use super::pipeline::{commit_archive, plan_archive};
use super::{store, ArchiveCandidate, MatchedScope, ScopeType};
use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag};
use rusqlite::{params, Connection};

const RELAY: &str = "wss://archive-test.invalid";

fn memory_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    conn
}

fn ids(conn: &Connection, identity: &str, relay: &str) -> Vec<String> {
    conn.prepare(
        "SELECT id FROM archived_events WHERE identity_pubkey=?1 AND relay_url=?2 ORDER BY id",
    )
    .unwrap()
    .query_map(params![identity, relay], |row| row.get(0))
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn observer_candidate(owner: &Keys, agent: &Keys, total_bytes: Option<usize>) -> ArchiveCandidate {
    let owner_pk = owner.public_key().to_hex();
    let agent_pk = agent.public_key().to_hex();
    let event = EventBuilder::new(Kind::Custom(24200), "encrypted-test-payload")
        .tags([
            Tag::parse(["p", &owner_pk]).unwrap(),
            Tag::parse(["agent", &agent_pk]).unwrap(),
            Tag::parse(["frame", "telemetry"]).unwrap(),
        ])
        .sign_with_keys(agent)
        .unwrap();
    let mut raw = event.as_json();
    if let Some(total_bytes) = total_bytes {
        assert!(raw.len() <= total_bytes);
        raw.push_str(&" ".repeat(total_bytes - raw.len()));
        assert_eq!(raw.len(), total_bytes);
    }
    ArchiveCandidate {
        raw_event_json: raw,
        matched_scope: MatchedScope {
            scope_type: ScopeType::OwnerP,
            scope_value: owner_pk,
        },
    }
}

#[test]
fn observer_event_exact_256kib_is_retained_and_plus_one_is_rejected() {
    for (size, expected_persisted) in [(256 * 1024, 1), (256 * 1024 + 1, 0)] {
        let conn = memory_db();
        let owner = Keys::generate();
        let owner_pk = owner.public_key().to_hex();
        store::merge_owner_p_kinds(&conn, &owner_pk, RELAY, &owner_pk, 24200, 0).unwrap();
        let candidate = observer_candidate(&owner, &Keys::generate(), Some(size));
        let plan = plan_archive(vec![candidate], &owner_pk, RELAY, &conn).unwrap();
        let result = commit_archive(
            vec![],
            plan.ephemeral,
            plan.pre_dropped,
            &owner_pk,
            RELAY,
            &owner,
            1000,
            &conn,
        )
        .unwrap();
        assert_eq!(result.persisted, expected_persisted, "frame bytes={size}");
        assert_eq!(result.dropped, 1 - expected_persisted, "frame bytes={size}");
        assert_eq!(
            ids(&conn, &owner_pk, RELAY).len(),
            expected_persisted as usize
        );
    }
}

#[test]
fn off_from_second_connection_before_commit_drops_buffered_observer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let reader = store::open_archive_db(&path).unwrap();
    let settings = store::open_archive_db(&path).unwrap();
    let owner = Keys::generate();
    let owner_pk = owner.public_key().to_hex();
    store::merge_owner_p_kinds(&settings, &owner_pk, RELAY, &owner_pk, 24200, 0).unwrap();
    let plan = plan_archive(
        vec![observer_candidate(&owner, &Keys::generate(), None)],
        &owner_pk,
        RELAY,
        &reader,
    )
    .unwrap();
    assert_eq!(plan.ephemeral.len(), 1, "frame must already be buffered");
    store::delete_save_subscription(&settings, &owner_pk, RELAY, "owner_p", &owner_pk).unwrap();
    let result = commit_archive(
        vec![],
        plan.ephemeral,
        plan.pre_dropped,
        &owner_pk,
        RELAY,
        &owner,
        1000,
        &reader,
    )
    .unwrap();
    assert_eq!((result.persisted, result.dropped), (0, 1));
    assert!(ids(&reader, &owner_pk, RELAY).is_empty());
}

#[test]
fn malformed_subscription_kinds_fail_closed_during_commit() {
    for malformed in [
        "not-json",
        "[24200,\"44200\"]",
        "{\"24200\":true}",
        "[24200,999999]",
    ] {
        let conn = memory_db();
        let owner = Keys::generate();
        let owner_pk = owner.public_key().to_hex();
        store::upsert_save_subscription(
            &conn, &owner_pk, RELAY, "owner_p", &owner_pk, malformed, 0,
        )
        .unwrap();
        let plan = plan_archive(
            vec![observer_candidate(&owner, &Keys::generate(), None)],
            &owner_pk,
            RELAY,
            &conn,
        )
        .unwrap();
        let result = commit_archive(
            vec![],
            plan.ephemeral,
            plan.pre_dropped,
            &owner_pk,
            RELAY,
            &owner,
            1000,
            &conn,
        )
        .unwrap();
        assert_eq!((result.persisted, result.dropped), (0, 1), "{malformed}");
        assert!(ids(&conn, &owner_pk, RELAY).is_empty());
    }
}

#[test]
fn consent_for_another_scope_identity_or_relay_cannot_authorize_observer() {
    for (identity, relay, scope_type, scope_value) in [
        ("other-owner", RELAY, "owner_p", "SELF"),
        ("SELF", "other-relay", "owner_p", "SELF"),
        ("SELF", RELAY, "owner_p", "other-owner"),
        ("SELF", RELAY, "channel_h", "SELF"),
    ] {
        let conn = memory_db();
        let owner = Keys::generate();
        let owner_pk = owner.public_key().to_hex();
        let resolve = |value| {
            if value == "SELF" {
                owner_pk.as_str()
            } else {
                value
            }
        };
        store::upsert_save_subscription(
            &conn,
            resolve(identity),
            relay,
            scope_type,
            resolve(scope_value),
            "[24200]",
            0,
        )
        .unwrap();
        let plan = plan_archive(
            vec![observer_candidate(&owner, &Keys::generate(), None)],
            &owner_pk,
            RELAY,
            &conn,
        )
        .unwrap();
        let result = commit_archive(
            vec![],
            plan.ephemeral,
            plan.pre_dropped,
            &owner_pk,
            RELAY,
            &owner,
            1000,
            &conn,
        )
        .unwrap();
        assert_eq!((result.persisted, result.dropped), (0, 1));
    }
}

fn stored_frame(conn: &Connection, identity: &str, relay: &str, id: &str, kind: i64, raw: &str) {
    store::upsert_archived_event(conn, identity, relay, id, kind, "agent", 0, raw, 0).unwrap();
    store::upsert_event_scope(conn, identity, relay, id, "owner_p", identity, 0).unwrap();
}

#[test]
fn count_admission_accepts_exact_limit_then_blocks_only_new_ids() {
    let conn = memory_db();
    stored_frame(&conn, "owner", RELAY, "a", 24200, "{}");
    assert!(store::observer_admission_with_limits(&conn, "owner", RELAY, "b", 2, 2, 100).unwrap());
    stored_frame(&conn, "owner", RELAY, "b", 24200, "{}");
    assert!(!store::observer_admission_with_limits(&conn, "owner", RELAY, "c", 2, 2, 100).unwrap());
    assert!(store::observer_admission_with_limits(&conn, "owner", RELAY, "a", 2, 2, 100).unwrap());
    assert_eq!(
        ids(&conn, "owner", RELAY),
        ["a", "b"],
        "admission never deletes history"
    );
}

#[test]
fn byte_admission_uses_utf8_bytes_and_preserves_existing_rows_at_capacity() {
    let conn = memory_db();
    let unicode = "\"\u{e9}\"";
    assert_eq!(unicode.len(), 4);
    assert_eq!(unicode.chars().count(), 3);
    stored_frame(&conn, "owner", RELAY, "a", 24200, unicode);
    assert!(store::observer_admission_with_limits(&conn, "owner", RELAY, "b", 4, 10, 8).unwrap());
    assert!(!store::observer_admission_with_limits(&conn, "owner", RELAY, "b", 5, 10, 8).unwrap());
    stored_frame(&conn, "owner", RELAY, "b", 24200, unicode);
    assert!(!store::observer_admission_with_limits(&conn, "owner", RELAY, "c", 1, 10, 8).unwrap());
    assert!(store::observer_admission_with_limits(&conn, "owner", RELAY, "a", 4, 10, 8).unwrap());
    assert_eq!(ids(&conn, "owner", RELAY), ["a", "b"]);
}

#[test]
fn admission_totals_exclude_other_identity_relay_and_metric_kind() {
    let conn = memory_db();
    stored_frame(&conn, "other-owner", RELAY, "a", 24200, &"x".repeat(100));
    stored_frame(&conn, "owner", "other-relay", "b", 24200, &"x".repeat(100));
    stored_frame(&conn, "owner", RELAY, "metric", 44200, &"x".repeat(100));
    assert!(store::observer_admission_with_limits(&conn, "owner", RELAY, "new", 2, 1, 2).unwrap());
    assert_eq!(ids(&conn, "owner", RELAY), ["metric"]);
    assert_eq!(ids(&conn, "other-owner", RELAY), ["a"]);
    assert_eq!(ids(&conn, "owner", "other-relay"), ["b"]);
}

#[test]
fn reopening_preserves_old_history_and_mixed_observer_metric_consent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    {
        let conn = store::open_archive_db(&path).unwrap();
        stored_frame(&conn, "owner", RELAY, "history", 24200, "{}");
        stored_frame(&conn, "owner", RELAY, "metric", 44200, "{}");
        store::upsert_save_subscription(
            &conn,
            "owner",
            RELAY,
            "owner_p",
            "owner",
            "[24200,44200]",
            0,
        )
        .unwrap();
    }
    let conn = store::open_archive_db(&path).unwrap();
    assert_eq!(ids(&conn, "owner", RELAY), ["history", "metric"]);
    assert_eq!(
        store::get_subscription_kinds(&conn, "owner", RELAY, "owner_p", "owner")
            .unwrap()
            .as_deref(),
        Some("[24200,44200]")
    );
}

#[test]
fn observer_scope_failure_rolls_back_event_and_index_rows() {
    let conn = memory_db();
    let owner = Keys::generate();
    let owner_pk = owner.public_key().to_hex();
    store::merge_owner_p_kinds(&conn, &owner_pk, RELAY, &owner_pk, 24200, 0).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_scope_insert BEFORE INSERT ON archived_event_scopes BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    let plan = plan_archive(
        vec![observer_candidate(&owner, &Keys::generate(), None)],
        &owner_pk,
        RELAY,
        &conn,
    )
    .unwrap();
    assert!(commit_archive(
        vec![],
        plan.ephemeral,
        plan.pre_dropped,
        &owner_pk,
        RELAY,
        &owner,
        1000,
        &conn
    )
    .is_err());
    assert!(ids(&conn, &owner_pk, RELAY).is_empty());
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observer_channel_index", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn batch_admission_counts_earlier_frames_in_same_transaction() {
    let conn = memory_db();
    let owner = Keys::generate();
    let owner_pk = owner.public_key().to_hex();
    store::merge_owner_p_kinds(&conn, &owner_pk, RELAY, &owner_pk, 24200, 0).unwrap();
    conn.execute(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<9999)
         INSERT INTO archived_events SELECT ?1,?2,CAST(x AS TEXT),24200,'agent',0,'{}',0 FROM n",
        params![owner_pk, RELAY],
    )
    .unwrap();
    let candidates = vec![
        observer_candidate(&owner, &Keys::generate(), None),
        observer_candidate(&owner, &Keys::generate(), None),
    ];
    let plan = plan_archive(candidates, &owner_pk, RELAY, &conn).unwrap();
    let result = commit_archive(
        vec![],
        plan.ephemeral,
        plan.pre_dropped,
        &owner_pk,
        RELAY,
        &owner,
        1000,
        &conn,
    )
    .unwrap();
    assert_eq!((result.persisted, result.dropped), (1, 1));
    assert_eq!(ids(&conn, &owner_pk, RELAY).len(), 10_000);
}

#[test]
fn consent_is_rechecked_after_waiting_for_an_inflight_off_transaction() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    static WRITER_WAITING: AtomicBool = AtomicBool::new(false);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let settings = store::open_archive_db(&path).unwrap();
    let owner = Keys::generate();
    let owner_pk = owner.public_key().to_hex();
    store::merge_owner_p_kinds(&settings, &owner_pk, RELAY, &owner_pk, 24200, 0).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    WRITER_WAITING.store(false, Ordering::SeqCst);
    let worker = std::thread::spawn(move || {
        let conn = store::open_archive_db(&path).unwrap();
        conn.busy_handler(Some(|attempt| {
            WRITER_WAITING.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1));
            attempt < 5000
        }))
        .unwrap();
        let plan = plan_archive(
            vec![observer_candidate(&owner, &Keys::generate(), None)],
            &owner_pk,
            RELAY,
            &conn,
        )
        .unwrap();
        ready_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        commit_archive(
            vec![],
            plan.ephemeral,
            plan.pre_dropped,
            &owner_pk,
            RELAY,
            &owner,
            1000,
            &conn,
        )
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let tx =
        rusqlite::Transaction::new_unchecked(&settings, rusqlite::TransactionBehavior::Immediate)
            .unwrap();
    tx.execute("UPDATE save_subscriptions SET kinds='[]'", [])
        .unwrap();
    go_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !WRITER_WAITING.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let observed_wait = WRITER_WAITING.load(Ordering::SeqCst);
    tx.commit().unwrap();
    let result = worker.join().unwrap().unwrap();
    assert!(
        observed_wait,
        "fixture must put archive commit behind pending OFF write"
    );
    assert_eq!((result.persisted, result.dropped), (0, 1));
    let count: i64 = settings
        .query_row("SELECT COUNT(*) FROM archived_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn consent_read_failure_aborts_mixed_persistent_and_observer_batch() {
    let conn = memory_db();
    let owner = Keys::generate();
    let owner_pk = owner.public_key().to_hex();
    store::merge_owner_p_kinds(&conn, &owner_pk, RELAY, &owner_pk, 24200, 0).unwrap();
    store::upsert_save_subscription(&conn, &owner_pk, RELAY, "channel_h", "channel", "[1]", 0)
        .unwrap();
    let persistent = EventBuilder::new(Kind::Custom(1), "ordinary message")
        .sign_with_keys(&owner)
        .unwrap();
    let persistent_id = persistent.id.to_hex();
    let plan = plan_archive(
        vec![
            observer_candidate(&owner, &Keys::generate(), None),
            ArchiveCandidate {
                raw_event_json: persistent.as_json(),
                matched_scope: MatchedScope {
                    scope_type: ScopeType::ChannelH,
                    scope_value: "channel".to_owned(),
                },
            },
        ],
        &owner_pk,
        RELAY,
        &conn,
    )
    .unwrap();
    let results = plan
        .buckets
        .into_iter()
        .map(|bucket| super::pipeline::BucketWithResult {
            scope_type_str: bucket.scope_type_str,
            scope_value: bucket.scope_value,
            allowed_kinds: bucket.allowed_kinds,
            group: bucket.group,
            returned_ids: [persistent_id.clone()].into_iter().collect(),
            relay_failed: false,
        })
        .collect();
    // A fixture-only schema failure must not be mistaken for ordinary OFF consent.
    conn.execute_batch("DROP TABLE save_subscriptions").unwrap();
    assert!(commit_archive(
        results,
        plan.ephemeral,
        plan.pre_dropped,
        &owner_pk,
        RELAY,
        &owner,
        1000,
        &conn
    )
    .is_err());
    assert!(ids(&conn, &owner_pk, RELAY).is_empty());
}
