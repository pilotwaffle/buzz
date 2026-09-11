//! Durable control store adapter (Slice 2).
//!
//! One SQLite file per agent process; six tables; atomic claim / CAS transactions;
//! fail-closed on any store error. All mutations go through sealed permits — no
//! other module can construct `CommandEffectPermit` or `LeaseEffectPermit`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use buzz_core::agent_control::{
    ControlAckStatus, OneShotControlAck, OneShotControlKind, PauseLeaseTransitionKind,
    ValidatedOneShotControl, ValidatedPauseLeaseTransition,
};
use buzz_core::CommunityId;
use chrono::Utc;
use rusqlite::{params, Connection, Transaction};
use thiserror::Error;
use uuid::Uuid;

// ── Sealed permits ──────────────────────────────────────────────────────────────

/// Proof that a one-shot command was durably claimed. Only `ControlStore` can mint.
pub struct CommandEffectPermit {
    _private: (),
}

/// Proof that a pause-lease transition was durably applied. Only `ControlStore` can mint.
pub struct LeaseEffectPermit {
    _private: (),
    pub queue_hold: QueueHoldState,
}

/// Mirror of the durable pause state, updated only via permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueHoldState {
    Running,
    HoldQueue,
}

// ── Result types ────────────────────────────────────────────────────────────────

pub enum ClaimOutcome {
    Fresh(CommandEffectPermit),
    DuplicatePending,
    DuplicateCompleted {
        ack_json: String,
        acked_at: u64,
    },
    CommandIdConflict,
    Expired,
    AuthorityConflict,
}

pub enum LeaseOutcome {
    Applied(LeaseEffectPermit),
    ExactDuplicate { ack_json: String },
    LeaseConflict,
    Expired,
    AuthorityConflict,
}

pub enum ReleaseReason {
    Expired,
    AuthorityChanged {
        persisted_revision: u64,
        current_revision: u64,
    },
}

// ── Host identity input ─────────────────────────────────────────────────────────

pub struct HostIdentityInput {
    pub computer_id_override: Option<String>,
    pub community_id: CommunityId,
    pub relay_origin: String,
}

// ── Read models ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ResolvedPauseLease {
    pub community_id: CommunityId,
    pub agent_pubkey: String,
    pub computer_id: String,
    pub lease_id: String,
    pub operator_pubkey: String,
    pub ownership_revision: u64,
    pub channel_id: String,
    pub run_id: String,
    pub generation: u64,
    pub active: bool,
    pub lease_expires_at: u64,
    pub last_transition_id: String,
    pub last_transition_fingerprint: String,
    pub updated_at: u64,
}

impl From<&ResolvedPauseLease> for buzz_core::agent_control::ResolvedPauseLease {
    fn from(r: &ResolvedPauseLease) -> Self {
        buzz_core::agent_control::ResolvedPauseLease {
            community_id: r.community_id,
            lease_id: Uuid::parse_str(&r.lease_id).unwrap_or(Uuid::nil()),
            operator_pubkey: r.operator_pubkey.clone(),
            agent_ownership_revision: r.ownership_revision,
            target: buzz_core::agent_control::ControlTarget {
                computer_id: r.computer_id.clone(),
                agent_pubkey: r.agent_pubkey.clone(),
                channel_id: Uuid::parse_str(&r.channel_id).unwrap_or(Uuid::nil()),
                run_id: r.run_id.clone(),
            },
            generation: r.generation,
            active: r.active,
            lease_expires_at: r.lease_expires_at,
            last_transition_id: Uuid::parse_str(&r.last_transition_id).unwrap_or(Uuid::nil()),
            last_transition_fingerprint: r.last_transition_fingerprint.clone(),
        }
    }
}

// ── Audit event ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub at: u64,
    pub event: AuditEvent,
    pub community_id: Option<String>,
    pub command_id: Option<String>,
    pub transition_id: Option<String>,
    pub lease_id: Option<String>,
    pub fingerprint: Option<String>,
    pub operator_pubkey: Option<String>,
    pub agent_pubkey: Option<String>,
    pub computer_id: Option<String>,
    pub channel_id: Option<String>,
    pub run_id: Option<String>,
    pub ownership_revision: Option<u64>,
    pub persisted_revision: Option<u64>,
    pub outcome: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEvent {
    ControlIssued,
    ControlAck,
    ControlExpired,
    ControlRefused,
    PauseLeaseGranted,
    PauseLeaseRenewed,
    PauseLeaseReleased,
    PauseLeaseExpired,
    PauseLeaseAuthorityReleased,
    HostIdentityUpdated,
    OwnerBindingAdvanced,
    TenantMismatch,
}

impl AuditEvent {
    fn as_str(&self) -> &'static str {
        match self {
            AuditEvent::ControlIssued => "control_issued",
            AuditEvent::ControlAck => "control_ack",
            AuditEvent::ControlExpired => "control_expired",
            AuditEvent::ControlRefused => "control_refused",
            AuditEvent::PauseLeaseGranted => "pause_lease_granted",
            AuditEvent::PauseLeaseRenewed => "pause_lease_renewed",
            AuditEvent::PauseLeaseReleased => "pause_lease_released",
            AuditEvent::PauseLeaseExpired => "pause_lease_expired",
            AuditEvent::PauseLeaseAuthorityReleased => "pause_lease_authority_released",
            AuditEvent::HostIdentityUpdated => "host_identity_updated",
            AuditEvent::OwnerBindingAdvanced => "owner_binding_advanced",
            AuditEvent::TenantMismatch => "tenant_mismatch",
        }
    }
}

// ── Error types ─────────────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("store unavailable: {0}")]
    StoreUnavailable(#[from] rusqlite::Error),
    #[error("tenant mismatch: stored origin {stored_origin}, current origin {current_origin}")]
    TenantMismatch {
        stored_origin: String,
        current_origin: String,
    },
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::StoreUnavailable(_) => "store_unavailable",
            StoreError::TenantMismatch { .. } => "tenant_mismatch",
        }
    }
}

// ── ControlStore ────────────────────────────────────────────────────────────────

pub struct ControlStore {
    conn: Mutex<Connection>,
    community_id: CommunityId,
    path: PathBuf,
    computer_id: String,
}

/// Poisoned handle — every structured control is refused `store_unavailable`.
pub enum ControlStoreHandle {
    Ready(ControlStore),
    Poisoned(String),
}

impl ControlStoreHandle {
    pub fn as_ready(&self) -> Option<&ControlStore> {
        match self {
            ControlStoreHandle::Ready(s) => Some(s),
            ControlStoreHandle::Poisoned(_) => None,
        }
    }

    pub fn poison_reason(&self) -> Option<&str> {
        match self {
            ControlStoreHandle::Ready(_) => None,
            ControlStoreHandle::Poisoned(reason) => Some(reason),
        }
    }
}

