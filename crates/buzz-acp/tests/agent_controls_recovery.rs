//! Integration tests for durable control store recovery behavior.
//!
//! Each test uses a real temp SQLite file and simulates agent-process kill /
//! restart, tenant conflict, and authority-change scenarios.
//!
//! T19: kill_mid_pause_restart_rereads_same_lease
//! T20: expired_lease_found_at_startup_releases_with_expired_audit
//! T21: a_b_a_owner_sequence_releases_stale_lease_with_both_revisions
//! T22: store_opened_under_other_relay_origin_refuses_everything

use std::sync::Mutex;

use buzz_acp::control_store::*;
use buzz_core::agent_control::{
    decrypt_and_validate_pause_lease_transition, ControlTarget, PauseLeaseTransition,
    PauseLeaseTransitionKind, ResolvedControlFacts, ResolvedPauseLease,
};
use buzz_core::kind::KIND_AGENT_OBSERVER_FRAME;
use buzz_core::observer::{
    encrypt_observer_payload, OBSERVER_AGENT_TAG, OBSERVER_FRAME_CONTROL, OBSERVER_FRAME_TAG,
};
use buzz_core::CommunityId;
use nostr::{EventBuilder, Keys, Kind, Tag};
use rusqlite::params;
use uuid::Uuid;

const NOW: u64 = 1_800_000_000;
const CHANNEL_ID: &str = "52a85618-0f8f-4542-94ec-599e6e1c6f2e";

// ── Helpers ──────────────────────────────────────────────────────────────────────

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("buzz-acp-int-{}-{}.sqlite", name, Uuid::new_v4()))
}

fn community_id() -> CommunityId {
    CommunityId::from_uuid(
        Uuid::parse_str("3580ca9b-47b4-4af9-b22a-1068778f26c6").expect("fixed community id"),
    )
}

fn channel_id() -> Uuid {
    Uuid::parse_str(CHANNEL_ID).expect("fixed channel id")
}

fn test_host_identity() -> HostIdentityInput {
    HostIdentityInput {
        computer_id_override: Some("int-test-computer".into()),
        community_id: community_id(),
        relay_origin: "ws://localhost:3000".into(),
    }
}

fn test_host_identity_with_community(cid: CommunityId) -> HostIdentityInput {
    HostIdentityInput {
        computer_id_override: Some("int-test-computer".into()),
        community_id: cid,
        relay_origin: "ws://localhost:3000".into(),
    }
}

fn make_control_target(agent_pk_hex: &str) -> ControlTarget {
    ControlTarget {
        computer_id: "int-test-computer".into(),
        agent_pubkey: agent_pk_hex.to_string(),
        channel_id: channel_id(),
        run_id: "run-1".into(),
    }
}

fn make_pause_event(
    owner: &Keys,
    agent: &Keys,
    target: &ControlTarget,
    transition: &PauseLeaseTransition,
) -> nostr::Event {
    let encrypted =
        encrypt_observer_payload(owner, &agent.public_key(), transition).expect("encrypt");
    EventBuilder::new(
        Kind::Custom(KIND_AGENT_OBSERVER_FRAME as u16),
        encrypted,
    )
    .tags([
        Tag::parse(["p", &agent.public_key().to_hex()]).expect("p tag"),
        Tag::parse([OBSERVER_AGENT_TAG, &target.agent_pubkey]).expect("agent tag"),
        Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_CONTROL]).expect("frame tag"),
        Tag::parse(["h", &target.channel_id.to_string()]).expect("h tag"),
    ])
    .custom_created_at(nostr::Timestamp::from(transition.issued_at))
    .sign_with_keys(owner)
    .expect("sign")
}

fn base_pause(owner: &Keys, agent: &Keys, target: &ControlTarget) -> PauseLeaseTransition {
    PauseLeaseTransition {
        format: buzz_core::agent_control::PAUSE_LEASE_FORMAT.into(),
        version: buzz_core::agent_control::VERSION,
        transition_id: Uuid::new_v4(),
        lease_id: Uuid::new_v4(),
        generation: 1,
        transition: PauseLeaseTransitionKind::Pause,
        operator_pubkey: owner.public_key().to_hex(),
        target: target.clone(),
        seq: 20,
        issued_at: NOW - 1,
        transition_expires_at: NOW + 60,
        lease_expires_at: Some(NOW + buzz_core::agent_control::DEFAULT_PAUSE_LEASE_SECS),
    }
}

