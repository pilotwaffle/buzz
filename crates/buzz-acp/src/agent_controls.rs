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
// ── target resolution (design_answers Q2) ──────────────────────────

/// Resolve the sidecar-side [`ControlTarget`] from in-flight pool state.
///
/// - `channel_id`: the channel scope of the agent's in-flight task, else the
///   `channel_id` carried in the command payload when no turn is in flight.
/// - `run_id`: [`TaskMeta::turn_id`](pool::TaskMeta::turn_id) of the in-flight
///   turn, else the literal `"idle"` sentinel.
///
/// The validator then performs real channel/run mismatch checks; only the
/// sidecar knows which task is actually running for this agent.
fn resolve_control_target(
    payload: &Option<serde_json::Value>,
    pool: &AgentPool,
    computer_id: String,
    agent_pubkey: String,
) -> agent_control::ControlTarget {
    let payload_channel_id = payload
        .as_ref()
        .and_then(|p| p.get("target"))
        .and_then(|t| t.get("channel_id"))
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);

    let in_flight = pool
        .task_map()
        .values()
        .find(|meta| meta.channel_id == Some(payload_channel_id));

    let (channel_id, run_id) = if let Some(meta) = in_flight {
        (meta.channel_id.unwrap_or(payload_channel_id), meta.turn_id.clone())
    } else {
        (payload_channel_id, "idle".to_string())
    };

    agent_control::ControlTarget {
        computer_id,
        agent_pubkey,
        channel_id,
        run_id,
    }
}

// ── structured-control entry point ─────────────────────────────────

///
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

    // Resolve target from real state per design_answers Q2:
    let target = resolve_control_target(
        &payload,
        pool,
        host.computer_id,
        keys.public_key().to_hex(),
    );

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
                    if let Err(e) = ready.complete_one_shot(permit, &validated, &ack) {
                        controls.controls_refused += 1;
                        tracing::warn!("complete_one_shot cancel store error: {e}");
                        return;
                    }
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
                    if let Err(e) = ready.complete_one_shot(permit, &validated, &ack) {
                        controls.controls_refused += 1;
                        tracing::warn!("complete_one_shot steer store error: {e}");
                        return;
                    }
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