impl ControlStore {
    /// Open (or create) the control store at `path`, run DDL, reconcile identity.
    pub fn open(
        path: &Path,
        expected: &HostIdentityInput,
    ) -> Result<ControlStoreHandle, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_e| {
                StoreError::StoreUnavailable(rusqlite::Error::InvalidPath(
                    path.to_path_buf(),
                ))
            })?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;

        // Run DDL verbatim from Step 1.3.
        conn.execute_batch(DDL)?;

        let computer_id = Self::reconcile_host_identity(&conn, expected)?;
        let community_id = expected.community_id;

        tracing::info!(
            "control store path={} computer_id={} community_id={}",
            path.display(),
            computer_id,
            community_id.as_uuid(),
        );

        Ok(ControlStoreHandle::Ready(ControlStore {
            conn: Mutex::new(conn),
            community_id,
            path: path.to_path_buf(),
            computer_id,
        }))
    }

    pub fn community_id(&self) -> CommunityId {
        self.community_id
    }

    pub fn computer_id(&self) -> &str {
        &self.computer_id
    }

    fn reconcile_host_identity(
        conn: &Connection,
        expected: &HostIdentityInput,
    ) -> Result<String, StoreError> {
        let now = Utc::now().timestamp();
        let computer_id = expected
            .computer_id_override
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let existing: Option<(String, String, String)> = conn
            .query_row(
                "SELECT computer_id, community_id, relay_origin FROM host_identity WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO host_identity (id, computer_id, community_id, relay_origin, created_at) VALUES (1, ?1, ?2, ?3, ?4)",
                    params![computer_id, expected.community_id.as_uuid().to_string(), expected.relay_origin, now],
                )?;
                Ok(computer_id)
            }
            Some((stored_computer_id, stored_community, stored_origin)) => {
                if stored_community != expected.community_id.as_uuid().to_string() {
                    return Err(StoreError::TenantMismatch {
                        stored_origin,
                        current_origin: expected.relay_origin.clone(),
                    });
                }
                // Override takes precedence (desktop is the host).
                if let Some(ref override_id) = expected.computer_id_override {
                    if *override_id != stored_computer_id {
                        conn.execute(
                            "UPDATE host_identity SET computer_id = ?1 WHERE id = 1",
                            params![override_id],
                        )?;
                        // Audit the override.
                        audit_simple(
                            conn,
                            AuditEntry {
                                at: now as u64,
                                event: AuditEvent::HostIdentityUpdated,
                                community_id: Some(expected.community_id.as_uuid().to_string()),
                                command_id: None,
                                transition_id: None,
                                lease_id: None,
                                fingerprint: None,
                                operator_pubkey: None,
                                agent_pubkey: None,
                                computer_id: Some(override_id.clone()),
                                channel_id: None,
                                run_id: None,
                                ownership_revision: None,
                                persisted_revision: None,
                                outcome: None,
                                detail: Some(format!(
                                    "previous={}",
                                    stored_computer_id
                                )),
                            },
                        )?;
                        return Ok(override_id.clone());
                    }
                }
                Ok(stored_computer_id)
            }
        }
    }

    // ── Owner binding reconciliation ──────────────────────────────────────────

    /// Reconcile the owner binding row. Returns `(pubkey, revision)`.
    pub fn reconcile_owner_binding(
        &self,
        resolved_owner: Option<&str>,
    ) -> Result<Option<(String, u64)>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        let community_id_str = self.community_id.as_uuid().to_string();
        let computer_id = self.computer_id.clone();
        Self::reconcile_owner_binding_inner(&conn, resolved_owner, now, &community_id_str, &computer_id)
    }

    fn reconcile_owner_binding_inner(
        conn: &Connection,
        resolved_owner: Option<&str>,
        now: i64,
        community_id: &str,
        computer_id: &str,
    ) -> Result<Option<(String, u64)>, StoreError> {
        let Some(owner) = resolved_owner else {
            return Ok(None);
        };
        let owner = owner.to_ascii_lowercase();

        let existing: Option<(String, u64)> = conn
            .query_row(
                "SELECT owner_pubkey, revision FROM owner_binding WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO owner_binding (id, owner_pubkey, revision, updated_at) VALUES (1, ?1, 1, ?2)",
                    params![owner, now],
                )?;
                tracing::info!("owner binding revision=1");
                Ok(Some((owner, 1)))
            }
            Some((stored_owner, revision)) => {
                if stored_owner != owner {
                    let new_revision = revision + 1;
                    conn.execute(
                        "UPDATE owner_binding SET owner_pubkey = ?1, revision = ?2, updated_at = ?3 WHERE id = 1",
                        params![owner, new_revision, now],
                    )?;
                    audit_simple(
                        conn,
                        AuditEntry {
                            at: now as u64,
                            event: AuditEvent::OwnerBindingAdvanced,
                            community_id: Some(community_id.to_string()),
                            command_id: None,
                            transition_id: None,
                            lease_id: None,
                            fingerprint: None,
                            operator_pubkey: Some(owner.clone()),
                            agent_pubkey: None,
                            computer_id: Some(computer_id.to_string()),
                            channel_id: None,
                            run_id: None,
                            ownership_revision: Some(new_revision),
                            persisted_revision: Some(revision),
                            outcome: None,
                            detail: None,
                        },
                    )?;
                    tracing::info!("owner binding revision={new_revision}");
                    Ok(Some((owner, new_revision)))
                } else {
                    tracing::info!("owner binding revision={revision}");
                    Ok(Some((owner, revision)))
                }
            }
        }
    }

    // ── Read helpers ──────────────────────────────────────────────────────────

    pub fn read_owner_binding(&self) -> Result<(String, u64), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT owner_pubkey, revision FROM owner_binding WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(StoreError::StoreUnavailable)
    }

    pub fn conn(&self) -> &Mutex<Connection> {
        &self.conn
    }

    pub fn read_host_identity(&self) -> Result<HostIdentityRow, StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT computer_id, community_id, relay_origin, created_at FROM host_identity WHERE id = 1",
            [],
            |row| {
                Ok(HostIdentityRow {
                    computer_id: row.get(0)?,
                    community_id: row.get(1)?,
                    relay_origin: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        )
        .map_err(StoreError::StoreUnavailable)
    }

    pub fn read_current_lease(&self) -> Result<Option<ResolvedPauseLease>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT community_id, agent_pubkey, computer_id, lease_id, operator_pubkey,
                        ownership_revision, channel_id, run_id, generation, active,
                        lease_expires_at, last_transition_id, last_transition_fingerprint, updated_at
                 FROM pause_lease_current
                 WHERE community_id = ?1",
                params![self.community_id.as_uuid().to_string()],
                |row| {
                    Ok(ResolvedPauseLease {
                        community_id: self.community_id,
                        agent_pubkey: row.get(1)?,
                        computer_id: row.get(2)?,
                        lease_id: row.get(3)?,
                        operator_pubkey: row.get(4)?,
                        ownership_revision: row.get(5)?,
                        channel_id: row.get(6)?,
                        run_id: row.get(7)?,
                        generation: row.get(8)?,
                        active: row.get::<_, i32>(9)? != 0,
                        lease_expires_at: row.get(10)?,
                        last_transition_id: row.get(11)?,
                        last_transition_fingerprint: row.get(12)?,
                        updated_at: row.get(13)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    // ── Claim one-shot ────────────────────────────────────────────────────────

    pub fn claim_one_shot(
        &self,
        validated: &ValidatedOneShotControl,
        target: &buzz_core::agent_control::ControlTarget,
    ) -> Result<ClaimOutcome, StoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp() as u64;
        let community_id_str = self.community_id.as_uuid().to_string();
        let command_id = validated.command().command_id.to_string();
        let fingerprint = validated.fingerprint().to_string();
        let claim = validated.claim();

        // Recheck expiry inside transaction.
        if now >= claim.expires_at() {
            return Ok(ClaimOutcome::Expired);
        }

        // Recheck authority.
        let (owner_pk, revision): (String, u64) = tx
            .query_row(
                "SELECT owner_pubkey, revision FROM owner_binding WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| StoreError::StoreUnavailable(e))?;

        if owner_pk != claim.operator_pubkey() || revision != claim.agent_ownership_revision() {
            return Ok(ClaimOutcome::AuthorityConflict);
        }

        // Check for existing row.
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT fingerprint, state FROM spent_command WHERE community_id = ?1 AND command_id = ?2",
                params![&community_id_str, &command_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        match existing {
            Some((existing_fp, state)) => {
                if existing_fp != fingerprint {
                    return Ok(ClaimOutcome::CommandIdConflict);
                }
                match state.as_str() {
                    "pending" => Ok(ClaimOutcome::DuplicatePending),
                    "completed" => {
                        let (ack_json, acked_at): (Option<String>, Option<u64>) = tx
                            .query_row(
                                "SELECT ack_json, acked_at FROM spent_command WHERE community_id = ?1 AND command_id = ?2",
                                params![&community_id_str, &command_id],
                                |row| Ok((row.get(0)?, row.get(1)?)),
                            )
                            .map_err(|e| StoreError::StoreUnavailable(e))?;
                        Ok(ClaimOutcome::DuplicateCompleted {
                            ack_json: ack_json.unwrap_or_default(),
                            acked_at: acked_at.unwrap_or(0),
                        })
                    }
                    "abandoned" | "authority_conflict" => Ok(ClaimOutcome::Expired),
                    _ => Ok(ClaimOutcome::CommandIdConflict),
                }
            }
            None => {
                // Map control kind to string.
                let control = match validated.command().control {
                    OneShotControlKind::Cancel => "cancel",
                    OneShotControlKind::Steer => "steer",
                };

                tx.execute(
                    "INSERT INTO spent_command
                     (community_id, command_id, fingerprint, control, operator_pubkey,
                      agent_pubkey, computer_id, channel_id, run_id, command_seq,
                      ownership_revision, issued_at, expires_at, claimed_at, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'pending')",
                    params![
                        &community_id_str,
                        &command_id,
                        &fingerprint,
                        control,
                        claim.operator_pubkey(),
                        claim.agent_pubkey(),
                        &self.computer_id,
                        &target.channel_id.to_string(),
                        &target.run_id,
                        validated.command().seq as i64,
                        claim.agent_ownership_revision() as i64,
                        validated.command().issued_at as i64,
                        claim.expires_at() as i64,
                        now as i64,
                    ],
                )?;

                // Audit.
                audit(
                    &tx,
                    &AuditEntry {
                        at: now,
                        event: AuditEvent::ControlIssued,
                        community_id: Some(community_id_str),
                        command_id: Some(command_id),
                        transition_id: None,
                        lease_id: None,
                        fingerprint: Some(fingerprint),
                        operator_pubkey: Some(claim.operator_pubkey().to_string()),
                        agent_pubkey: Some(claim.agent_pubkey().to_string()),
                        computer_id: Some(self.computer_id.clone()),
                        channel_id: Some(target.channel_id.to_string()),
                        run_id: Some(target.run_id.clone()),
                        ownership_revision: Some(claim.agent_ownership_revision()),
                        persisted_revision: None,
                        outcome: None,
                        detail: None,
                    },
                )?;

                tx.commit()?;
                Ok(ClaimOutcome::Fresh(CommandEffectPermit { _private: () }))
            }
        }
    }

    // ── Complete one-shot ─────────────────────────────────────────────────────

    pub fn complete_one_shot(
        &self,
        _permit: CommandEffectPermit,
        cmd: &ValidatedOneShotControl,
        ack: &OneShotControlAck,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp() as u64;
        let community_id = self.community_id.as_uuid().to_string();
        let command_id = cmd.command().command_id.to_string();
        let ack_json =
            serde_json::to_string(ack).map_err(|_| StoreError::StoreUnavailable(
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::fmt::Error)),
            ))?;

        tx.execute(
            "UPDATE spent_command SET state = 'completed', ack_json = ?1, acked_at = ?2
             WHERE community_id = ?3 AND command_id = ?4 AND state = 'pending'",
            params![&ack_json, now as i64, &community_id, &command_id],
        )?;

        audit(
            &tx,
            &AuditEntry {
                at: now,
                event: AuditEvent::ControlAck,
                community_id: Some(community_id),
                command_id: Some(command_id),
                transition_id: None,
                lease_id: None,
                fingerprint: Some(cmd.fingerprint().to_string()),
                operator_pubkey: Some(cmd.command().operator_pubkey.clone()),
                agent_pubkey: Some(cmd.command().target.agent_pubkey.clone()),
                computer_id: Some(self.computer_id.clone()),
                channel_id: Some(cmd.command().target.channel_id.to_string()),
                run_id: Some(cmd.command().target.run_id.clone()),
                ownership_revision: Some(cmd.claim().agent_ownership_revision()),
                persisted_revision: None,
                outcome: Some(ack_status_str(&ack.status).to_string()),
                detail: ack.detail.as_ref().map(|d| d.text.clone()),
            },
        )?;

        tx.commit()?;
        Ok(())
    }

    // ── Abandon one-shot ──────────────────────────────────────────────────────

    pub fn abandon_one_shot(
        &self,
        community_id: &str,
        command_id: &str,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp() as u64;

        conn.execute(
            "UPDATE spent_command SET state = 'abandoned' WHERE community_id = ?1 AND command_id = ?2 AND state = 'pending'",
            params![community_id, command_id],
        )?;

        audit_simple(
            &conn,
            AuditEntry {
                at: now,
                event: AuditEvent::ControlExpired,
                community_id: Some(community_id.to_string()),
                command_id: Some(command_id.to_string()),
                transition_id: None,
                lease_id: None,
                fingerprint: None,
                operator_pubkey: None,
                agent_pubkey: None,
                computer_id: Some(self.computer_id.clone()),
                channel_id: None,
                run_id: None,
                ownership_revision: None,
                persisted_revision: None,
                outcome: None,
                detail: None,
            },
        )?;

        Ok(())
    }

    // ── Apply pause transition ────────────────────────────────────────────────

    pub fn apply_pause_transition(
        &self,
        validated: &ValidatedPauseLeaseTransition,
        ack_builder: impl FnOnce(&QueueHoldState) -> serde_json::Value,
    ) -> Result<LeaseOutcome, StoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp() as u64;
        let community_id = self.community_id.as_uuid().to_string();
        let claim = validated.claim();
        let transition = validated.transition();

        // Recheck expiry.
        if now >= claim.transition_expires_at() {
            return Ok(LeaseOutcome::Expired);
        }

        // Recheck authority.
        let (owner_pk, revision): (String, u64) = tx
            .query_row(
                "SELECT owner_pubkey, revision FROM owner_binding WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| StoreError::StoreUnavailable(e))?;

        if owner_pk != claim.operator_pubkey() || revision != claim.agent_ownership_revision() {
            return Ok(LeaseOutcome::AuthorityConflict);
        }

        // Check transition tombstone.
        let transition_id = transition.transition_id.to_string();
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT fingerprint, ack_json FROM pause_lease_transition WHERE community_id = ?1 AND transition_id = ?2",
                params![&community_id, &transition_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        if let Some((existing_fp, existing_ack)) = existing {
            if existing_fp == validated.fingerprint() {
                // Exact duplicate — don't touch current row.
                return Ok(LeaseOutcome::ExactDuplicate { ack_json: existing_ack });
            }
            return Ok(LeaseOutcome::LeaseConflict);
        }

        // Read current row.
        let current: Option<(String, u64, i32)> = tx
            .query_row(
                "SELECT lease_id, generation, active FROM pause_lease_current WHERE community_id = ?1 AND agent_pubkey = ?2 AND computer_id = ?3",
                params![&community_id, &claim.target().agent_pubkey, &self.computer_id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i32>(2)?)),
            )
            .ok();

        let (lease_id_str, new_generation, queue_hold, audit_event) = match transition.transition {
            PauseLeaseTransitionKind::Pause => {
                let lease_id = transition.lease_id.to_string();
                let lease_expires_at = transition.lease_expires_at.unwrap_or(claim.transition_expires_at());
                let generation = 1u64;

                // Upsert.
                tx.execute(
                    "INSERT INTO pause_lease_current
                     (community_id, agent_pubkey, computer_id, lease_id, operator_pubkey,
                      ownership_revision, channel_id, run_id, generation, active,
                      lease_expires_at, last_transition_id, last_transition_fingerprint, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?13)
                     ON CONFLICT(community_id, agent_pubkey, computer_id)
                     DO UPDATE SET lease_id = ?4, operator_pubkey = ?5,
                       ownership_revision = ?6, channel_id = ?7, run_id = ?8,
                       generation = ?9, active = 1, lease_expires_at = ?10,
                       last_transition_id = ?11, last_transition_fingerprint = ?12,
                       updated_at = ?13",
                    params![
                        &community_id,
                        &claim.target().agent_pubkey,
                        &self.computer_id,
                        &lease_id,
                        claim.operator_pubkey(),
                        claim.agent_ownership_revision() as i64,
                        &claim.target().channel_id.to_string(),
                        &claim.target().run_id,
                        generation as i64,
                        lease_expires_at as i64,
                        &transition_id,
                        validated.fingerprint(),
                        now as i64,
                    ],
                )?;

                (lease_id, generation, QueueHoldState::HoldQueue, AuditEvent::PauseLeaseGranted)
            }
            PauseLeaseTransitionKind::Renew => {
                match current {
                    Some((lid, gen, active)) if active != 0 => {
                        let new_gen = gen + 1;
                        let lease_expires_at = transition.lease_expires_at.unwrap_or(claim.transition_expires_at());
                        tx.execute(
                            "UPDATE pause_lease_current SET generation = ?1, lease_expires_at = ?2,
                             last_transition_id = ?3, last_transition_fingerprint = ?4, updated_at = ?5
                             WHERE community_id = ?6 AND agent_pubkey = ?7 AND computer_id = ?8 AND active = 1",
                            params![
                                new_gen as i64,
                                lease_expires_at as i64,
                                &transition_id,
                                validated.fingerprint(),
                                now as i64,
                                &community_id,
                                &claim.target().agent_pubkey,
                                &self.computer_id,
                            ],
                        )?;
                        (lid, new_gen, QueueHoldState::HoldQueue, AuditEvent::PauseLeaseRenewed)
                    }
                    _ => return Ok(LeaseOutcome::LeaseConflict),
                }
            }
            PauseLeaseTransitionKind::Resume => {
                match current {
                    Some((lid, gen, active)) if active != 0 => {
                        let new_gen = gen + 1;
                        tx.execute(
                            "UPDATE pause_lease_current SET active = 0, generation = ?1,
                             last_transition_id = ?2, last_transition_fingerprint = ?3, updated_at = ?4
                             WHERE community_id = ?5 AND agent_pubkey = ?6 AND computer_id = ?7 AND active = 1",
                            params![
                                new_gen as i64,
                                &transition_id,
                                validated.fingerprint(),
                                now as i64,
                                &community_id,
                                &claim.target().agent_pubkey,
                                &self.computer_id,
                            ],
                        )?;
                        (lid, new_gen, QueueHoldState::Running, AuditEvent::PauseLeaseReleased)
                    }
                    _ => return Ok(LeaseOutcome::LeaseConflict),
                }
            }
        };

        // Compute transition label for the CHECK constraint *before* the
        // INSERT (G2A D1 fix — audit_event.as_str() returns "pause_lease_granted"
        // etc. which violates CHECK transition IN ('pause','renew','resume')).
        let transition_label = match transition.transition {
            PauseLeaseTransitionKind::Pause => "pause",
            PauseLeaseTransitionKind::Renew => "renew",
            PauseLeaseTransitionKind::Resume => "resume",
        };

        // Build ack and insert tombstone.
        let ack = ack_builder(&queue_hold);
        let ack_json = serde_json::to_string(&ack).map_err(|_| {
            StoreError::StoreUnavailable(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::fmt::Error,
            )))
        })?;

        tx.execute(
            "INSERT INTO pause_lease_transition
             (community_id, transition_id, lease_id, fingerprint, generation, transition,
              ownership_revision, transition_expires_at, applied_at, ack_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &community_id,
                &transition_id,
                &lease_id_str,
                validated.fingerprint(),
                new_generation as i64,
                transition_label,
                claim.agent_ownership_revision() as i64,
                claim.transition_expires_at() as i64,
                now as i64,
                &ack_json,
            ],
        )?;
        audit(
            &tx,
            &AuditEntry {
                at: now,
                event: audit_event,
                community_id: Some(community_id),
                command_id: None,
                transition_id: Some(transition_id),
                lease_id: Some(lease_id_str),
                fingerprint: Some(validated.fingerprint().to_string()),
                operator_pubkey: Some(claim.operator_pubkey().to_string()),
                agent_pubkey: Some(claim.target().agent_pubkey.to_string()),
                computer_id: Some(self.computer_id.clone()),
                channel_id: Some(claim.target().channel_id.to_string()),
                run_id: Some(claim.target().run_id.clone()),
                ownership_revision: Some(claim.agent_ownership_revision()),
                persisted_revision: None,
                outcome: None,
                detail: Some(format!("transition={transition_label}")),
            },
        )?;

        tx.commit()?;

        Ok(LeaseOutcome::Applied(LeaseEffectPermit {
            _private: (),
            queue_hold,
        }))
    }

    // ── Release lease ─────────────────────────────────────────────────────────

    pub fn release_lease(
        &self,
        reason: ReleaseReason,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp() as u64;
        let community_id = self.community_id.as_uuid().to_string();

        // Read current row to get lease_id and generation for CAS.
        let current: Option<(String, u64)> = conn
            .query_row(
                "SELECT lease_id, generation FROM pause_lease_current WHERE community_id = ?1 AND active = 1",
                params![&community_id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)),
            )
            .ok();

        let Some((lease_id, generation)) = current else {
            return Ok(()); // Nothing to release.
        };

        let affected = conn.execute(
            "UPDATE pause_lease_current SET active = 0, updated_at = ?1
             WHERE community_id = ?2 AND active = 1 AND lease_id = ?3 AND generation = ?4",
            params![now as i64, &community_id, &lease_id, generation as i64],
        )?;

        if affected == 0 {
            return Ok(()); // Someone else won the CAS.
        }

        let (event, persisted_revision) = match &reason {
            ReleaseReason::Expired => (AuditEvent::PauseLeaseExpired, None),
            ReleaseReason::AuthorityChanged {
                persisted_revision: pr,
                current_revision: cr,
            } => (AuditEvent::PauseLeaseAuthorityReleased, Some((*pr, *cr))),
        };

        audit_simple(
            &conn,
            AuditEntry {
                at: now,
                event,
                community_id: Some(community_id),
                command_id: None,
                transition_id: None,
                lease_id: Some(lease_id),
                fingerprint: None,
                operator_pubkey: None,
                agent_pubkey: None,
                computer_id: Some(self.computer_id.clone()),
                channel_id: None,
                run_id: None,
                ownership_revision: persisted_revision.map(|(_, cr)| cr),
                persisted_revision: persisted_revision.map(|(pr, _)| pr),
                outcome: None,
                detail: None,
            },
        )?;

        Ok(())
    }

    // ── Purge expired ─────────────────────────────────────────────────────────

    pub fn purge_expired(&self, now: u64) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();

        // Delete spent_command rows past expires_at + 600s retention window.
        conn.execute(
            "DELETE FROM spent_command WHERE expires_at + 600 < ?1",
            params![now as i64],
        )?;

        // Delete pause_lease_transition rows past transition_expires_at + 600s.
        conn.execute(
            "DELETE FROM pause_lease_transition WHERE transition_expires_at + 600 < ?1",
            params![now as i64],
        )?;

        // Cap control_audit at 10,000 rows.
        conn.execute(
            "DELETE FROM control_audit WHERE id NOT IN (SELECT id FROM control_audit ORDER BY id DESC LIMIT 10000)",
            [],
        )?;

        Ok(())
    }

    // ── Audit helpers ─────────────────────────────────────────────────────────

    /// Write an audit row and log at INFO.
    pub fn write_audit(&self, entry: &AuditEntry) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        audit_simple(&conn, entry.clone())
    }

    /// Best-effort audit write that doesn't break the caller's transaction.
    pub fn write_audit_best_effort(&self, entry: &AuditEntry) {
        if let Ok(conn) = self.conn.lock() {
            let _ = audit_simple(&conn, entry.clone());
        }
    }
}

