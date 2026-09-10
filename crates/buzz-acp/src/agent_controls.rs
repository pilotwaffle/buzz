//! Structured control handler (Slice 2).
//!
//! Handles `buzz-agent-control-command` and `buzz-agent-pause-lease` frames
//! beside the legacy handlers. Cancel → ControlSignal::Cancel; steer → receipt
//! query; pause → durable queue hold.

use std::collections::HashMap;

use buzz_core::agent_control::{
    self, decrypt_and_validate_owner_control, ControlAckExcerpt, ControlAckReason,
    ControlAckStatus, OneShotControlAck, OneShotControlKind, PauseLeaseAck,
    PauseLeaseAckStatus, PauseLeaseTransitionKind, QueueHoldState as CoreQueueHoldState,
    ResolvedControlFacts, ResolvedSteerMessage,
    ValidatedOneShotControl, ValidatedOwnerControl, ValidatedPauseLeaseTransition,
    COMMAND_ACK_FORMAT, COMMAND_FORMAT, PAUSE_LEASE_ACK_FORMAT, PAUSE_LEASE_FORMAT, VERSION,
};
use buzz_core::observer::{decrypt_observer_payload, encrypt_observer_payload, OBSERVER_FRAME_TELEMETRY};
use buzz_core::CommunityId;
use chrono::Utc;
use nostr::{Event, Keys, PublicKey};
use url::Url;
use uuid::Uuid;

use crate::config::Config;
use crate::control_store::{
    AuditEntry, AuditEvent, ClaimOutcome, ControlStore, ControlStoreHandle, HostIdentityInput,
    LeaseOutcome, QueueHoldState, ReleaseReason, StoreError,
};
use crate::pool::{AgentPool, ControlSignal};
use crate::RelayEventPublisher;

// Slice-2 stopgap (design_answers.md Q1): this deployment has a single operator
// and one relay, and the relay publishes no NIP-11 `self` key, so the consumer
// derives the tenant id from the relay origin and keeps its own monotonic
// ownership revision. The server-authoritative source (relay identity +
// relay-issued ownership revision) is Slice-5 relay item R5-3. The validator in
// buzz-core is unchanged; these values enter only via ResolvedControlFacts.

/// Namespace UUID for deriving community_id from relay origin.
const BUZZ_AO_TENANT_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Derive a stable [`CommunityId`] from the relay's WebSocket origin.
pub(crate) fn derive_community_id(relay_url: &str) -> CommunityId {
    let parsed = Url::parse(relay_url).expect("relay_url must be a valid URL");
    let host = parsed.host_str().unwrap_or("localhost");
    let port = parsed.port().unwrap_or_else(|| match parsed.scheme() {
        "wss" | "https" => 443,
        _ => 80,
    });
    let origin = format!(
        "{}://{}:{}",
        parsed.scheme(),
        host.to_ascii_lowercase(),
        port
    );
    CommunityId::from_uuid(Uuid::new_v5(&BUZZ_AO_TENANT_NAMESPACE, origin.as_bytes()))
}

/// Per-agent structured control state.
pub(crate) struct AgentControls {
    pub store: ControlStoreHandle,
    pub community_id: CommunityId,
    pub queue_hold: QueueHoldState,
    /// Monotonic ack sequence counter.
    pub ack_seq: u64,
    /// Counters for tracing lines.
    pub controls_received: u64,
    pub controls_acked: u64,
    pub controls_refused: u64,
    pub controls_expired: u64,
    pub pause_active: u64,
    /// Pending steer commands (event id → raw entry).
    pub pending_steers: Vec<PendingSteerEntry>,
    /// Bounded per-channel ring of recent events for steer receipt resolution.
    pub recent_events: HashMap<Uuid, Vec<RecentChannelEvent>>,
}

pub(crate) struct PendingSteerEntry {
    pub event: Event,
    pub expires_at: u64,
    pub deadline: u64,
}