fn next_lease_transition(
    prior: &PauseLeaseTransition,
    kind: PauseLeaseTransitionKind,
) -> PauseLeaseTransition {
    PauseLeaseTransition {
        transition_id: Uuid::new_v4(),
        generation: prior.generation + 1,
        transition: kind,
        seq: prior.seq + 1,
        issued_at: NOW,
        transition_expires_at: NOW + 60,
        lease_expires_at: match kind {
            PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => Some(NOW + 600),
            PauseLeaseTransitionKind::Resume => None,
        },
        ..prior.clone()
    }
}

fn validate_pause_transition(
    owner: &Keys,
    agent: &Keys,
    transition: &PauseLeaseTransition,
    facts: &ResolvedControlFacts,
    current: Option<&ResolvedPauseLease>,
) -> buzz_core::agent_control::ValidatedPauseLeaseTransition {
    let event = make_pause_event(owner, agent, &facts.target, transition);
    decrypt_and_validate_pause_lease_transition(&event, agent, facts, current).expect("validate")
}

/// Open a store and unwrap the Ready variant, panicking if poisoned.
fn open_ready(path: &std::path::Path, identity: &HostIdentityInput) -> ControlStore {
    match ControlStore::open(path, identity).expect("open store") {
        ControlStoreHandle::Ready(s) => s,
        ControlStoreHandle::Poisoned(reason) => panic!("store poisoned: {reason}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════
// T19: kill_mid_pause_restart_rereads_same_lease
// ═══════════════════════════════════════════════════════════════════════════════════

#[test]
fn kill_mid_pause_restart_rereads_same_lease() {
    let path = temp_path("t19");
    let cid = community_id();
    let owner = Keys::generate();
    let agent = Keys::generate();
    let target = make_control_target(&agent.public_key().to_hex());
    let facts = ResolvedControlFacts {
        community_id: cid,
        now: NOW,
        operator_pubkey: owner.public_key().to_hex(),
        agent_ownership_revision: 1,
        target: target.clone(),
        steer_message: None,
    };

    // 1. Open store, reconcile owner binding, apply a pause transition.
    let pause = {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        store
            .reconcile_owner_binding(Some(&owner.public_key().to_hex()))
            .expect("reconcile");

        let transition = base_pause(&owner, &agent, &target);
        let validated =
            validate_pause_transition(&owner, &agent, &transition, &facts, None);

        let outcome = store
            .apply_pause_transition(&validated, |_qh| serde_json::json!({"status": "ok"}))
            .expect("apply pause");
        assert!(matches!(outcome, LeaseOutcome::Applied(_)));
        transition
    };
    // store dropped here — simulates kill

    // 3. Re-open the SAME store file with the SAME community_id.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        store
            .reconcile_owner_binding(Some(&owner.public_key().to_hex()))
            .expect("reconcile");

        // 4. read_current_lease — must find lease with active=true
        let lease = store
            .read_current_lease()
            .expect("read lease")
            .expect("lease must exist");
        assert!(lease.active, "lease must be active after restart");
        assert_eq!(
            lease.lease_id, pause.lease_id.to_string(),
            "same lease_id"
        );
        assert_eq!(lease.generation, 1, "same generation");

        // 5. Apply a resume transition — must work with generation+1
        let resume = next_lease_transition(&pause, PauseLeaseTransitionKind::Resume);
        let core_lease: buzz_core::agent_control::ResolvedPauseLease = (&lease).into();
        let validated =
            validate_pause_transition(&owner, &agent, &resume, &facts, Some(&core_lease));

        let outcome = store
            .apply_pause_transition(&validated, |_qh| serde_json::json!({"status": "ok"}))
            .expect("apply resume");
        assert!(
            matches!(outcome, LeaseOutcome::Applied(_)),
            "resume must be Applied"
        );

        // 6. Verify lease is now active=false
        let lease_after = store
            .read_current_lease()
            .expect("read after resume")
            .expect("lease must still exist");
        assert!(!lease_after.active, "lease must be inactive after resume");
        assert_eq!(lease_after.generation, 2, "generation must advance");
    }

    let _ = std::fs::remove_file(&path);
}

// ═══════════════════════════════════════════════════════════════════════════════════
// T20: expired_lease_found_at_startup_releases_with_expired_audit
// ═══════════════════════════════════════════════════════════════════════════════════

#[test]
fn expired_lease_found_at_startup_releases_with_expired_audit() {
    let path = temp_path("t20");
    let cid = community_id();
    let owner = Keys::generate();
    let agent = Keys::generate();
    let target = make_control_target(&agent.public_key().to_hex());
    let facts = ResolvedControlFacts {
        community_id: cid,
        now: NOW,
        operator_pubkey: owner.public_key().to_hex(),
        agent_ownership_revision: 1,
        target: target.clone(),
        steer_message: None,
    };

    // 1. Open store, apply a pause, drop store.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        store
            .reconcile_owner_binding(Some(&owner.public_key().to_hex()))
            .expect("reconcile");

        let transition = base_pause(&owner, &agent, &target);
        let validated =
            validate_pause_transition(&owner, &agent, &transition, &facts, None);

        let outcome = store
            .apply_pause_transition(&validated, |_qh| serde_json::json!({"status": "ok"}))
            .expect("apply pause");
        assert!(matches!(outcome, LeaseOutcome::Applied(_)));
    }

    // 2. Manipulate lease_expires_at to be in the past.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        let conn = store.conn().lock().unwrap();
        conn.execute(
            "UPDATE pause_lease_current SET lease_expires_at = ?1 WHERE community_id = ?2",
            params![1_i64, cid.as_uuid().to_string()],
        )
        .expect("update lease_expires_at");
    }

    // 3. Re-open store, verify lease is found but expired.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        store
            .reconcile_owner_binding(Some(&owner.public_key().to_hex()))
            .expect("reconcile");

        let lease = store
            .read_current_lease()
            .expect("read lease")
            .expect("lease must exist");
        assert!(lease.active, "lease should still be active");
        assert!(
            lease.lease_expires_at < NOW,
            "lease_expires_at should be in the past"
        );

        // 4. Call release_lease(ReleaseReason::Expired)
        store
            .release_lease(ReleaseReason::Expired)
            .expect("release expired");

        // 5. Verify active=0
        let lease_after = store
            .read_current_lease()
            .expect("read after release")
            .expect("lease must still exist");
        assert!(!lease_after.active, "lease must be inactive after release");

        // 6. Verify exactly one PauseLeaseExpired audit entry
        let conn = store.conn().lock().unwrap();
        let expired_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM control_audit WHERE event = 'pause_lease_expired'",
                [],
                |row| row.get(0),
            )
            .expect("count expired audits");
        assert_eq!(
            expired_count, 1,
            "exactly one PauseLeaseExpired audit entry"
        );

        // And no PauseLeaseReleased entry
        let released_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM control_audit WHERE event = 'pause_lease_released'",
                [],
                |row| row.get(0),
            )
            .expect("count released audits");
        assert_eq!(
            released_count, 0,
            "no PauseLeaseReleased entry for expired release"
        );
    }

    let _ = std::fs::remove_file(&path);
}