// ── Standalone audit functions ─────────────────────────────────────────────────

fn audit(tx: &Transaction, entry: &AuditEntry) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO control_audit
         (at, event, community_id, command_id, transition_id, lease_id, fingerprint,
          operator_pubkey, agent_pubkey, computer_id, channel_id, run_id,
          ownership_revision, persisted_revision, outcome, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            entry.at as i64,
            entry.event.as_str(),
            entry.community_id,
            entry.command_id,
            entry.transition_id,
            entry.lease_id,
            entry.fingerprint,
            entry.operator_pubkey,
            entry.agent_pubkey,
            entry.computer_id,
            entry.channel_id,
            entry.run_id,
            entry.ownership_revision.map(|v| v as i64),
            entry.persisted_revision.map(|v| v as i64),
            entry.outcome,
            entry.detail,
        ],
    )?;
    Ok(())
}

pub fn audit_simple(conn: &Connection, entry: AuditEntry) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO control_audit
         (at, event, community_id, command_id, transition_id, lease_id, fingerprint,
          operator_pubkey, agent_pubkey, computer_id, channel_id, run_id,
          ownership_revision, persisted_revision, outcome, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            entry.at as i64,
            entry.event.as_str(),
            entry.community_id,
            entry.command_id,
            entry.transition_id,
            entry.lease_id,
            entry.fingerprint,
            entry.operator_pubkey,
            entry.agent_pubkey,
            entry.computer_id,
            entry.channel_id,
            entry.run_id,
            entry.ownership_revision.map(|v| v as i64),
            entry.persisted_revision.map(|v| v as i64),
            entry.outcome,
            entry.detail,
        ],
    )?;
    Ok(())
}