pub(crate) struct RecentChannelEvent {
    pub event_id: String,
    pub author: String,
    pub created_at: u64,
    pub delivery: SteerDeliveryMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SteerDeliveryMethod {
    Queued,
    NativeSteer,
    CrossAdapterSteer,
    CancelAndMerge,
    PromptedNormally,
}

impl AgentControls {
    /// Open the control store and reconcile identity.
    pub(crate) fn open(
        config: &Config,
        agent_pubkey_hex: &str,
        resolved_owner: Option<&str>,
    ) -> Result<Self, StoreError> {
        let community_id = derive_community_id(&config.relay_url);

        let relay_origin = {
            let parsed = Url::parse(&config.relay_url).map_err(|_| {
                StoreError::StoreUnavailable(rusqlite::Error::InvalidParameterName(
                    "relay_url".into(),
                ))
            })?;
            let host = parsed.host_str().unwrap_or("localhost");
            format!(
                "{}://{}:{}",
                parsed.scheme(),
                host.to_ascii_lowercase(),
                parsed.port().unwrap_or(80)
            )
        };

        let store_path = config.control_store.clone().unwrap_or_else(|| {
            let mut dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            dir.push(".buzz-acp");
            dir.push(format!("control-{}.sqlite", &agent_pubkey_hex[..16.min(agent_pubkey_hex.len())]));
            dir
        });

        let hi = HostIdentityInput {
            computer_id_override: config.computer_id.clone(),
            community_id,
            relay_origin,
        };

        let store = ControlStore::open(&store_path, &hi)?;

        let ready_store = match &store {
            ControlStoreHandle::Ready(s) => s,
            ControlStoreHandle::Poisoned(reason) => {
                tracing::warn!("control store poisoned: {reason}");
                return Ok(AgentControls {
                    store,
                    community_id,
                    queue_hold: QueueHoldState::Running,
                    ack_seq: 1,
                    controls_received: 0,
                    controls_acked: 0,
                    controls_refused: 0,
                    controls_expired: 0,
                    pause_active: 0,
                    pending_steers: Vec::new(),
                    recent_events: HashMap::new(),
                });
            }
        };

        // Reconcile owner binding.
        ready_store.reconcile_owner_binding(resolved_owner)?;

        // Read current lease for startup recovery (I-8, I-9, I-10).
        let queue_hold = if let Some(ref lease) = ready_store.read_current_lease()? {
            let now = Utc::now().timestamp() as u64;
            let (owner, revision) = ready_store
                .read_owner_binding()
                .unwrap_or((String::new(), 0));
            let core_lease: agent_control::ResolvedPauseLease = lease.into();
            match agent_control::effective_pause_state(&core_lease, now, &owner, revision) {
                Ok(agent_control::EffectivePauseState::HoldQueue) => {
                    tracing::info!(
                        "pause recovered lease_id={} generation={} expires_at={} effective=HoldQueue",
                        lease.lease_id,
                        lease.generation,
                        lease.lease_expires_at,
                    );
                    QueueHoldState::HoldQueue
                }
                Ok(agent_control::EffectivePauseState::ExpiredMustRelease) => {
                    ready_store.release_lease(ReleaseReason::Expired)?;
                    QueueHoldState::Running
                }
                Ok(agent_control::EffectivePauseState::AuthorityChangedMustRelease) => {
                    let (_owner, current_revision) = ready_store.read_owner_binding()?;
                    ready_store.release_lease(ReleaseReason::AuthorityChanged {
                        persisted_revision: lease.ownership_revision,
                        current_revision,
                    })?;
                    QueueHoldState::Running
                }
                _ => QueueHoldState::Running,
            }
        } else {
            QueueHoldState::Running
        };

        Ok(AgentControls {
            store,
            community_id,
            queue_hold,
            ack_seq: 1,
            controls_received: 0,
            controls_acked: 0,
            controls_refused: 0,
            controls_expired: 0,
            pause_active: 0,
            pending_steers: Vec::new(),
            recent_events: HashMap::new(),
        })
    }

    /// Evaluate effective pause state before dispatch.
    pub(crate) fn effective_state_before_dispatch(
        &mut self,
        now: u64,
    ) -> QueueHoldState {
        let ready = match self.store.as_ready() {
            Some(s) => s,
            None => return QueueHoldState::HoldQueue, // fail closed
        };

        let Ok(Some(lease)) = ready.read_current_lease() else {
            return QueueHoldState::Running;
        };

        let Ok((owner, revision)) = ready.read_owner_binding() else {
            return QueueHoldState::HoldQueue; // fail closed
        };

        let core_lease: agent_control::ResolvedPauseLease = (&lease).into();
        match agent_control::effective_pause_state(&core_lease, now, &owner, revision) {
            Ok(agent_control::EffectivePauseState::HoldQueue) => QueueHoldState::HoldQueue,
            Ok(agent_control::EffectivePauseState::ExpiredMustRelease) => {
                let _ = ready.release_lease(ReleaseReason::Expired);
                self.queue_hold = QueueHoldState::Running;
                QueueHoldState::Running
            }
            Ok(agent_control::EffectivePauseState::AuthorityChangedMustRelease) => {
                let (_owner, current_revision) = ready.read_owner_binding().unwrap_or((String::new(), 0));
                let _ = ready.release_lease(ReleaseReason::AuthorityChanged {
                    persisted_revision: lease.ownership_revision,
                    current_revision,
                });
                self.queue_hold = QueueHoldState::Running;
                QueueHoldState::Running
            }
            _ => QueueHoldState::Running,
        }
    }

    /// Periodic tick: handle expiry, abandon, and purge.
    pub(crate) fn tick(&mut self, now: u64) {
        // Release expired pause lease.
        let expiry_needed = if self.queue_hold == QueueHoldState::HoldQueue {
            self.store.as_ready().and_then(|ready| {
                ready.read_current_lease().ok().flatten().map(|lease| {
                    now >= lease.lease_expires_at
                })
            }).unwrap_or(false)
        } else {
            false
        };

        if expiry_needed {
            if let Some(ready) = self.store.as_ready() {
                let _ = ready.release_lease(ReleaseReason::Expired);
                self.queue_hold = QueueHoldState::Running;
                self.controls_expired += 1;
            }
        }

        // Drop expired pending steers.
        self.pending_steers.retain(|entry| {
            if now >= entry.deadline {
                self.controls_expired += 1;
                false
            } else {
                true
            }
        });

        // Purge expired.
        if let Some(ready) = self.store.as_ready() {
            let _ = ready.purge_expired(now);
        }
    }