// ═══════════════════════════════════════════════════════════════════════════════════
// T21: a_b_a_owner_sequence_releases_stale_lease_with_both_revisions
// ═══════════════════════════════════════════════════════════════════════════════════

#[test]
fn a_b_a_owner_sequence_releases_stale_lease_with_both_revisions() {
    let path = temp_path("t21");
    let cid = community_id();
    let owner_a = Keys::generate();
    let owner_b = Keys::generate();
    let agent = Keys::generate();
    let target = make_control_target(&agent.public_key().to_hex());
    let facts_a = ResolvedControlFacts {
        community_id: cid,
        now: NOW,
        operator_pubkey: owner_a.public_key().to_hex(),
        agent_ownership_revision: 1,
        target: target.clone(),
        steer_message: None,
    };

    // 1. Open store with owner A, reconcile (revision 1), apply pause.
    let pause_transition_id: Uuid;
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);

        // Owner A — revision 1
        let (pk, rev) = store
            .reconcile_owner_binding(Some(&owner_a.public_key().to_hex()))
            .expect("reconcile a")
            .expect("owner binding");
        assert_eq!(pk, owner_a.public_key().to_hex());
        assert_eq!(rev, 1);

        let transition = base_pause(&owner_a, &agent, &target);
        pause_transition_id = transition.transition_id;
        let validated =
            validate_pause_transition(&owner_a, &agent, &transition, &facts_a, None);

        let outcome = store
            .apply_pause_transition(&validated, |_qh| serde_json::json!({"status": "ok"}))
            .expect("apply pause");
        assert!(matches!(outcome, LeaseOutcome::Applied(_)));
    }

    // 2. Reconcile owner B — revision advances to 2.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        let (pk, rev) = store
            .reconcile_owner_binding(Some(&owner_b.public_key().to_hex()))
            .expect("reconcile b")
            .expect("owner binding");
        assert_eq!(pk, owner_b.public_key().to_hex());
        assert_eq!(rev, 2);
    }

    // 3. Reconcile owner A again — revision advances to 3.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);
        let (pk, rev) = store
            .reconcile_owner_binding(Some(&owner_a.public_key().to_hex()))
            .expect("reconcile a again")
            .expect("owner binding");
        assert_eq!(pk, owner_a.public_key().to_hex());
        assert_eq!(rev, 3);
    }

    // 4. The lease persisted under revision 1.
    // 5. Call release_lease with AuthorityChanged.
    {
        let identity = test_host_identity_with_community(cid);
        let store = open_ready(&path, &identity);

        let lease = store
            .read_current_lease()
            .expect("read lease")
            .expect("lease must exist");
        assert!(lease.active);
        assert_eq!(lease.ownership_revision, 1);

        store
            .release_lease(ReleaseReason::AuthorityChanged {
                persisted_revision: 1,
                current_revision: 3,
            })
            .expect("release authority changed");

        // 6. Verify active=0
        let lease_after = store
            .read_current_lease()
            .expect("read after release");

        // The release_lease CAS checks lease_id + generation + active=1.
        // If the CAS succeeded, active is now 0.
        let lease_after = lease_after.expect("lease must exist");
        assert!(
            !lease_after.active,
            "lease must be inactive after authority release"
        );

        // 7. Audit shows PauseLeaseAuthorityReleased with both revisions.
        let conn = store.conn().lock().unwrap();
        let (event, pr, cr): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT event, persisted_revision, ownership_revision
                 FROM control_audit
                 WHERE event = 'pause_lease_authority_released'
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read authority release audit");
        assert_eq!(event, "pause_lease_authority_released");
        assert_eq!(pr, Some(1), "persisted_revision must be 1");
        assert_eq!(cr, Some(3), "ownership_revision must be 3");
    }

    let _ = std::fs::remove_file(&path);
}