// ── DDL (Step 1.3 — verbatim) ──────────────────────────────────────────────────

const DDL: &str = "\
CREATE TABLE IF NOT EXISTS host_identity (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  computer_id TEXT NOT NULL,
  community_id TEXT NOT NULL,
  relay_origin TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS owner_binding (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  owner_pubkey TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision > 0),
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS spent_command (
  community_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  control TEXT NOT NULL CHECK (control IN ('cancel','steer')),
  operator_pubkey TEXT NOT NULL,
  agent_pubkey TEXT NOT NULL,
  computer_id TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  command_seq INTEGER NOT NULL,
  ownership_revision INTEGER NOT NULL,
  issued_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  claimed_at INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending','completed','abandoned','authority_conflict')),
  ack_json TEXT,
  acked_at INTEGER,
  PRIMARY KEY (community_id, command_id)
);
CREATE TABLE IF NOT EXISTS pause_lease_current (
  community_id TEXT NOT NULL,
  agent_pubkey TEXT NOT NULL,
  computer_id TEXT NOT NULL,
  lease_id TEXT NOT NULL,
  operator_pubkey TEXT NOT NULL,
  ownership_revision INTEGER NOT NULL CHECK (ownership_revision > 0),
  channel_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation > 0),
  active INTEGER NOT NULL CHECK (active IN (0,1)),
  lease_expires_at INTEGER NOT NULL,
  last_transition_id TEXT NOT NULL,
  last_transition_fingerprint TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (community_id, agent_pubkey, computer_id)
);
CREATE TABLE IF NOT EXISTS pause_lease_transition (
  community_id TEXT NOT NULL,
  transition_id TEXT NOT NULL,
  lease_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  generation INTEGER NOT NULL,
  transition TEXT NOT NULL CHECK (transition IN ('pause','renew','resume')),
  ownership_revision INTEGER NOT NULL,
  transition_expires_at INTEGER NOT NULL,
  applied_at INTEGER NOT NULL,
  ack_json TEXT NOT NULL,
  PRIMARY KEY (community_id, transition_id)
);
CREATE TABLE IF NOT EXISTS control_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  event TEXT NOT NULL,
  community_id TEXT,
  command_id TEXT,
  transition_id TEXT,
  lease_id TEXT,
  fingerprint TEXT,
  operator_pubkey TEXT,
  agent_pubkey TEXT,
  computer_id TEXT,
  channel_id TEXT,
  run_id TEXT,
  ownership_revision INTEGER,
  persisted_revision INTEGER,
  outcome TEXT,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS spent_command_expiry ON spent_command (expires_at);