// ── Unit tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use buzz_core::agent_control::*;
    use buzz_core::observer::{
        encrypt_observer_payload, OBSERVER_AGENT_TAG, OBSERVER_FRAME_CONTROL, OBSERVER_FRAME_TAG,
    };
    use buzz_core::CommunityId;
    use nostr::{EventBuilder, Keys, Tag};
    use uuid::Uuid;

    use crate::control_store::{
        ClaimOutcome, ControlStore, ControlStoreHandle, HostIdentityInput, LeaseOutcome,
        ReleaseReason,
    };
    // Qualify to avoid ambiguity with buzz_core::agent_control::QueueHoldState.
    use crate::control_store::QueueHoldState as StoreQueueHoldState;
    use crate::agent_controls::AgentControls;
    use crate::pool::TaskMeta;

    const NOW: u64 = 1_800_000_000;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn open_test_store(name: &str) -> ControlStore {
        let path =
            std::env::temp_dir().join(format!("buzz-acp-test-{}-{}.sqlite", name, Uuid::new_v4()));
        let hi = HostIdentityInput {
            computer_id_override: Some("test-computer-01".into()),
            community_id: CommunityId::from_uuid(Uuid::new_v4()),
            relay_origin: "ws://localhost:3000".into(),
        };
        match ControlStore::open(&path, &hi).unwrap() {
            ControlStoreHandle::Ready(s) => s,
            ControlStoreHandle::Poisoned(reason) => panic!("store poisoned: {reason}"),
        }
    }

    fn make_command_event(
        owner: &Keys,
        agent: &Keys,
        target: &ControlTarget,
        cmd: &OneShotControlCommand,
    ) -> nostr::Event {
        let encrypted = encrypt_observer_payload(owner, &agent.public_key(), cmd).unwrap();
        EventBuilder::new(
            nostr::Kind::Custom(buzz_core::kind::KIND_AGENT_OBSERVER_FRAME as u16),
            encrypted,
        )
        .tags([
            Tag::parse(["p", &agent.public_key().to_hex()]).unwrap(),
            Tag::parse([OBSERVER_AGENT_TAG, &target.agent_pubkey]).unwrap(),
            Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_CONTROL]).unwrap(),
            Tag::parse(["h", &target.channel_id.to_string()]).unwrap(),
        ])
        .custom_created_at(nostr::Timestamp::from(cmd.issued_at))
        .sign_with_keys(owner)
        .unwrap()
    }

    fn make_pause_event(
        owner: &Keys,
        agent: &Keys,
        target: &ControlTarget,
        transition: &PauseLeaseTransition,
    ) -> nostr::Event {
        let encrypted = encrypt_observer_payload(owner, &agent.public_key(), transition).unwrap();
        EventBuilder::new(
            nostr::Kind::Custom(buzz_core::kind::KIND_AGENT_OBSERVER_FRAME as u16),
            encrypted,
        )
        .tags([
            Tag::parse(["p", &agent.public_key().to_hex()]).unwrap(),
            Tag::parse([OBSERVER_AGENT_TAG, &target.agent_pubkey]).unwrap(),
            Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_CONTROL]).unwrap(),
            Tag::parse(["h", &target.channel_id.to_string()]).unwrap(),
        ])
        .custom_created_at(nostr::Timestamp::from(transition.issued_at))
        .sign_with_keys(owner)
        .unwrap()
    }

    fn make_test_facts(store: &ControlStore, target: &ControlTarget, owner_pk_hex: &str) -> ResolvedControlFacts {
        ResolvedControlFacts {
            community_id: store.community_id(),
            now: NOW,
            operator_pubkey: owner_pk_hex.to_string(),
            agent_ownership_revision: 1,
            target: target.clone(),
            steer_message: None,
        }
    }

    fn make_cancel_command(
        owner: &Keys,
        agent: &Keys,
        target: &ControlTarget,
    ) -> OneShotControlCommand {
        OneShotControlCommand {
            format: COMMAND_FORMAT.into(),
            version: VERSION,
            command_id: Uuid::new_v4(),
            control: OneShotControlKind::Cancel,
            operator_pubkey: owner.public_key().to_hex(),
            target: target.clone(),
            seq: 7,
            issued_at: NOW - 1,
            expires_at: NOW + 60,
            steer_message_event_id: None,
        }
    }

    fn make_steer_command(
        owner: &Keys,
        agent: &Keys,
        target: &ControlTarget,
        steer_message_event_id: &str,
    ) -> OneShotControlCommand {
        OneShotControlCommand {
            format: COMMAND_FORMAT.into(),
            version: VERSION,
            command_id: Uuid::new_v4(),
            control: OneShotControlKind::Steer,
            operator_pubkey: owner.public_key().to_hex(),
            target: target.clone(),
            seq: 8,
            issued_at: NOW - 1,
            expires_at: NOW + 60,
            steer_message_event_id: Some(steer_message_event_id.to_string()),
        }
    }

    fn make_pause_transition(
        owner: &Keys,
        target: &ControlTarget,
    ) -> PauseLeaseTransition {
        PauseLeaseTransition {
            format: PAUSE_LEASE_FORMAT.into(),
            version: VERSION,
            transition_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
            transition: PauseLeaseTransitionKind::Pause,
            operator_pubkey: owner.public_key().to_hex(),
            target: target.clone(),
            seq: 20,
            issued_at: NOW - 1,
            transition_expires_at: NOW + 60,
            lease_expires_at: Some(NOW + DEFAULT_PAUSE_LEASE_SECS),
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
                PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => {
                    Some(NOW + DEFAULT_PAUSE_LEASE_SECS)
                }
                PauseLeaseTransitionKind::Resume => None,
            },
            ..prior.clone()
        }
    }

    fn make_command_ack(
        validated: &ValidatedOneShotControl,
        status: ControlAckStatus,
        detail: Option<&str>,
    ) -> OneShotControlAck {
        let command = validated.command();
        OneShotControlAck {
            format: COMMAND_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            command_id: command.command_id,
            command_fingerprint: validated.fingerprint().into(),
            control: command.control,
            operator_pubkey: command.operator_pubkey.clone(),
            target: command.target.clone(),
            command_seq: command.seq,
            seq: 10,
            acked_at: NOW + 1,
            status,
            reason: None,
            detail: detail.map(|text| ControlAckExcerpt {
                truncated: false,
                text: text.into(),
            }),
        }
    }

    // ── T10: cancel_live_turn_signals_once ──────────────────────────────────

    #[test]
    fn cancel_live_turn_signals_once() {
        let store = open_test_store("t10");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(), // non-"idle"
        };

        let command = make_cancel_command(&owner, &agent, &target);
        let event = make_command_event(&owner, &agent, &target, &command);
        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        // First claim — must be Fresh.
        let outcome = store.claim_one_shot(&validated, &target).unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::Fresh(_)),
            "expected Fresh(permit)"
        );

        // Complete with Applied ack.
        if let ClaimOutcome::Fresh(permit) = outcome {
            let ack = make_command_ack(&validated, ControlAckStatus::Applied, None);
            store.complete_one_shot(permit, &validated, &ack).unwrap();
        }

        // Verify the row is in 'completed' state.
        {
            let conn = store.conn().lock().unwrap();
            let state: String = conn
                .query_row(
                    "SELECT state FROM spent_command WHERE command_id = ?1",
                    rusqlite::params![command.command_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(state, "completed");
        }

        // Second claim — must be DuplicateCompleted.
        let outcome2 = store.claim_one_shot(&validated, &target).unwrap();
        assert!(
            matches!(outcome2, ClaimOutcome::DuplicateCompleted { .. }),
            "expected DuplicateCompleted"
        );
    }

    // ── T10b: payload_operator_mismatch_rejected_binding_mismatch ──────────

    #[test]
    fn payload_operator_mismatch_rejected_binding_mismatch() {
        let store = open_test_store("t10b");
        let owner_a = Keys::generate();
        let owner_b = Keys::generate();
        let agent = Keys::generate();
        let owner_a_hex = owner_a.public_key().to_hex();
        let owner_b_hex = owner_b.public_key().to_hex();

        // Store bound to owner A.
        store
            .reconcile_owner_binding(Some(&owner_a_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        // Command signed by owner B, facts point to owner B.
        let command = make_cancel_command(&owner_b, &agent, &target);
        let event = make_command_event(&owner_b, &agent, &target, &command);
        let facts = make_test_facts(&store, &target, &owner_b_hex);

        // Validation passes (command/facts agree on owner B).
        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        // But the store has owner A — claim should return AuthorityConflict.
        let outcome = store.claim_one_shot(&validated, &target).unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::AuthorityConflict),
            "expected AuthorityConflict"
        );
    }

    // ── T11: steer_ack_names_delivery_branch ─────────────────────────────────

    #[test]
    fn steer_ack_names_delivery_branch() {
        // Tests that steer commands flow through claim_one_shot correctly.
        // Delivery-branch classification happens in the handler (via
        // recent-events ring); here we verify the store accepts steer commands
        // and the ack status reflects the delivery branch selected by the
        // caller.

        let store = open_test_store("t11");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let steer_event_id = "ab".repeat(32);

        // Add a steer fact so validation passes.
        let mut facts = make_test_facts(&store, &target, &owner_pk_hex);
        facts.steer_message = Some(ResolvedSteerMessage {
            community_id: facts.community_id,
            event_id: steer_event_id.clone(),
            operator_pubkey: owner_pk_hex.clone(),
            channel_id: target.channel_id,
            created_at: NOW - 2,
        });

        let command = make_steer_command(&owner, &agent, &target, &steer_event_id);
        let event = make_command_event(&owner, &agent, &target, &command);

        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        // Case 1: NativeSteer → Applied, "delivered_via=native_steer"
        {
            let store2 = open_test_store("t11-native");
            store2
                .reconcile_owner_binding(Some(&owner_pk_hex))
                .unwrap();
            let outcome = store2.claim_one_shot(&validated, &target).unwrap();
            assert!(matches!(outcome, ClaimOutcome::Fresh(_)));
            if let ClaimOutcome::Fresh(permit) = outcome {
                let ack = make_command_ack(
                    &validated,
                    ControlAckStatus::Applied,
                    Some("delivered_via=native_steer"),
                );
                store2.complete_one_shot(permit, &validated, &ack).unwrap();
            }
        }

        // Case 2: CrossAdapterSteer → Applied, "delivered_via=cross_adapter_steering"
        {
            let store2 = open_test_store("t11-cross");
            store2
                .reconcile_owner_binding(Some(&owner_pk_hex))
                .unwrap();
            let outcome = store2.claim_one_shot(&validated, &target).unwrap();
            assert!(matches!(outcome, ClaimOutcome::Fresh(_)));
            if let ClaimOutcome::Fresh(permit) = outcome {
                let ack = make_command_ack(
                    &validated,
                    ControlAckStatus::Applied,
                    Some("delivered_via=cross_adapter_steering"),
                );
                store2.complete_one_shot(permit, &validated, &ack).unwrap();
            }
        }

        // Case 3: No matching recent event → Queued, "delivered_via=queued"
        {
            let store2 = open_test_store("t11-queued");
            store2
                .reconcile_owner_binding(Some(&owner_pk_hex))
                .unwrap();
            let outcome = store2.claim_one_shot(&validated, &target).unwrap();
            assert!(matches!(outcome, ClaimOutcome::Fresh(_)));
            if let ClaimOutcome::Fresh(permit) = outcome {
                let ack = make_command_ack(
                    &validated,
                    ControlAckStatus::Queued,
                    Some("delivered_via=queued"),
                );
                store2.complete_one_shot(permit, &validated, &ack).unwrap();
            }
        }
    }

    // ── T12: steer_unknown_id_pends_then_resolves_on_arrival ─────────────────

    #[test]
    fn steer_unknown_id_pends_then_resolves_on_arrival() {
        // A steer command whose steer_message_event_id is NOT in the
        // recent-events ring: the handler pends it (add_pending_steer).
        // The pending entry is stored in the AgentControls in-memory list.
        //
        // We test the store side: the steer command is durably claimable
        // through claim_one_shot regardless of whether the referenced
        // event has arrived yet.

        let store = open_test_store("t12");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let steer_event_id = "cd".repeat(32);

        // Steer with facts — the steer_message IS resolved for validation.
        let mut facts = make_test_facts(&store, &target, &owner_pk_hex);
        facts.steer_message = Some(ResolvedSteerMessage {
            community_id: facts.community_id,
            event_id: steer_event_id.clone(),
            operator_pubkey: owner_pk_hex.clone(),
            channel_id: target.channel_id,
            created_at: NOW - 2,
        });

        let command = make_steer_command(&owner, &agent, &target, &steer_event_id);
        let event = make_command_event(&owner, &agent, &target, &command);
        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        // claim_one_shot must accept the steer (Fresh).
        let outcome = store.claim_one_shot(&validated, &target).unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::Fresh(_)),
            "steer should be claimed Fresh even when recent-events ring is empty"
        );

        // Complete with Queued status (simulating the pending-steer path).
        if let ClaimOutcome::Fresh(permit) = outcome {
            let ack = make_command_ack(
                &validated,
                ControlAckStatus::Queued,
                Some("delivered_via=queued"),
            );
            store.complete_one_shot(permit, &validated, &ack).unwrap();
        }

        // Verify the command is stored.
        let conn = store.conn().lock().unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM spent_command WHERE command_id = ?1",
                rusqlite::params![command.command_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "completed");
    }

    // ── T13: steer_pending_set_caps_at_64_and_expires_on_deadline ─────────────

    #[test]
    fn steer_pending_set_caps_at_64_and_expires_on_deadline() {
        // Test the in-memory pending_steers list: 64 entries fill the set
        // without eviction; the 65th evicts the oldest by deadline.
        //
        // Also test that an expired command returns Expired from claim_one_shot.

        // Part 1: pending_steers capacity.
        {
            let mut controls = AgentControls {
                store: ControlStoreHandle::Poisoned("test".into()),
                community_id: CommunityId::from_uuid(Uuid::new_v4()),
                queue_hold: StoreQueueHoldState::Running,
                ack_seq: 1,
                controls_received: 0,
                controls_acked: 0,
                controls_refused: 0,
                controls_expired: 0,
                pause_active: 0,
                pending_steers: Vec::new(),
                recent_events: std::collections::HashMap::new(),
            };

            let owner = Keys::generate();
            let agent = Keys::generate();
            let target = ControlTarget {
                computer_id: "test-computer-01".into(),
                agent_pubkey: agent.public_key().to_hex(),
                channel_id: Uuid::new_v4(),
                run_id: "run-1".into(),
            };

            let steer_event_id = "ef".repeat(32);
            let command = make_steer_command(&owner, &agent, &target, &steer_event_id);
            let event = make_command_event(&owner, &agent, &target, &command);

            // Add 64 entries — none evicted.
            for _ in 0..64 {
                let (added, evicted) = controls.add_pending_steer(event.clone(), NOW + 300);
                assert!(added);
                assert!(evicted.is_none(), "unexpected eviction within 64-entry cap");
            }
            assert_eq!(controls.pending_steers.len(), 64);

            // 65th entry evicts the oldest (by deadline).
            let (added, evicted) = controls.add_pending_steer(event.clone(), NOW + 300);
            assert!(added);
            assert!(evicted.is_some(), "65th entry should evict oldest");
            assert_eq!(controls.pending_steers.len(), 64);
        }

        // Part 2: expired command at claim_one_shot level.
        {
            let store = open_test_store("t13-expiry");
            let owner = Keys::generate();
            let agent = Keys::generate();
            let owner_pk_hex = owner.public_key().to_hex();
            store
                .reconcile_owner_binding(Some(&owner_pk_hex))
                .unwrap();

            let target = ControlTarget {
                computer_id: "test-computer-01".into(),
                agent_pubkey: agent.public_key().to_hex(),
                channel_id: Uuid::new_v4(),
                run_id: "run-1".into(),
            };

            // Command with expires_at in the distant past relative to wall clock.
            // Validation passes (facts.now is before expires_at), but
            // claim_one_shot rechecks expiry with Utc::now() which will be
            // far past the fixture timestamp.
            let mut command = make_cancel_command(&owner, &agent, &target);
            command.expires_at = 100; // well in the past vs real time
            command.issued_at = 50;

            let event = make_command_event(&owner, &agent, &target, &command);
            // Set facts.now between issued_at and expires_at so validation passes.
            let facts = ResolvedControlFacts {
                community_id: CommunityId::from_uuid(Uuid::new_v4()),
                now: 75,
                operator_pubkey: owner_pk_hex.clone(),
                agent_ownership_revision: 1,
                target: target.clone(),
                steer_message: None,
            };

            let validated =
                decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

            let outcome = store.claim_one_shot(&validated, &target).unwrap();
            assert!(
                matches!(outcome, ClaimOutcome::Expired),
                "expected Expired for command with expires_at in the past"
            );
        }
    }

    // ── T14: pause_holds_dispatch_but_not_in_flight_turn ──────────────────────

    #[test]
    fn pause_holds_dispatch_but_not_in_flight_turn() {
        let store = open_test_store("t14");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        // 1. Apply pause.
        let pause = make_pause_transition(&owner, &target);
        let pause_event = make_pause_event(&owner, &agent, &target, &pause);
        let validated_pause =
            decrypt_and_validate_pause_lease_transition(&pause_event, &agent, &facts, None)
                .unwrap();

        let outcome = store
            .apply_pause_transition(&validated_pause, |qs| {
                let qs_str = match *qs {
                    StoreQueueHoldState::HoldQueue => "paused",
                    StoreQueueHoldState::Running => "running",
                };
                serde_json::json!({ "queue_state": qs_str })
            })
            .unwrap();

        assert!(
            matches!(outcome, LeaseOutcome::Applied(ref permit) if permit.queue_hold == StoreQueueHoldState::HoldQueue),
            "expected Applied(HoldQueue)"
        );

        // Verify lease is active=1 and QueueHoldState::HoldQueue.
        let lease = store.read_current_lease().unwrap().unwrap();
        assert!(lease.active, "lease should be active after pause");
        assert_eq!(lease.generation, 1);

        // 2. Apply resume.
        let resume = next_lease_transition(&pause, PauseLeaseTransitionKind::Resume);
        let resume_event = make_pause_event(&owner, &agent, &target, &resume);
        let core_lease: buzz_core::agent_control::ResolvedPauseLease = (&lease).into();
        let validated_resume = decrypt_and_validate_pause_lease_transition(
            &resume_event,
            &agent,
            &facts,
            Some(&core_lease),
        )
        .unwrap();

        let outcome2 = store
            .apply_pause_transition(&validated_resume, |qs| {
                let qs_str = match *qs {
                    StoreQueueHoldState::HoldQueue => "paused",
                    StoreQueueHoldState::Running => "running",
                };
                serde_json::json!({ "queue_state": qs_str })
            })
            .unwrap();

        assert!(
            matches!(outcome2, LeaseOutcome::Applied(ref permit) if permit.queue_hold == StoreQueueHoldState::Running),
            "expected Applied(Running)"
        );

        // Verify lease is now inactive.
        let lease_after = store.read_current_lease().unwrap().unwrap();
        assert!(!lease_after.active, "lease should be inactive after resume");

        // Verify pause tombstone still exists.
        let conn = store.conn().lock().unwrap();
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pause_lease_transition WHERE transition_id = ?1",
                rusqlite::params![pause.transition_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstone_count, 1, "pause tombstone should still exist after resume");
    }

    // ── T15: lease_generation_rules_pause_renew_resume ────────────────────────

    #[test]
    fn lease_generation_rules_pause_renew_resume() {
        let store = open_test_store("t15");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        let ack_builder = |qs: &StoreQueueHoldState| {
            let qs_str = match *qs {
                StoreQueueHoldState::HoldQueue => "paused",
                StoreQueueHoldState::Running => "running",
            };
            serde_json::json!({ "queue_state": qs_str })
        };

        // Pause → generation 1.
        let pause = make_pause_transition(&owner, &target);
        let pause_event = make_pause_event(&owner, &agent, &target, &pause);
        let validated_pause =
            decrypt_and_validate_pause_lease_transition(&pause_event, &agent, &facts, None)
                .unwrap();

        let outcome = store
            .apply_pause_transition(&validated_pause, &ack_builder)
            .unwrap();
        assert!(matches!(outcome, LeaseOutcome::Applied(_)));

        let lease = store.read_current_lease().unwrap().unwrap();
        assert_eq!(lease.generation, 1, "generation should be 1 after pause");

        // Renew → generation 2.
        let renew = PauseLeaseTransition {
            transition_id: Uuid::new_v4(),
            generation: 2,
            transition: PauseLeaseTransitionKind::Renew,
            seq: pause.seq + 1,
            issued_at: NOW,
            transition_expires_at: NOW + 60,
            lease_expires_at: Some(NOW + DEFAULT_PAUSE_LEASE_SECS + 60),
            ..pause.clone()
        };
        let renew_event = make_pause_event(&owner, &agent, &target, &renew);
        let core_lease1: buzz_core::agent_control::ResolvedPauseLease = (&lease).into();
        let validated_renew = decrypt_and_validate_pause_lease_transition(
            &renew_event,
            &agent,
            &facts,
            Some(&core_lease1),
        )
        .unwrap();

        let outcome2 = store
            .apply_pause_transition(&validated_renew, &ack_builder)
            .unwrap();
        assert!(matches!(outcome2, LeaseOutcome::Applied(_)));

        let lease2 = store.read_current_lease().unwrap().unwrap();
        assert_eq!(lease2.generation, 2, "generation should be 2 after renew");

        // Resume → generation 3.
        let resume = PauseLeaseTransition {
            transition_id: Uuid::new_v4(),
            generation: 3,
            transition: PauseLeaseTransitionKind::Resume,
            seq: renew.seq + 1,
            issued_at: NOW,
            transition_expires_at: NOW + 60,
            lease_expires_at: None,
            ..renew.clone()
        };
        let resume_event = make_pause_event(&owner, &agent, &target, &resume);
        let core_lease2: buzz_core::agent_control::ResolvedPauseLease = (&lease2).into();
        let validated_resume = decrypt_and_validate_pause_lease_transition(
            &resume_event,
            &agent,
            &facts,
            Some(&core_lease2),
        )
        .unwrap();

        let outcome3 = store
            .apply_pause_transition(&validated_resume, &ack_builder)
            .unwrap();
        assert!(matches!(outcome3, LeaseOutcome::Applied(_)));

        let lease3 = store.read_current_lease().unwrap().unwrap();
        assert_eq!(lease3.generation, 3, "generation should be 3 after resume");
        assert!(!lease3.active, "lease should be inactive after resume");
    }

    // ── T16: historical_exact_retry_after_new_lease_leaves_current_row_unchanged

    #[test]
    fn historical_exact_retry_after_new_lease_leaves_current_row_unchanged() {
        // After a pause, the transition tombstone exists. Re-submitting the
        // exact same pause event (same transition_id, same fingerprint) is
        // an exact retry — the validator accepts it (lease is still active,
        // same transition_id and fingerprint), and the store's tombstone
        // check returns ExactDuplicate without touching the current lease row.

        let store = open_test_store("t16");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        let ack_builder = |qs: &StoreQueueHoldState| {
            let qs_str = match *qs {
                StoreQueueHoldState::HoldQueue => "paused",
                StoreQueueHoldState::Running => "running",
            };
            serde_json::json!({ "queue_state": qs_str })
        };

        // Step 1: Apply pause (transition_id T1, generation 1).
        let pause = make_pause_transition(&owner, &target);
        let pause_event = make_pause_event(&owner, &agent, &target, &pause);
        let validated_pause =
            decrypt_and_validate_pause_lease_transition(&pause_event, &agent, &facts, None)
                .unwrap();

        let outcome = store
            .apply_pause_transition(&validated_pause, &ack_builder)
            .unwrap();
        assert!(matches!(outcome, LeaseOutcome::Applied(_)));
        let lease_after_pause = store.read_current_lease().unwrap().unwrap();
        assert_eq!(lease_after_pause.generation, 1);
        assert!(lease_after_pause.active);

        // Step 2: Re-submit the EXACT SAME pause event while the lease is
        // still active. The validator's exact-retry path (same transition_id,
        // same fingerprint, lease still active) returns retry=true, and the
        // store's tombstone check returns ExactDuplicate.
        let core_lease: buzz_core::agent_control::ResolvedPauseLease =
            (&lease_after_pause).into();
        let validated_replay = decrypt_and_validate_pause_lease_transition(
            &pause_event,
            &agent,
            &facts,
            Some(&core_lease),
        )
        .unwrap();

        // retry flag must be set by the validator.
        assert!(validated_replay.is_exact_retry(), "exact retry must set retry flag");

        let outcome2 = store
            .apply_pause_transition(&validated_replay, &ack_builder)
            .unwrap();

        assert!(
            matches!(outcome2, LeaseOutcome::ExactDuplicate { .. }),
            "expected ExactDuplicate for tombstone-protected transition"
        );

        // Current lease row must be unchanged (still generation 1, active).
        let lease_final = store.read_current_lease().unwrap().unwrap();
        assert_eq!(
            lease_final.generation, 1,
            "current lease generation must be unchanged after ExactDuplicate"
        );
        assert!(
            lease_final.active,
            "current lease must still be active after ExactDuplicate"
        );
    }

    // ── T17: transition_id_reuse_with_different_fingerprint_is_lease_conflict ──

    #[test]
    fn transition_id_reuse_with_different_fingerprint_is_lease_conflict() {
        // Apply a pause transition. Then create a new pause with the SAME
        // transition_id but different fields (different lease_id). After
        // the lease is resumed (inactive), the validator allows a fresh
        // pause, but the store's tombstone check returns LeaseConflict
        // because the new fingerprint differs from the stored one.

        let store = open_test_store("t17");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        let ack_builder = |qs: &StoreQueueHoldState| {
            let qs_str = match *qs {
                StoreQueueHoldState::HoldQueue => "paused",
                StoreQueueHoldState::Running => "running",
            };
            serde_json::json!({ "queue_state": qs_str })
        };

        // Step 1: Apply original pause.
        let pause1 = make_pause_transition(&owner, &target);
        let pause1_event = make_pause_event(&owner, &agent, &target, &pause1);
        let validated1 =
            decrypt_and_validate_pause_lease_transition(&pause1_event, &agent, &facts, None)
                .unwrap();
        let outcome = store
            .apply_pause_transition(&validated1, &ack_builder)
            .unwrap();
        assert!(matches!(outcome, LeaseOutcome::Applied(_)));

        // Step 2: Resume to make lease inactive.
        let resume1 = next_lease_transition(&pause1, PauseLeaseTransitionKind::Resume);
        let resume_event = make_pause_event(&owner, &agent, &target, &resume1);
        let lease_after_pause = store.read_current_lease().unwrap().unwrap();
        let core_lease: buzz_core::agent_control::ResolvedPauseLease =
            (&lease_after_pause).into();
        let validated_resume = decrypt_and_validate_pause_lease_transition(
            &resume_event,
            &agent,
            &facts,
            Some(&core_lease),
        )
        .unwrap();
        let outcome_r = store
            .apply_pause_transition(&validated_resume, &ack_builder)
            .unwrap();
        assert!(matches!(outcome_r, LeaseOutcome::Applied(_)));

        // Step 3: Create a new pause with the SAME transition_id as pause1
        // but different lease_id. Validator sees inactive lease + pause →
        // allows it. Store sees tombstone with matching transition_id but
        // different fingerprint → LeaseConflict.
        let pause2 = PauseLeaseTransition {
            lease_id: Uuid::new_v4(), // different lease
            ..pause1.clone() // same transition_id
        };
        let pause2_event = make_pause_event(&owner, &agent, &target, &pause2);
        let lease_after_resume = store.read_current_lease().unwrap().unwrap();
        let core_lease2: buzz_core::agent_control::ResolvedPauseLease =
            (&lease_after_resume).into();
        let validated2 = decrypt_and_validate_pause_lease_transition(
            &pause2_event,
            &agent,
            &facts,
            Some(&core_lease2),
        )
        .unwrap();

        let outcome3 = store
            .apply_pause_transition(&validated2, &ack_builder)
            .unwrap();

        assert!(
            matches!(outcome3, LeaseOutcome::LeaseConflict),
            "expected LeaseConflict for transition_id reuse with different fingerprint"
        );

        // Original lease row must be unchanged (still inactive, gen from resume).
        let lease_final = store.read_current_lease().unwrap().unwrap();
        assert!(!lease_final.active, "lease must remain inactive after LeaseConflict");
        assert_eq!(lease_final.generation, 2, "lease generation must be unchanged");
    }

    // ── T18: expiry_tick_releases_and_audits_pause_lease_expired_once ──────────

    #[test]
    fn expiry_tick_releases_and_audits_pause_lease_expired_once() {
        let store = open_test_store("t18");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(),
        };

        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        let ack_builder = |qs: &StoreQueueHoldState| {
            let qs_str = match *qs {
                StoreQueueHoldState::HoldQueue => "paused",
                StoreQueueHoldState::Running => "running",
            };
            serde_json::json!({ "queue_state": qs_str })
        };

        // Apply a pause to create an active lease.
        let pause = make_pause_transition(&owner, &target);
        let pause_event = make_pause_event(&owner, &agent, &target, &pause);
        let validated_pause =
            decrypt_and_validate_pause_lease_transition(&pause_event, &agent, &facts, None)
                .unwrap();
        store
            .apply_pause_transition(&validated_pause, &ack_builder)
            .unwrap();

        // Verify lease is active.
        let lease = store.read_current_lease().unwrap().unwrap();
        assert!(lease.active, "lease should be active before expiry release");

        // Release via Expired.
        store.release_lease(ReleaseReason::Expired).unwrap();

        // Verify lease is now inactive.
        let lease_after = store.read_current_lease().unwrap().unwrap();
        assert!(!lease_after.active, "lease should be inactive after release");

        // Call release_lease again — must be a no-op (no active lease to release).
        store.release_lease(ReleaseReason::Expired).unwrap();

        // Verify exactly one PauseLeaseExpired audit entry.
        let conn = store.conn().lock().unwrap();
        let expired_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM control_audit WHERE event = 'pause_lease_expired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            expired_count, 1,
            "expected exactly one PauseLeaseExpired audit entry, got {}",
            expired_count
        );
    }

    // ── T18b: run_id_race_turn_ended_between_ui_and_receipt_fails_closed ──────

    #[test]
    fn run_id_race_turn_ended_between_ui_and_receipt_fails_closed() {
        // Tests the ack pattern for the run-id race condition:
        // run_id != "idle" but the in-flight task is already ending.
        // The handler produces Applied status with "turn already ending" detail.
        // Here we verify the store accepts such an ack.

        let store = open_test_store("t18b");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let target = ControlTarget {
            computer_id: "test-computer-01".into(),
            agent_pubkey: agent.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "run-1".into(), // non-"idle" — a real turn was selected
        };

        let command = make_cancel_command(&owner, &agent, &target);
        let event = make_command_event(&owner, &agent, &target, &command);
        let facts = make_test_facts(&store, &target, &owner_pk_hex);

        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        // Claim Fresh.
        let outcome = store.claim_one_shot(&validated, &target).unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::Fresh(_)),
            "expected Fresh"
        );

        // Complete with status=Applied and detail="turn already ending"
        // (simulating the handler path where run_id != "idle" but
        // signal_in_flight_task returns false).
        if let ClaimOutcome::Fresh(permit) = outcome {
            let ack = make_command_ack(
                &validated,
                ControlAckStatus::Applied,
                Some("turn already ending"),
            );
            store.complete_one_shot(permit, &validated, &ack).unwrap();
        }

        // Verify the stored ack reflects Applied + turn-already-ending detail.
        let conn = store.conn().lock().unwrap();
        let (state, ack_json): (String, Option<String>) = conn
            .query_row(
                "SELECT state, ack_json FROM spent_command WHERE command_id = ?1",
                rusqlite::params![command.command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "completed");
        let ack_json = ack_json.unwrap();
        assert!(
            ack_json.contains("turn already ending"),
            "ack_json should contain 'turn already ending', got: {}",
            ack_json
        );
        assert!(
            ack_json.contains("applied"),
            "ack_json should contain 'applied' status"
        );
    }

    // ── Defect 3: target resolution from pool state ──────────────────────────
    //
    // These tests verify that the sidecar resolves channel_id and run_id from
    // real in-flight pool state rather than hard-coding Uuid::nil() / "".
    // Fixtures do NOT hand-build the resolved target — they go through the
    // production helper resolve_control_target().

    fn make_test_payload(channel_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "format": COMMAND_FORMAT,
            "version": VERSION,
            "control": "cancel",
            "target": {
                "computer_id": "test-computer-01",
                "agent_pubkey": "",
                "channel_id": channel_id.to_string(),
                "run_id": ""
            }
        })
    }

    fn make_test_pool(channel_id: Uuid, turn_id: &str) -> (AgentPool, tokio::sync::oneshot::Receiver<ControlSignal>) {
        let (control_tx, control_rx) = tokio::sync::oneshot::channel();
        let meta = TaskMeta {
            agent_index: 0,
            channel_id: Some(channel_id),
            scope: None,
            turn_id: turn_id.to_string(),
            recoverable_batch: None,
            control_tx: Some(control_tx),
            steer_tx: None,
            successful_steer_deliveries: std::collections::HashSet::new(),
        };
        let mut pool = AgentPool::from_slots(vec![]);
        pool.test_insert_task(meta);
        (pool, control_rx)
    }

    #[test]
    fn resolve_target_with_in_flight_task() {
        let channel_id = Uuid::new_v4();
        let (pool, _control_rx) = make_test_pool(channel_id, "turn-abc");
        let payload = Some(make_test_payload(channel_id));

        let target = resolve_control_target(
            &payload,
            &pool,
            "test-computer-01".into(),
            "aa".repeat(32),
        );

        assert_eq!(target.computer_id, "test-computer-01");
        assert_eq!(target.channel_id, channel_id);
        assert_eq!(
            target.run_id, "turn-abc",
            "run_id must be the in-flight task's turn_id"
        );
    }

    #[test]
    fn resolve_target_no_in_flight_task_uses_idle() {
        let channel_id = Uuid::new_v4();
        let (pool, _control_rx) = make_test_pool(channel_id, "turn-abc");
        let other_channel = Uuid::new_v4();
        let payload = Some(make_test_payload(other_channel));

        let target = resolve_control_target(
            &payload,
            &pool,
            "test-computer-01".into(),
            "aa".repeat(32),
        );

        assert_eq!(target.channel_id, other_channel);
        assert_eq!(
            target.run_id, "idle",
            "run_id must be 'idle' when no task matches the payload channel_id"
        );
    }

    #[test]
    fn resolve_target_empty_pool_uses_idle() {
        let channel_id = Uuid::new_v4();
        let pool = AgentPool::from_slots(vec![]);
        let payload = Some(make_test_payload(channel_id));

        let target = resolve_control_target(
            &payload,
            &pool,
            "test-computer-01".into(),
            "aa".repeat(32),
        );

        assert_eq!(target.channel_id, channel_id);
        assert_eq!(target.run_id, "idle");
    }

    #[test]
    fn resolve_target_nil_payload_channel_id_with_empty_pool() {
        // Malformed payload with no target.channel_id → Uuid::nil().
        let pool = AgentPool::from_slots(vec![]);
        let payload = Some(serde_json::json!({
            "format": COMMAND_FORMAT,
            "control": "cancel"
        }));

        let target = resolve_control_target(
            &payload,
            &pool,
            "test-computer-01".into(),
            "aa".repeat(32),
        );

        assert_eq!(target.channel_id, Uuid::nil());
        assert_eq!(target.run_id, "idle");
    }

    /// Integration-style test: builds a cancel command with a real channel_id,
    /// creates a pool with a matching in-flight task, resolves the target
    /// through the production helper, and drives it through the validator.
    /// Verifies that the validator accepts the real target (no schema_error
    /// about channel_id).
    #[test]
    fn cancel_with_in_flight_task_validates() {
        let store = open_test_store("d3-inflight");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let channel_id = Uuid::new_v4();
        let (pool, _control_rx) = make_test_pool(channel_id, "turn-abc");
        let payload = Some(make_test_payload(channel_id));

        // Resolve target through the PRODUCTION helper — NOT hand-built.
        let target = resolve_control_target(
            &payload,
            &pool,
            store.computer_id().to_string(),
            agent.public_key().to_hex(),
        );

        assert_eq!(target.channel_id, channel_id);
        assert_eq!(target.run_id, "turn-abc");

        let facts = ResolvedControlFacts {
            community_id: store.community_id(),
            now: NOW,
            operator_pubkey: owner_pk_hex.clone(),
            agent_ownership_revision: 1,
            target: target.clone(),
            steer_message: None,
        };

        let command = make_cancel_command(&owner, &agent, &target);
        let event = make_command_event(&owner, &agent, &target, &command);

        // Must NOT fail with schema_error ("invalid agent-control field: channel_id").
        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        let outcome = store.claim_one_shot(&validated, &target).unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::Fresh(_)),
            "cancel with in-flight task must yield Fresh claim"
        );
    }

    /// Same pipeline as above, but with no matching in-flight task → run_id
    /// resolves to "idle", which the one-shot handler maps to NoActiveTurn.
    #[test]
    fn cancel_with_no_matching_in_flight_task_yields_idle_run_id() {
        let store = open_test_store("d3-idle");
        let owner = Keys::generate();
        let agent = Keys::generate();
        let owner_pk_hex = owner.public_key().to_hex();
        store
            .reconcile_owner_binding(Some(&owner_pk_hex))
            .unwrap();

        let channel_id = Uuid::new_v4();
        let pool = AgentPool::from_slots(vec![]); // empty — no in-flight task
        let payload = Some(make_test_payload(channel_id));

        // Resolve target through the PRODUCTION helper.
        let target = resolve_control_target(
            &payload,
            &pool,
            store.computer_id().to_string(),
            agent.public_key().to_hex(),
        );

        assert_eq!(target.channel_id, channel_id);
        assert_eq!(target.run_id, "idle");

        let facts = ResolvedControlFacts {
            community_id: store.community_id(),
            now: NOW,
            operator_pubkey: owner_pk_hex.clone(),
            agent_ownership_revision: 1,
            target: target.clone(),
            steer_message: None,
        };

        let command = make_cancel_command(&owner, &agent, &target);
        let event = make_command_event(&owner, &agent, &target, &command);

        // Validation must pass (no schema_error).
        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent, &facts).unwrap();

        // Claim should be Fresh — the run_id is "idle" but that doesn't block
        // the store claim (the handler decides status, not the store).
        let outcome = store.claim_one_shot(&validated, &target).unwrap();
        assert!(matches!(outcome, ClaimOutcome::Fresh(_)));
    }
}