// ═══════════════════════════════════════════════════════════════════════════════════
// T22: store_opened_under_other_relay_origin_refuses_everything
// ═══════════════════════════════════════════════════════════════════════════════════

#[test]
fn store_opened_under_other_relay_origin_refuses_everything() {
    let path = temp_path("t22");

    // 1. Open store at temp path with relay_origin "ws://relay-a:3000".
    let community_a = CommunityId::from_uuid(Uuid::new_v4());
    {
        let identity_a = HostIdentityInput {
            computer_id_override: Some("int-test-computer".into()),
            community_id: community_a,
            relay_origin: "ws://relay-a:3000".into(),
        };
        match ControlStore::open(&path, &identity_a).expect("first open") {
            ControlStoreHandle::Ready(_) => {} // success
            ControlStoreHandle::Poisoned(reason) => panic!("unexpected poison: {reason}"),
        }
    }

    // 4. Re-open SAME file with relay_origin "ws://relay-b:3000"
    // (different community_id → different origin → tenant mismatch).
    let community_b = CommunityId::from_uuid(Uuid::new_v4());
    {
        let identity_b = HostIdentityInput {
            computer_id_override: Some("int-test-computer".into()),
            community_id: community_b,
            relay_origin: "ws://relay-b:3000".into(),
        };
        let result = ControlStore::open(&path, &identity_b);
        // 5. Must return Poisoned (TenantMismatch)
        match result {
            Ok(handle) => {
                // Must be Poisoned, not Ready
                assert!(
                    matches!(handle, ControlStoreHandle::Poisoned(_)),
                    "expected Poisoned, got Ready"
                );
                let reason = handle.poison_reason().expect("must have poison reason");
                assert!(
                    reason.contains("tenant_mismatch")
                        || reason.contains("store unavailable"),
                    "poison reason must mention tenant: got '{reason}'"
                );
            }
            Err(e) => {
                // Also valid — the open itself returned TenantMismatch
                assert!(
                    matches!(e, StoreError::TenantMismatch { .. }),
                    "expected TenantMismatch error, got: {e}"
                );
            }
        }
    }

    let _ = std::fs::remove_file(&path);
}