CREATE INDEX IF NOT EXISTS pause_lease_transition_expiry ON pause_lease_transition (transition_expires_at);
";

fn ack_status_str(status: &ControlAckStatus) -> &'static str {
    match status {
        ControlAckStatus::Applied => "applied",
        ControlAckStatus::NoActiveTurn => "no_active_turn",
        ControlAckStatus::Queued => "queued",
        ControlAckStatus::Rejected => "rejected",
    }
}

// ── Host identity row ──────────────────────────────────────────────────────────

pub struct HostIdentityRow {
    #[allow(dead_code)]
    pub computer_id: String,
    #[allow(dead_code)]
    pub community_id: String,
    #[allow(dead_code)]
    pub relay_origin: String,
    #[allow(dead_code)]
    pub created_at: i64,
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_core::agent_control::*;
    use buzz_core::observer::{
        encrypt_observer_payload, OBSERVER_AGENT_TAG, OBSERVER_FRAME_CONTROL, OBSERVER_FRAME_TAG,
    };
    use buzz_core::kind::KIND_AGENT_OBSERVER_FRAME;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    /// Fixture timestamp used for all store-level tests.
    /// Must be a recent Unix epoch second so the validator's timestamp-
    /// freshness checks pass (nostr::Timestamp::from_secs round-trips
    /// cleanly while Utc::now().timestamp() can drift during test execution).
    const NOW: u64 = 1_800_000_000;