    // ── RecentChannelEvents ring (spec 3.4) ─────────────────────────

    const MAX_RECENT_PER_CHANNEL: usize = 256;

    /// Record a channel event in the bounded recent ring.
    pub(crate) fn record_channel_event(
        &mut self,
        channel_id: Uuid,
        event_id: String,
        author: String,
        created_at: u64,
        delivery: SteerDeliveryMethod,
    ) {
        let ring = self.recent_events.entry(channel_id).or_default();
        ring.push(RecentChannelEvent { event_id, author, created_at, delivery });
        if ring.len() > Self::MAX_RECENT_PER_CHANNEL {
            ring.remove(0);
        }
    }

    /// Look up a steer message in the recent ring.
    fn find_in_recent_ring(
        &self,
        channel_id: Uuid,
        event_id: &str,
    ) -> Option<&RecentChannelEvent> {
        self.recent_events
            .get(&channel_id)
            .and_then(|ring| ring.iter().find(|e| e.event_id == event_id))
    }

    // ── PendingSteerCommands (spec 3.4) ─────────────────────────────

    const MAX_PENDING_STEERS: usize = 64;

    /// Add a steer command whose referenced event id is not yet known.
    /// Returns `true` if added, `false` if the set was full (caller audits
    /// the evicted entry).
    pub(crate) fn add_pending_steer(
        &mut self,
        event: Event,
        expires_at: u64,
    ) -> (bool, Option<PendingSteerEntry>) {
        let deadline = expires_at.min(
            (chrono::Utc::now().timestamp() as u64).saturating_add(60)
        );
        let entry = PendingSteerEntry { event, expires_at, deadline };
        let mut evicted = None;
        if self.pending_steers.len() >= Self::MAX_PENDING_STEERS {
            // Evict oldest by deadline.
            self.pending_steers.sort_by_key(|e| e.deadline);
            evicted = Some(self.pending_steers.remove(0));
        }
        self.pending_steers.push(entry);
        (true, evicted)
    }
}
///
/// Called from `handle_relay_observer_control_event` after the
/// decrypted payload has a `format` field matching [`COMMAND_FORMAT`]
/// or [`PAUSE_LEASE_FORMAT`]. Validation, durable claim, effect
/// dispatch, and ack publishing all happen here.
pub(crate) fn handle_structured_control(
    keys: &Keys,
    event: Event,
    controls: &mut AgentControls,
    pool: &mut AgentPool,
    owner_pubkey_hex: &str,
    event_publisher: RelayEventPublisher,
    queue: &crate::queue::EventQueue,
) {
    let ready = match controls.store.as_ready() {
        Some(s) => s,
        None => {
            tracing::info!("control refused: store unavailable");
            controls.controls_refused += 1;
            return;
        }
    };

    let (stored_owner, revision) = match ready.read_owner_binding() {
        Ok(b) => b,
        Err(_) => {
            tracing::info!("control refused: cannot read owner binding");
            return;
        }
    };

    let host = match ready.read_host_identity() {
        Ok(h) => h,
        Err(_) => {
            tracing::info!("control refused: cannot read host identity");
            return;
        }
    };

    // Peek at the decrypted payload to check for steer receipt and
    // to build facts (spec 3.1: channel peek is lenient; the frozen
    // entry point re-decrypts from the signed event for validation).
    let payload: Option<serde_json::Value> = decrypt_observer_payload(keys, &event).ok();
    let format = payload.as_ref()
        .and_then(|p| p.get("format"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut steer_message: Option<ResolvedSteerMessage> = None;

    if format == COMMAND_FORMAT {
        let is_steer = payload.as_ref()
            .and_then(|p| p.get("control"))
            .and_then(|v| v.as_str())
            .map(|c| c == "steer")
            .unwrap_or(false);

        if is_steer {
            // Resolve the steer message event id (spec 3.4).
            if let Some(se_id) = payload.as_ref()
                .and_then(|p| p.get("steer_message_event_id"))
                .and_then(|v| v.as_str())
            {
                // Check sources in order: ring → queue → pool in-flight.
                if let Some(ring_entry) = controls.find_in_recent_ring(
                    Uuid::default(), // channel unknown at peek time — scan ring
                    se_id,
                ) {
                    steer_message = Some(ResolvedSteerMessage {
                        community_id: controls.community_id,
                        event_id: ring_entry.event_id.clone(),
                        operator_pubkey: ring_entry.author.clone(),
                        channel_id: Uuid::default(),
                        created_at: ring_entry.created_at,
                    });
                } else {
                    // Scan the queues for the event.
                    // The channel_id is in the payload target — peek leniently.
                    let peek_channel = payload.as_ref()
                        .and_then(|p| p.get("target"))
                        .and_then(|t| t.get("channel_id"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .unwrap_or_else(Uuid::nil);

                    if let Some(_qe) = queue.find_queued_event(peek_channel, se_id)
                        .or_else(|| queue.find_withheld_event(peek_channel, se_id))
                    {
                        steer_message = Some(ResolvedSteerMessage {
                            community_id: controls.community_id,
                            event_id: se_id.to_string(),
                            operator_pubkey: String::new(), // filled by validator
                            channel_id: peek_channel,
                            created_at: 0, // filled by validator
                        });
                    } else {
                        // Check in-flight tasks for the event in their
                        // recoverable_batch or successful_steer_deliveries.
                        let found_in_pool = pool.task_map()
                            .values()
                            .any(|meta| {
                                meta.channel_id == Some(peek_channel)
                                && (
                                    meta.recoverable_batch.iter()
                                        .flat_map(|fb| fb.events.iter())
                                        .any(|be| be.event.id.to_hex() == se_id)
                                    || meta.successful_steer_deliveries.iter()
                                        .any(|d| d.event_id == se_id)
                                )
                            });

                        if found_in_pool {
                            steer_message = Some(ResolvedSteerMessage {
                                community_id: controls.community_id,
                                event_id: se_id.to_string(),
                                operator_pubkey: String::new(),
                                channel_id: peek_channel,
                                created_at: 0,
                            });
                        } else {
                            // Unknown id — pend and return (spec 3.4).
                            let cmd_expires = payload.as_ref()
                                .and_then(|p| p.get("expires_at"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or_else(|| {
                                    (Utc::now().timestamp() as u64).saturating_add(60)
                                });
                            let (added, evicted) =
                                controls.add_pending_steer(event.clone(), cmd_expires);
                            if let Some(_evicted_entry) = evicted {
                                if let Some(ready) = controls.store.as_ready() {
                                    let entry = crate::control_store::AuditEntry {
                                        at: Utc::now().timestamp() as u64,
                                        event: crate::control_store::AuditEvent::ControlExpired,
                                        community_id: Some(controls.community_id.to_string()),
                                        command_id: None,
                                        transition_id: None,
                                        lease_id: None,
                                        fingerprint: None,
                                        operator_pubkey: None,
                                        agent_pubkey: None,
                                        computer_id: None,
                                        channel_id: None,
                                        run_id: None,
                                        ownership_revision: None,
                                        persisted_revision: None,
                                        outcome: Some("pending_overflow".to_string()),
                                        detail: Some("pending_overflow".to_string()),
                                    };
                                    let conn = ready.conn().lock().unwrap();
                                    let _ = crate::control_store::audit_simple(&conn, entry);
                                }
                            }
                            if added {
                                controls.controls_received += 1;
                            }
                            return;
                        }
                    }
                }
            }
        }
    }

    let target = agent_control::ControlTarget {
        computer_id: host.computer_id,
        agent_pubkey: keys.public_key().to_hex(),
        channel_id: Uuid::nil(),
        run_id: String::new(),
    };

    let facts = ResolvedControlFacts {
        community_id: controls.community_id,
        now: Utc::now().timestamp() as u64,
        operator_pubkey: stored_owner,
        agent_ownership_revision: revision,
        target,
        steer_message,
    };

    // Validate against the frozen core validator.
    let current_lease = ready.read_current_lease().ok().flatten();
    let core_lease: Option<agent_control::ResolvedPauseLease> =
        current_lease.as_ref().map(|l| l.into());

    let validated = match decrypt_and_validate_owner_control(
        &event,
        keys,
        &facts,
        core_lease.as_ref(),
    ) {
        Ok(v) => v,
        Err(err) => {
            controls.controls_refused += 1;
            let code = error_code(&err);
            tracing::info!(code = %code, "control refused: {err}");

            if let Some((builder, ids)) = build_rejection_ack(&err, &event, keys, owner_pubkey_hex) {
                audit_refusal(ready, &event, &ids, code, &err);
                publish_ack(event_publisher, builder, keys, owner_pubkey_hex);
            }
            return;
        }
    };

    controls.controls_received += 1;
    let received_ms = chrono::Utc::now().timestamp_millis();

    // Log "control received" (spec 3.8 / N2).
    match &validated {
        ValidatedOwnerControl::OneShot(cmd) => {
            let c = cmd.command();
            tracing::info!(
                format = %buzz_core::agent_control::COMMAND_FORMAT,
                control = %format!("{:?}", c.control),
                command_id = %c.command_id,
                channel = %c.target.channel_id,
                run_id = %c.target.run_id,
                issued_at = %c.issued_at,
                control_received_epoch_millis = %received_ms,
                controls_received = %controls.controls_received,
                controls_acked = %controls.controls_acked,
                controls_refused = %controls.controls_refused,
                controls_expired = %controls.controls_expired,
                "control received"
            );
        }
        ValidatedOwnerControl::PauseLease(t) => {
            let tr = t.transition();
            tracing::info!(
                format = %buzz_core::agent_control::PAUSE_LEASE_FORMAT,
                transition = %format!("{:?}", tr.transition),
                transition_id = %tr.transition_id,
                channel = %tr.target.channel_id,
                run_id = %tr.target.run_id,
                issued_at = %tr.issued_at,
                control_received_epoch_millis = %received_ms,
                controls_received = %controls.controls_received,
                controls_acked = %controls.controls_acked,
                controls_refused = %controls.controls_refused,
                controls_expired = %controls.controls_expired,
                "control received"
            );
        }
    }

    match validated {
        ValidatedOwnerControl::OneShot(cmd) => {
            handle_one_shot(controls, pool, keys, owner_pubkey_hex, event_publisher, cmd, received_ms);
        }
        ValidatedOwnerControl::PauseLease(transition) => {
            handle_pause_lease(controls, keys, owner_pubkey_hex, event_publisher, transition, received_ms);
        }
    }
}

// ── error helpers ─────────────────────────────────────────────────

fn error_code(err: &agent_control::AgentControlError) -> &'static str {
    use agent_control::AgentControlError::*;
    match err {
        OperatorMismatch | AuthorityConflict | AgentMismatch | ComputerMismatch
        | ChannelMismatch | RunMismatch | SteerMessageMismatch | CommandReplay
        | LeaseConflict | LeaseGenerationMismatch | LeaseExpired | LeaseMissing
        | Expired => "binding_mismatch",
        StoreUnavailable => "store_unavailable",
        _ => "schema_error",
    }
}

fn audit_refusal(
    ready: &ControlStore,
    _event: &Event,
    ids: &AckIds,
    code: &str,
    err: &agent_control::AgentControlError,
) {
    let entry = AuditEntry {
        at: Utc::now().timestamp() as u64,
        event: AuditEvent::ControlRefused,
        community_id: None,
        command_id: ids.command_id.clone(),
        transition_id: ids.transition_id.clone(),
        lease_id: None,
        fingerprint: None,
        operator_pubkey: None,
        agent_pubkey: None,
        computer_id: None,
        channel_id: None,
        run_id: None,
        ownership_revision: None,
        persisted_revision: None,
        outcome: Some(code.to_string()),
        detail: Some(format!("validation: {err}")),
    };
    let conn = ready.conn().lock().unwrap();
    let _ = crate::control_store::audit_simple(&conn, entry);
}

struct AckIds {
    command_id: Option<String>,
    transition_id: Option<String>,
}

/// Build a `rejected` ack frame. Returns `None` for schema-level errors.
fn build_rejection_ack(
    err: &agent_control::AgentControlError,
    event: &Event,
    keys: &Keys,
    owner_pubkey_hex: &str,
) -> Option<(nostr::EventBuilder, AckIds)> {
    use agent_control::AgentControlError::*;
    let reason = match err {
        OperatorMismatch | AuthorityConflict | AgentMismatch | ComputerMismatch
        | ChannelMismatch | RunMismatch | SteerMessageMismatch | CommandReplay
        | LeaseConflict | LeaseGenerationMismatch | LeaseExpired | LeaseMissing
        | Expired => ControlAckReason::BindingMismatch,
        StoreUnavailable => ControlAckReason::InternalError,
        _ => ControlAckReason::InternalError,
    };

    let acked_at = Utc::now().timestamp() as u64;
    let agent_pubkey = keys.public_key().to_hex();

    // Decrypt to peek at the format discriminator.
    let payload: serde_json::Value = decrypt_observer_payload(keys, event).ok()?;
    let format = payload.get("format").and_then(|v| v.as_str()).unwrap_or("");

    if format == COMMAND_FORMAT {
        let cmd_id = payload.get("command_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);
        let kind = payload.get("control")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "steer" => OneShotControlKind::Steer,
                _ => OneShotControlKind::Cancel,
            })
            .unwrap_or(OneShotControlKind::Cancel);

        let ack = OneShotControlAck {
            format: COMMAND_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            command_id: cmd_id,
            command_fingerprint: String::new(),
            control: kind,
            operator_pubkey: owner_pubkey_hex.to_string(),
            target: agent_control::ControlTarget {
                computer_id: String::new(),
                agent_pubkey,
                channel_id: Uuid::nil(),
                run_id: String::new(),
            },
            command_seq: 0,
            seq: 0,
            acked_at,
            status: ControlAckStatus::Rejected,
            reason: Some(reason),
            detail: Some(ControlAckExcerpt { truncated: false, text: format!("{err}") }),
        };
        build_encrypted_ack_frame(keys, owner_pubkey_hex, &ack, acked_at)
            .ok()
            .map(|b| (b, AckIds {
                command_id: Some(cmd_id.to_string()),
                transition_id: None,
            }))
    } else if format == PAUSE_LEASE_FORMAT {
        let tid = payload.get("transition_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);
        let lid = payload.get("lease_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::new_v4);
        let kind = payload.get("transition")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "renew" => PauseLeaseTransitionKind::Renew,
                "resume" => PauseLeaseTransitionKind::Resume,
                _ => PauseLeaseTransitionKind::Pause,
            })
            .unwrap_or(PauseLeaseTransitionKind::Pause);

        let ack = PauseLeaseAck {
            format: PAUSE_LEASE_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            transition_id: tid,
            transition_fingerprint: String::new(),
            lease_id: lid,
            generation: 0,
            transition: kind,
            operator_pubkey: owner_pubkey_hex.to_string(),
            target: agent_control::ControlTarget {
                computer_id: String::new(),
                agent_pubkey,
                channel_id: Uuid::nil(),
                run_id: String::new(),
            },
            transition_seq: 0,
            seq: 0,
            acked_at,
            status: PauseLeaseAckStatus::Rejected,
            queue_state: CoreQueueHoldState::Running,
            detail: Some(ControlAckExcerpt { truncated: false, text: format!("{err}") }),
        };
        build_encrypted_ack_frame(keys, owner_pubkey_hex, &ack, acked_at)
            .ok()
            .map(|b| (b, AckIds {
                command_id: None,
                transition_id: Some(tid.to_string()),
            }))
    } else {
        // Unknown format — no ack.
        None
    }
}

// ── one-shot (cancel / steer) ────────────────────────────────────

fn handle_one_shot(
    controls: &mut AgentControls,
    pool: &mut AgentPool,
    keys: &Keys,
    owner_pubkey_hex: &str,
    publisher: RelayEventPublisher,
    validated: ValidatedOneShotControl,
    received_ms: i64,
) {
    let ready = match controls.store.as_ready() {
        Some(s) => s,
        None => { controls.controls_refused += 1; return; }
    };
    let target = validated.command().target.clone();
    let kind = validated.command().control;

    match kind {
        OneShotControlKind::Cancel => {
            match ready.claim_one_shot(&validated, &target) {
                Ok(ClaimOutcome::Fresh(permit)) => {
                    let run_id = target.run_id.clone();
                    let ack = if run_id == "idle" {
                        OneShotControlAck {
                            format: COMMAND_ACK_FORMAT.into(),
                            version: VERSION,
                            ack_id: Uuid::new_v4(),
                            command_id: validated.command().command_id,
                            command_fingerprint: validated.fingerprint().to_string(),
                            control: OneShotControlKind::Cancel,
                            operator_pubkey: validated.command().operator_pubkey.clone(),
                            target: target.clone(),
                            command_seq: validated.command().seq,
                            seq: controls.ack_seq,
                            acked_at: Utc::now().timestamp() as u64,
                            status: ControlAckStatus::NoActiveTurn,
                            reason: None,
                            detail: None,
                        }
                    } else {
                        let sent = signal_in_flight_task(pool, target.channel_id, ControlSignal::Cancel);
                        OneShotControlAck {
                            format: COMMAND_ACK_FORMAT.into(),
                            version: VERSION,
                            ack_id: Uuid::new_v4(),
                            command_id: validated.command().command_id,
                            command_fingerprint: validated.fingerprint().to_string(),
                            control: OneShotControlKind::Cancel,
                            operator_pubkey: validated.command().operator_pubkey.clone(),
                            target: target.clone(),
                            command_seq: validated.command().seq,
                            seq: controls.ack_seq,
                            acked_at: Utc::now().timestamp() as u64,
                            status: ControlAckStatus::Applied,
                            reason: None,
                            detail: if !sent {
                                Some(ControlAckExcerpt { truncated: false, text: "turn already ending".into() })
                            } else {
                                None
                            },
                        }
                    };

                    let acked_at = ack.acked_at;
                    let _ = ready.complete_one_shot(permit, &validated, &ack);
                    controls.ack_seq += 1;
                    controls.controls_acked += 1;

                    let acked_ms = chrono::Utc::now().timestamp_millis();
                    let latency_ms = (acked_ms - received_ms).max(0) as u64;
                    tracing::info!(
                        command_id = %ack.command_id,
                        control = "cancel",
                        status = %format!("{:?}", ack.status),
                        acked_epoch_millis = %acked_ms,
                        sidecar_latency_ms = %latency_ms,
                        controls_received = %controls.controls_received,
                        controls_acked = %controls.controls_acked,
                        controls_refused = %controls.controls_refused,
                        controls_expired = %controls.controls_expired,
                        pause_active = %controls.pause_active,
                        "control acked"
                    );

                    if let Ok(builder) = build_encrypted_ack_frame(keys, owner_pubkey_hex, &ack, acked_at) {
                        publish_ack(publisher, builder, keys, owner_pubkey_hex);
                    }
                }
                Ok(ClaimOutcome::DuplicatePending) => {}
                Ok(ClaimOutcome::DuplicateCompleted { ack_json: stored_ack_json, acked_at }) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stored_ack_json) {
                        if let Ok(stored) = serde_json::from_value::<OneShotControlAck>(v) {
                            if let Ok(builder) = build_encrypted_ack_frame(keys, owner_pubkey_hex, &stored, acked_at as u64) {
                                publish_ack(publisher, builder, keys, owner_pubkey_hex);
                            }
                        }
                    }
                }
                Ok(ClaimOutcome::CommandIdConflict) | Ok(ClaimOutcome::Expired)
                | Ok(ClaimOutcome::AuthorityConflict) => {
                    controls.controls_refused += 1;
                }
                Err(e) => {
                    controls.controls_refused += 1;
                    tracing::warn!("claim_one_shot error: {e}");
                }
            }
        }
        OneShotControlKind::Steer => {
            match ready.claim_one_shot(&validated, &target) {
                Ok(ClaimOutcome::Fresh(permit)) => {
                    // Resolve delivery branch from the recent-events ring
                    // (spec 3.4). The event must be in the ring at this
                    // point because the steer_message was resolved before
                    // validation; fall back to Queued if absent.
                    let deliver_event_id = validated.command()
                        .steer_message_event_id.as_deref().unwrap_or("");
                    let delivery = controls
                        .find_in_recent_ring(target.channel_id, deliver_event_id)
                        .map(|r| r.delivery)
                        .unwrap_or(SteerDeliveryMethod::Queued);

                    let (status, detail_text) = match delivery {
                        SteerDeliveryMethod::NativeSteer => (
                            ControlAckStatus::Applied,
                            "delivered_via=native_steer",
                        ),
                        SteerDeliveryMethod::CrossAdapterSteer => (
                            ControlAckStatus::Applied,
                            "delivered_via=cross_adapter_steering",
                        ),
                        SteerDeliveryMethod::CancelAndMerge => (
                            ControlAckStatus::Applied,
                            "delivered_via=cancel_and_merge",
                        ),
                        SteerDeliveryMethod::Queued | SteerDeliveryMethod::PromptedNormally => (
                            ControlAckStatus::Queued,
                            "delivered_via=queued",
                        ),
                    };

                    let ack = OneShotControlAck {
                        format: COMMAND_ACK_FORMAT.into(),
                        version: VERSION,
                        ack_id: Uuid::new_v4(),
                        command_id: validated.command().command_id,
                        command_fingerprint: validated.fingerprint().to_string(),
                        control: OneShotControlKind::Steer,
                        operator_pubkey: validated.command().operator_pubkey.clone(),
                        target: target.clone(),
                        command_seq: validated.command().seq,
                        seq: controls.ack_seq,
                        acked_at: Utc::now().timestamp() as u64,
                        status,
                        reason: None,
                        detail: Some(ControlAckExcerpt {
                            truncated: false,
                            text: detail_text.into(),
                        }),
                    };

                    let acked_at = ack.acked_at;
                    let _ = ready.complete_one_shot(permit, &validated, &ack);
                    controls.ack_seq += 1;
                    controls.controls_acked += 1;

                    let acked_ms = chrono::Utc::now().timestamp_millis();
                    let latency_ms = (acked_ms - received_ms).max(0) as u64;
                    tracing::info!(
                        command_id = %ack.command_id,
                        control = "steer",
                        status = %format!("{:?}", ack.status),
                        acked_epoch_millis = %acked_ms,
                        sidecar_latency_ms = %latency_ms,
                        controls_received = %controls.controls_received,
                        controls_acked = %controls.controls_acked,
                        controls_refused = %controls.controls_refused,
                        controls_expired = %controls.controls_expired,
                        pause_active = %controls.pause_active,
                        "control acked"
                    );

                    if let Ok(builder) = build_encrypted_ack_frame(keys, owner_pubkey_hex, &ack, acked_at) {
                        publish_ack(publisher, builder, keys, owner_pubkey_hex);
                    }
                }
                Ok(ClaimOutcome::DuplicatePending) => {}
                Ok(ClaimOutcome::DuplicateCompleted { ack_json: stored_ack_json, acked_at }) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stored_ack_json) {
                        if let Ok(stored) = serde_json::from_value::<OneShotControlAck>(v) {
                            if let Ok(builder) = build_encrypted_ack_frame(keys, owner_pubkey_hex, &stored, acked_at as u64) {
                                publish_ack(publisher, builder, keys, owner_pubkey_hex);
                            }
                        }
                    }
                }
                _ => { controls.controls_refused += 1; }
            }
        }
    }
}

// ── pause-lease ──────────────────────────────────────────────────

fn handle_pause_lease(
    controls: &mut AgentControls,
    keys: &Keys,
    owner_pubkey_hex: &str,
    publisher: RelayEventPublisher,
    validated: ValidatedPauseLeaseTransition,
    received_ms: i64,
) {
    let ready = match controls.store.as_ready() {
        Some(s) => s,
        None => { controls.controls_refused += 1; return; }
    };
    let ack_seq = controls.ack_seq + 1;
    let transition = validated.transition().clone();

    match ready.apply_pause_transition(&validated, |queue_hold| {
        let qs = match *queue_hold {
            QueueHoldState::HoldQueue => CoreQueueHoldState::Paused,
            QueueHoldState::Running => CoreQueueHoldState::Running,
        };
        let ack = PauseLeaseAck {
            format: PAUSE_LEASE_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            transition_id: transition.transition_id,
            transition_fingerprint: validated.fingerprint().to_string(),
            lease_id: transition.lease_id,
            generation: transition.generation,
            transition: transition.transition,
            operator_pubkey: transition.operator_pubkey.clone(),
            target: transition.target.clone(),
            transition_seq: transition.seq,
            seq: ack_seq,
            acked_at: Utc::now().timestamp() as u64,
            status: PauseLeaseAckStatus::Applied,
            queue_state: qs,
            detail: None,
        };
        serde_json::to_value(&ack).unwrap_or_default()
    }) {
        Ok(LeaseOutcome::Applied(permit)) => {
            controls.queue_hold = permit.queue_hold;
            controls.pause_active = if permit.queue_hold == QueueHoldState::HoldQueue { 1 } else { 0 };
            controls.ack_seq += 1;
            controls.controls_acked += 1;

            let core_qs = match permit.queue_hold {
                QueueHoldState::HoldQueue => CoreQueueHoldState::Paused,
                QueueHoldState::Running => CoreQueueHoldState::Running,
            };
            let ack = PauseLeaseAck {
                format: PAUSE_LEASE_ACK_FORMAT.into(),
                version: VERSION,
                ack_id: Uuid::new_v4(),
                transition_id: transition.transition_id,
                transition_fingerprint: validated.fingerprint().to_string(),
                lease_id: transition.lease_id,
                generation: transition.generation,
                transition: transition.transition,
                operator_pubkey: transition.operator_pubkey.clone(),
                target: transition.target.clone(),
                transition_seq: transition.seq,
                seq: controls.ack_seq,
                acked_at: Utc::now().timestamp() as u64,
                status: PauseLeaseAckStatus::Applied,
                queue_state: core_qs,
                detail: None,
            };

            let acked_ms = chrono::Utc::now().timestamp_millis();
            let latency_ms = (acked_ms - received_ms).max(0) as u64;
            let reason = format!("{:?}", ack.transition);

            if ack.queue_state == CoreQueueHoldState::Paused {
                tracing::info!(
                    lease_id = %ack.lease_id,
                    generation = %ack.generation,
                    reason = %reason,
                    "pause hold"
                );
            } else {
                tracing::info!(
                    lease_id = %ack.lease_id,
                    generation = %ack.generation,
                    reason = %reason,
                    "pause released"
                );
            }

            tracing::info!(
                transition_id = %ack.transition_id,
                lease_id = %ack.lease_id,
                generation = %ack.generation,
                transition = %reason,
                status = %format!("{:?}", ack.status),
                acked_epoch_millis = %acked_ms,
                sidecar_latency_ms = %latency_ms,
                controls_received = %controls.controls_received,
                controls_acked = %controls.controls_acked,
                controls_refused = %controls.controls_refused,
                controls_expired = %controls.controls_expired,
                pause_active = %controls.pause_active,
                "control acked"
            );

            if let Ok(builder) = build_encrypted_ack_frame(keys, owner_pubkey_hex, &ack, ack.acked_at) {
                publish_ack(publisher, builder, keys, owner_pubkey_hex);
            }
        }
        Ok(LeaseOutcome::ExactDuplicate { ack_json }) => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ack_json) {
                if let Ok(stored) = serde_json::from_value::<PauseLeaseAck>(v) {
                    if let Ok(builder) = build_encrypted_ack_frame(keys, owner_pubkey_hex, &stored, stored.acked_at) {
                        publish_ack(publisher, builder, keys, owner_pubkey_hex);
                    }
                }
            }
        }
        Ok(LeaseOutcome::LeaseConflict) | Ok(LeaseOutcome::Expired)
        | Ok(LeaseOutcome::AuthorityConflict) => {
            controls.controls_refused += 1;
            tracing::info!("pause lease conflict/expired/authority");
        }
        Err(e) => {
            controls.controls_refused += 1;
            tracing::warn!("apply_pause_transition error: {e}");
        }
    }
}