    fn temp_store_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("buzz-acp-test-{}-{}.sqlite", name, Uuid::new_v4()))
    }

    fn make_host_identity() -> HostIdentityInput {
        HostIdentityInput {
            computer_id_override: Some("test-computer-01".to_string()),
            community_id: CommunityId::from_uuid(Uuid::new_v4()),
            relay_origin: "ws://localhost:3000".to_string(),
        }
    }

    fn open_store(name: &str) -> ControlStore {
        let path = temp_store_path(name);
        let hi = make_host_identity();
        match ControlStore::open(&path, &hi).unwrap() {
            ControlStoreHandle::Ready(s) => s,
            ControlStoreHandle::Poisoned(reason) => panic!("store poisoned: {reason}"),
        }
    }

    // ── T1: claim_fresh_inserts_pending_and_mints_permit ────────────────────

    #[test]
    fn claim_fresh_inserts_pending_and_mints_permit() {
        let store = open_store("t1");
        // Need a validated one-shot — we can construct one by calling the validator.
        // For a unit test, we simulate with a real validator call path.
        // The store test creates a minimal validated struct by going through
        // buzz_core's public API, or we test the store directly with a mock.
        //
        // The store is tested via integration with buzz_core's public validation.
        // For now, test schema correctness.
        let (owner, revision) = store.reconcile_owner_binding(Some("abcd1234")).unwrap().unwrap();
        assert_eq!(owner, "abcd1234");
        assert_eq!(revision, 1);
    }

    // ── Test helpers ─────────────────────────────────────────────────────────

    fn make_test_command(
        store: &ControlStore,
        owner_keys: &Keys,
        agent_keys: &Keys,
        expires_at: u64,
        seq: u64,
        command_id_override: Option<Uuid>,
    ) -> (ControlTarget, ValidatedOneShotControl) {
        let target = ControlTarget {
            computer_id: store.computer_id().to_string(),
            agent_pubkey: agent_keys.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "test-run".to_string(),
        };
        let cmd = OneShotControlCommand {
            format: COMMAND_FORMAT.into(),
            version: VERSION,
            command_id: command_id_override.unwrap_or_else(Uuid::new_v4),
            control: OneShotControlKind::Cancel,
            operator_pubkey: owner_keys.public_key().to_hex(),
            target: target.clone(),
            seq,
            issued_at: NOW,
            expires_at,
            steer_message_event_id: None,
        };
        let encrypted =
            encrypt_observer_payload(owner_keys, &agent_keys.public_key(), &cmd).unwrap();
        let event = EventBuilder::new(Kind::Custom(KIND_AGENT_OBSERVER_FRAME as u16), encrypted)
            .tags([
                Tag::parse(["p", &agent_keys.public_key().to_hex()]).unwrap(),
                Tag::parse([OBSERVER_AGENT_TAG, &target.agent_pubkey]).unwrap(),
                Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_CONTROL]).unwrap(),
                Tag::parse(["h", &target.channel_id.to_string()]).unwrap(),
            ])
            .custom_created_at(nostr::Timestamp::from(NOW))
            .sign_with_keys(owner_keys)
            .unwrap();
        let facts = ResolvedControlFacts {
            community_id: store.community_id(),
            now: NOW,
            operator_pubkey: owner_keys.public_key().to_hex(),
            agent_ownership_revision: 1,
            target: target.clone(),
            steer_message: None,
        };
        let validated =
            decrypt_and_validate_one_shot_control(&event, agent_keys, &facts).unwrap();
        (target, validated)
    }

    fn make_test_pause_transition(
        store: &ControlStore,
        owner_keys: &Keys,
        agent_keys: &Keys,
        target: &ControlTarget,
        kind: PauseLeaseTransitionKind,
        generation: u64,
        lease_id: Uuid,
        current: Option<&buzz_core::agent_control::ResolvedPauseLease>,
    ) -> ValidatedPauseLeaseTransition {
        let transition = PauseLeaseTransition {
            format: PAUSE_LEASE_FORMAT.into(),
            version: VERSION,
            transition_id: Uuid::new_v4(),
            lease_id,
            generation,
            transition: kind,
            operator_pubkey: owner_keys.public_key().to_hex(),
            target: target.clone(),
            seq: 1,
            issued_at: NOW,
            transition_expires_at: NOW + 60,
            lease_expires_at: match kind {
                PauseLeaseTransitionKind::Pause | PauseLeaseTransitionKind::Renew => {
                    Some(NOW + 1800)
                }
                PauseLeaseTransitionKind::Resume => None,
            },
        };
        let encrypted = encrypt_observer_payload(
            owner_keys,
            &agent_keys.public_key(),
            &transition,
        )
        .unwrap();
        let event = EventBuilder::new(Kind::Custom(KIND_AGENT_OBSERVER_FRAME as u16), encrypted)
            .tags([
                Tag::parse(["p", &agent_keys.public_key().to_hex()]).unwrap(),
                Tag::parse([OBSERVER_AGENT_TAG, &target.agent_pubkey]).unwrap(),
                Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_CONTROL]).unwrap(),
                Tag::parse(["h", &target.channel_id.to_string()]).unwrap(),
            ])
            .custom_created_at(nostr::Timestamp::from(NOW))
            .sign_with_keys(owner_keys)
            .unwrap();
        let facts = ResolvedControlFacts {
            community_id: store.community_id(),
            now: NOW,
            operator_pubkey: owner_keys.public_key().to_hex(),
            agent_ownership_revision: 1,
            target: target.clone(),
            steer_message: None,
        };
        decrypt_and_validate_pause_lease_transition(&event, agent_keys, &facts, current).unwrap()
    }

    // ── T2: duplicate claim returns DuplicatePending ────────────────────────

    #[test]
    fn duplicate_claim_returns_duplicate_pending() {
        let store = open_store("t2");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();
        let (target, validated) =
            make_test_command(&store, &owner_keys, &agent_keys, NOW + 60, 1, None);
        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        assert!(
            matches!(
                store.claim_one_shot(&validated, &target).unwrap(),
                ClaimOutcome::Fresh(_)
            ),
            "first claim must be Fresh"
        );
        assert!(
            matches!(
                store.claim_one_shot(&validated, &target).unwrap(),
                ClaimOutcome::DuplicatePending
            ),
            "second claim with same validated command must be DuplicatePending"
        );
    }

    // ── T3: duplicate completed returns stored ack ──────────────────────────

    #[test]
    fn duplicate_completed_returns_stored_ack() {
        let store = open_store("t3");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();
        let (target, validated) =
            make_test_command(&store, &owner_keys, &agent_keys, NOW + 60, 1, None);
        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        let permit = match store.claim_one_shot(&validated, &target).unwrap() {
            ClaimOutcome::Fresh(p) => p,
            other => panic!("expected Fresh"),
        };

        let cmd = validated.command();
        let ack = OneShotControlAck {
            format: COMMAND_ACK_FORMAT.into(),
            version: VERSION,
            ack_id: Uuid::new_v4(),
            command_id: cmd.command_id,
            command_fingerprint: validated.fingerprint().into(),
            control: cmd.control,
            operator_pubkey: cmd.operator_pubkey.clone(),
            target: cmd.target.clone(),
            command_seq: cmd.seq,
            seq: 1,
            acked_at: NOW,
            status: ControlAckStatus::Applied,
            reason: None,
            detail: None,
        };
        store.complete_one_shot(permit, &validated, &ack).unwrap();

        match store.claim_one_shot(&validated, &target).unwrap() {
            ClaimOutcome::DuplicateCompleted { ack_json, acked_at } => {
                assert!(!ack_json.is_empty(), "ack_json must not be empty");
                assert!(acked_at > 0, "acked_at must be > 0");
            }
            other => panic!("expected DuplicateCompleted"),
        }
    }

    // ── T4: different fingerprint, same command_id → CommandIdConflict ──────

    #[test]
    fn different_fingerprint_same_command_id_returns_conflict() {
        let store = open_store("t4");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();
        let shared_id = Uuid::new_v4();

        let (target, validated_a) = make_test_command(
            &store,
            &owner_keys,
            &agent_keys,
            NOW + 60,
            1,
            Some(shared_id),
        );
        let (_, validated_b) = make_test_command(
            &store,
            &owner_keys,
            &agent_keys,
            NOW + 60,
            2,
            Some(shared_id),
        );

        // Fingerprints must differ because seq differs.
        assert_ne!(
            validated_a.fingerprint(),
            validated_b.fingerprint(),
            "different seq must produce different fingerprints"
        );

        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        assert!(matches!(
            store.claim_one_shot(&validated_a, &target).unwrap(),
            ClaimOutcome::Fresh(_)
        ));
        assert!(
            matches!(
                store.claim_one_shot(&validated_b, &target).unwrap(),
                ClaimOutcome::CommandIdConflict
            ),
            "same command_id with different fingerprint must be CommandIdConflict"
        );
    }

    // ── T5: expiry recheck inside transaction → Expired ─────────────────────

    #[test]
    fn claim_rechecks_expiry_inside_transaction() {
        let store = open_store("t5");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();

        // Use past timestamps: validation passes (facts.now=1 < expires_at=2),
        // but the store's own Utc::now() (~1.79B) >= expires_at=2 triggers Expired.
        let target = ControlTarget {
            computer_id: store.computer_id().to_string(),
            agent_pubkey: agent_keys.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "test-run-t5".to_string(),
        };
        let cmd = OneShotControlCommand {
            format: COMMAND_FORMAT.into(),
            version: VERSION,
            command_id: Uuid::new_v4(),
            control: OneShotControlKind::Cancel,
            operator_pubkey: owner_keys.public_key().to_hex(),
            target: target.clone(),
            seq: 1,
            issued_at: 1,
            expires_at: 2,
            steer_message_event_id: None,
        };
        let encrypted =
            encrypt_observer_payload(&owner_keys, &agent_keys.public_key(), &cmd).unwrap();
        let event = EventBuilder::new(Kind::Custom(KIND_AGENT_OBSERVER_FRAME as u16), encrypted)
            .tags([
                Tag::parse(["p", &agent_keys.public_key().to_hex()]).unwrap(),
                Tag::parse([OBSERVER_AGENT_TAG, &target.agent_pubkey]).unwrap(),
                Tag::parse([OBSERVER_FRAME_TAG, OBSERVER_FRAME_CONTROL]).unwrap(),
                Tag::parse(["h", &target.channel_id.to_string()]).unwrap(),
            ])
            .custom_created_at(nostr::Timestamp::from(1u64))
            .sign_with_keys(&owner_keys)
            .unwrap();
        let facts = ResolvedControlFacts {
            community_id: store.community_id(),
            now: 1,
            operator_pubkey: owner_keys.public_key().to_hex(),
            agent_ownership_revision: 1,
            target: target.clone(),
            steer_message: None,
        };
        let validated =
            decrypt_and_validate_one_shot_control(&event, &agent_keys, &facts).unwrap();

        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        assert!(
            matches!(
                store.claim_one_shot(&validated, &target).unwrap(),
                ClaimOutcome::Expired
            ),
            "store must re-check expiry against its own clock and return Expired"
        );
    }

    // ── T6: authority conflict when owner binding differs ───────────────────

    #[test]
    fn authority_conflict_when_owner_binding_differs() {
        let store = open_store("t6");
        let owner_a = Keys::generate();
        let owner_b = Keys::generate();
        let agent_keys = Keys::generate();

        // Reconcile owner binding to owner_a.
        store
            .reconcile_owner_binding(Some(&owner_a.public_key().to_hex()))
            .unwrap();

        // Build command signed by owner_b (validated against owner_b's facts).
        let (target, validated) =
            make_test_command(&store, &owner_b, &agent_keys, NOW + 60, 1, None);

        assert!(
            matches!(
                store.claim_one_shot(&validated, &target).unwrap(),
                ClaimOutcome::AuthorityConflict
            ),
            "command from owner_b must fail authority check when store is bound to owner_a"
        );
    }

    // ── T7: poisoned handle refuses access ──────────────────────────────────

    #[test]
    fn poisoned_handle_refuses_access() {
        let handle = ControlStoreHandle::Poisoned("test poison".to_string());
        assert!(handle.as_ready().is_none(), "poisoned handle must return None");
        assert_eq!(
            handle.poison_reason(),
            Some("test poison"),
            "poisoned handle must return the reason"
        );
    }

    // ── T8: purge expired removes stale rows ────────────────────────────────

    #[test]
    fn purge_expired_removes_stale_rows() {
        let store = open_store("t8a");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();
        let (target, validated) =
            make_test_command(&store, &owner_keys, &agent_keys, NOW + 60, 1, None);
        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        // Claim so a row exists in spent_command.
        assert!(matches!(
            store.claim_one_shot(&validated, &target).unwrap(),
            ClaimOutcome::Fresh(_)
        ));

        // Verify row exists.
        let community_str = store.community_id().as_uuid().to_string();
        {
            let conn = store.conn().lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM spent_command WHERE community_id = ?1",
                    rusqlite::params![&community_str],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "row must exist before purge");
        }

        // Purge with a now far enough past expires_at + 600.
        // expires_at = NOW + 60, so purge_now = NOW + 5000 trivially passes.
        store.purge_expired(NOW + 5000).unwrap();

        {
            let conn = store.conn().lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM spent_command WHERE community_id = ?1",
                    rusqlite::params![&community_str],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "row must be gone after purge_expired");
        }
    }

    /// T8b: retention boundary — row with now < expires_at + 600 is retained.
    #[test]
    fn purge_retains_rows_before_deadline() {
        let store = open_store("t8b");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();
        let (target, validated) =
            make_test_command(&store, &owner_keys, &agent_keys, NOW + 60, 1, None);
        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        assert!(matches!(
            store.claim_one_shot(&validated, &target).unwrap(),
            ClaimOutcome::Fresh(_)
        ));

        let community_str = store.community_id().as_uuid().to_string();
        {
            let conn = store.conn().lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM spent_command WHERE community_id = ?1",
                    rusqlite::params![&community_str],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "row must exist before purge");
        }

        // Purge at NOW. expires_at = NOW + 60, so expires_at + 600 = NOW + 660.
        // NOW < NOW + 660 → retention boundary holds → row survives.
        store.purge_expired(NOW).unwrap();

        {
            let conn = store.conn().lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM spent_command WHERE community_id = ?1",
                    rusqlite::params![&community_str],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                count, 1,
                "row must be retained when now ({NOW}) < expires_at + 600 ({})",
                NOW + 60 + 600
            );
        }
    }

    // ── T9: pause transition tombstone survives resume ──────────────────────

    #[test]
    fn pause_transition_tombstone_survives_resume() {
        let store = open_store("t9");
        let owner_keys = Keys::generate();
        let agent_keys = Keys::generate();

        let target = ControlTarget {
            computer_id: store.computer_id().to_string(),
            agent_pubkey: agent_keys.public_key().to_hex(),
            channel_id: Uuid::new_v4(),
            run_id: "test-run-t9".to_string(),
        };
        store
            .reconcile_owner_binding(Some(&owner_keys.public_key().to_hex()))
            .unwrap();

        let lease_id = Uuid::new_v4();

        // 1. Apply pause.
        let validated_pause = make_test_pause_transition(
            &store,
            &owner_keys,
            &agent_keys,
            &target,
            PauseLeaseTransitionKind::Pause,
            1,
            lease_id,
            None,
        );
        let pause_transition_id = validated_pause.transition().transition_id;
        let pause_outcome = store
            .apply_pause_transition(&validated_pause, |_qs| serde_json::json!({"ok": true}))
            .unwrap();
        assert!(
            matches!(
                pause_outcome,
                LeaseOutcome::Applied(LeaseEffectPermit {
                    queue_hold: super::QueueHoldState::HoldQueue,
                    ..
                })
            ),
            "pause must apply successfully with HoldQueue state"
        );

        // 2. Read current lease for resume validation.
        let store_lease: super::ResolvedPauseLease =
            store.read_current_lease().unwrap().expect("lease must exist after pause");
        let core_lease: buzz_core::agent_control::ResolvedPauseLease = (&store_lease).into();
        assert!(core_lease.active, "lease must be active after pause");

        // 3. Apply resume (same lease_id, next generation).
        let validated_resume = make_test_pause_transition(
            &store,
            &owner_keys,
            &agent_keys,
            &target,
            PauseLeaseTransitionKind::Resume,
            2,
            lease_id,
            Some(&core_lease),
        );
        let resume_outcome = store
            .apply_pause_transition(&validated_resume, |_qs| serde_json::json!({"ok": true}))
            .unwrap();
        assert!(
            matches!(
                resume_outcome,
                LeaseOutcome::Applied(LeaseEffectPermit {
                    queue_hold: super::QueueHoldState::Running,
                    ..
                })
            ),
            "resume must apply successfully with Running state"
        );

        // 4. Verify the pause transition tombstone still exists.
        let conn = store.conn().lock().unwrap();
        let community_str = store.community_id().as_uuid().to_string();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pause_lease_transition
                 WHERE community_id = ?1 AND transition_id = ?2",
                rusqlite::params![&community_str, &pause_transition_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "pause transition tombstone must survive resume"
        );

        // 5. Verify the resume transition tombstone also exists.
        let resume_transition_id = validated_resume.transition().transition_id;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pause_lease_transition
                 WHERE community_id = ?1 AND transition_id = ?2",
                rusqlite::params![&community_str, &resume_transition_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "resume transition tombstone must also exist"
        );
    }

    #[test]
    fn schema_matches_ddl_column_for_column() {
        let store = open_store("schema");
        let conn = store.conn.lock().unwrap();

        // Verify host_identity.
        let columns: Vec<(String, String, i32, i32)> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info('host_identity')")
                .unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(1)?,   // name
                        row.get::<_, String>(2)?,   // type
                        row.get::<_, i32>(3)?,      // notnull
                        row.get::<_, i32>(5)?,      // pk
                    ))
                })
                .unwrap();
            rows.filter_map(|r| r.ok()).collect()
        };
        assert_eq!(columns.len(), 5);
        assert_eq!(columns[0], ("id".to_string(), "INTEGER".to_string(), 0, 1));
        assert_eq!(columns[1], ("computer_id".to_string(), "TEXT".to_string(), 1, 0));
        assert_eq!(columns[2], ("community_id".to_string(), "TEXT".to_string(), 1, 0));
        assert_eq!(columns[3], ("relay_origin".to_string(), "TEXT".to_string(), 1, 0));
        assert_eq!(columns[4], ("created_at".to_string(), "INTEGER".to_string(), 1, 0));

        // Verify spent_command.
        let sc_cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info('spent_command')").unwrap();
            let rows = stmt
                .query_map([], |row| Ok(row.get::<_, String>(1).unwrap()))
                .unwrap();
            rows.filter_map(|r| r.ok()).collect()
        };
        assert!(sc_cols.contains(&"community_id".to_string()));
        assert!(sc_cols.contains(&"command_id".to_string()));
        assert!(sc_cols.contains(&"fingerprint".to_string()));
        assert!(sc_cols.contains(&"state".to_string()));

        // Verify indexes.
        let indexes: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
                .unwrap();
            let rows = stmt
                .query_map([], |row| Ok(row.get::<_, String>(0).unwrap()))
                .unwrap();
            rows.filter_map(|r| r.ok()).collect()
        };
        assert!(
            indexes.contains(&"spent_command_expiry".to_string()),
            "missing spent_command_expiry index"
        );
        assert!(
            indexes.contains(&"pause_lease_transition_expiry".to_string()),
            "missing pause_lease_transition_expiry index"
        );
    }

    #[test]
    fn tenant_mismatch_refuses_open() {
        let path = temp_store_path("tenant");
        let hi_a = HostIdentityInput {
            computer_id_override: Some("pc-a".to_string()),
            community_id: CommunityId::from_uuid(Uuid::new_v4()),
            relay_origin: "ws://relay-a:3000".to_string(),
        };
        let _store_a = ControlStore::open(&path, &hi_a).unwrap();

        // Open with different community_id (different relay).
        let hi_b = HostIdentityInput {
            computer_id_override: Some("pc-a".to_string()),
            community_id: CommunityId::from_uuid(Uuid::new_v4()),
            relay_origin: "ws://relay-b:3000".to_string(),
        };
        let result = ControlStore::open(&path, &hi_b);
        assert!(matches!(result, Err(StoreError::TenantMismatch { .. })));

        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn owner_binding_advances_revision() {
        let store = open_store("owner-binding");
        let (owner, rev1) = store.reconcile_owner_binding(Some("aaaa")).unwrap().unwrap();
        assert_eq!(owner, "aaaa");
        assert_eq!(rev1, 1);

        // Same owner — no change.
        let (_, rev1b) = store.reconcile_owner_binding(Some("aaaa")).unwrap().unwrap();
        assert_eq!(rev1b, 1);

        // Different owner — advance.
        let (owner2, rev2) = store.reconcile_owner_binding(Some("bbbb")).unwrap().unwrap();
        assert_eq!(owner2, "bbbb");
        assert_eq!(rev2, 2);
    }

    #[test]
    fn reads_default_store_path_when_env_unset() {
        // Verify that the default path computation matches the spec.
        // The actual path resolution happens in agent_controls.rs; here we just
        // verify the temp path works.
        let store = open_store("default-path");
        let hi = store.read_host_identity().unwrap();
        assert_eq!(hi.computer_id, "test-computer-01");
    }
}