// ── ack frame construction ───────────────────────────────────────

fn build_encrypted_ack_frame<T: serde::Serialize>(
    keys: &Keys,
    owner_pubkey_hex: &str,
    ack: &T,
    acked_at: u64,
) -> Result<nostr::EventBuilder, String> {
    let owner_pk = PublicKey::from_hex(owner_pubkey_hex).map_err(|e| format!("{e}"))?;
    let encrypted = encrypt_observer_payload(keys, &owner_pk, ack).map_err(|e| format!("{e}"))?;
    let agent_pubkey_hex = keys.public_key().to_hex();
    buzz_sdk::build_agent_observer_frame(
        owner_pubkey_hex,
        &agent_pubkey_hex,
        OBSERVER_FRAME_TELEMETRY,
        &encrypted,
    )
    .map(|builder| builder.custom_created_at(nostr::Timestamp::from(acked_at)))
    .map_err(|e| format!("{e}"))
}

fn publish_ack(
    publisher: RelayEventPublisher,
    builder: nostr::EventBuilder,
    keys: &Keys,
    owner_pubkey_hex: &str,
) {
    let signed = match builder.sign_with_keys(keys) {
        Ok(event) => event,
        Err(error) => {
            tracing::warn!("failed to sign ack event: {error}");
            return;
        }
    };
    let publisher = publisher.clone();
    let _owner = owner_pubkey_hex.to_string();
    tokio::spawn(async move {
        if let Err(error) = publisher.publish_event(signed).await {
            tracing::warn!("relay observer event dropped: {error}");
        }
    });
}

fn signal_in_flight_task(pool: &mut AgentPool, channel_id: Uuid, mode: ControlSignal) -> bool {
    if pool.channel_control_is_ambiguous(channel_id) {
        return false;
    }
    let entry = pool
        .task_map_mut()
        .values_mut()
        .find(|m| m.channel_id == Some(channel_id));
    if let Some(meta) = entry {
        if let Some(tx) = meta.control_tx.take() {
            tracing::info!(channel = %channel_id, ?mode, "control signal sent to in-flight task");
            let _ = tx.send(mode);
            return true;
        }
    }
    false
}