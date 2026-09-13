#![deny(unsafe_code)]
#![warn(missing_docs)]
//! `buzz-workflow` — Workflow engine for Buzz.
//!
//! Channel-scoped automations with sequential execution, variable substitution,
//! conditional logic, and execution traces.
//!
//! ## Architecture
//!
//! - [`WorkflowEngine`] — top-level handle; lives in `AppState`
//! - [`schema`] — YAML/JSON definition types (`WorkflowDef`, `TriggerDef`, `ActionDef`, `Step`)
//! - [`executor`] — sequential execution, template resolution, condition evaluation
//! - [`error`] — [`WorkflowError`] enum
//!
//! ## Usage
//!
//! ```rust,ignore
//! let engine = Arc::new(WorkflowEngine::new(db, WorkflowConfig::default()));
//!
//! // Parse and validate a YAML definition.
//! let (def, json) = WorkflowEngine::parse_yaml(yaml_str)?;
//!
//! // React to an incoming event (called from event handler post-store hook).
//! // The community is the event's server-resolved tenant, threaded from the
//! // relay's bound `TenantContext` — the same workflow UUID can exist in two
//! // communities, so execution is always scoped to its owner.
//! engine.on_event(community_id, &stored_event).await?;
//!
//! // Run the background scheduler (cron triggers).
//! tokio::spawn(async move { engine.run().await });
//! ```

pub mod action_sink;
pub mod error;
pub mod executor;
pub mod schema;

pub use action_sink::{ActionSink, ActionSinkError};
pub use error::{PartialProgress, WorkflowError};
pub use executor::ExecutionResult;
pub use schema::{ActionDef, Step, TriggerDef, WorkflowDef};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use buzz_core::kind::{event_kind_u32, is_workflow_execution_kind, KIND_REACTION};
use buzz_core::tenant::CommunityId;
use buzz_db::workflow::RunStatus;
use buzz_db::Db;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Runtime configuration for the workflow engine.
#[derive(Clone, Debug)]
pub struct WorkflowConfig {
    /// Maximum number of concurrently executing workflow runs. Default: 100.
    pub max_concurrent: usize,
    /// Default per-step timeout in seconds. Default: 300 (5 minutes).
    pub default_timeout_secs: u64,
    /// Relay-wide kill switch for `invoke_agent` dispatch. Read once at engine
    /// construction from `BUZZ_WORKFLOW_INVOKE_AGENT`. Default: `false`.
    pub invoke_agent_enabled: bool,
    /// Seconds an open routine dispatch may stay unsettled before the sweeper
    /// fails it `routine_timeout`. Default: 1800 (30 minutes).
    pub routine_outcome_deadline_secs: u64,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 100,
            default_timeout_secs: 300,
            invoke_agent_enabled: false,
            routine_outcome_deadline_secs: 1800,
        }
    }
}

impl WorkflowConfig {
    /// Build config from environment variables, falling back to defaults.
    ///
    /// `BUZZ_WORKFLOW_INVOKE_AGENT=1` enables `invoke_agent` dispatch; any
    /// other value or unset leaves it disabled.
    /// `BUZZ_WORKFLOW_ROUTINE_OUTCOME_DEADLINE_SECS` overrides the sweeper
    /// deadline when set to a valid `u64`.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.invoke_agent_enabled =
            std::env::var("BUZZ_WORKFLOW_INVOKE_AGENT").as_deref() == Ok("1");
        if let Ok(secs) = std::env::var("BUZZ_WORKFLOW_ROUTINE_OUTCOME_DEADLINE_SECS") {
            if let Ok(parsed) = secs.parse::<u64>() {
                config.routine_outcome_deadline_secs = parsed;
            }
        }
        config
    }
}

/// The workflow engine. Clone is cheap (Arc-backed DB pool + semaphore).
pub struct WorkflowEngine {
    pub(crate) db: Db,
    pub(crate) config: WorkflowConfig,
    /// Semaphore enforcing `config.max_concurrent` simultaneous workflow runs.
    pub(crate) run_semaphore: Arc<Semaphore>,
    /// Last-fired timestamps for interval-triggered workflows, keyed by
    /// `(community_id, workflow_id)`. The same workflow UUID can exist in two
    /// communities (the PK is `(community_id, id)`); keying by bare id would let
    /// one community's interval fire suppress the other's for the interval.
    /// In-memory only — lost on restart. Missed fires during downtime are
    /// not replayed (acceptable for MVP).
    pub(crate) last_fired: DashMap<(CommunityId, Uuid), DateTime<Utc>>,
    /// Action sink for executing side-effects (SendMessage, etc.).
    /// Late-initialized via [`set_action_sink`] after `AppState` construction.
    pub(crate) action_sink: OnceLock<Arc<dyn ActionSink>>,
    /// Short-TTL cache for the per-event enabled-workflow lookup, keyed
    /// `(community_id, channel_id)`. Most channels have no workflows, so this
    /// removes one SELECT from nearly every ingested event.
    ///
    /// Consistency: the relay invalidates this cache on its own pod at the two
    /// workflow mutation sites (command upsert, NIP-09 deletion). There is
    /// deliberately no cross-pod invalidation — workflow triggering is not an
    /// access-control fence, so the worst case on another pod is a just-deleted
    /// workflow firing (or a just-created one missing events) for up to the TTL.
    /// The same TTL also bounds the same-pod look-aside race (a stale fill
    /// landing just after an invalidation). Workflow mutations are rare; the
    /// 10s window matches the relay's other moka caches (see `AppState` in
    /// `buzz-relay`).
    pub(crate) workflow_cache:
        moka::sync::Cache<(CommunityId, Uuid), Arc<Vec<buzz_db::workflow::WorkflowRecord>>>,
}

impl WorkflowEngine {
    /// Create a new `WorkflowEngine`.
    pub fn new(db: Db, config: WorkflowConfig) -> Self {
        let permits = config.max_concurrent.max(1);
        let run_semaphore = Arc::new(Semaphore::new(permits));
        Self {
            db,
            config,
            run_semaphore,
            last_fired: DashMap::new(),
            action_sink: OnceLock::new(),
            workflow_cache: moka::sync::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(std::time::Duration::from_secs(10))
                .build(),
        }
    }

    /// Drop the cached enabled-workflow list for a channel.
    ///
    /// Must be called after any write to a workflow's trigger eligibility or
    /// channel binding (currently the relay's command upsert and NIP-09
    /// deletion paths) so same-pod trigger matching sees the change
    /// immediately instead of after the cache TTL.
    pub fn invalidate_channel_workflows(&self, community_id: CommunityId, channel_id: Uuid) {
        self.workflow_cache.invalidate(&(community_id, channel_id));
    }

    /// Fail-closed pre-run authority gate (SEC-006).
    ///
    /// A workflow executes with its **owner's** standing authority long after
    /// the definition was saved, so every run-creation door must recheck the
    /// owner's *current* channel authority immediately before creating a run:
    ///
    /// - the owner must still be an active member of the workflow's channel;
    /// - if the definition contains an exfiltration-capable action
    ///   (`call_webhook`), the owner must currently hold the `owner` or
    ///   `admin` role.
    ///
    /// Any lookup error denies (fail-closed): a removed owner must never keep
    /// exfiltration authority because a membership read happened to fail.
    pub async fn check_owner_authority(
        &self,
        community_id: CommunityId,
        channel_id: Uuid,
        owner_pubkey: &[u8],
        def: &WorkflowDef,
    ) -> Result<(), WorkflowError> {
        let role = self
            .db
            .get_member_role(community_id, channel_id, owner_pubkey)
            .await
            .map_err(|e| {
                WorkflowError::Unauthorized(format!(
                    "owner authority lookup failed (fail-closed): {e}"
                ))
            })?;
        if owner_authority_allows(role.as_deref(), def.requires_elevated_authority()) {
            Ok(())
        } else {
            Err(WorkflowError::Unauthorized(
                "workflow owner lacks current channel authority".into(),
            ))
        }
    }

    /// Set the action sink. Called once after `AppState` construction.
    ///
    /// # Panics
    /// Panics if called more than once.
    pub fn set_action_sink(&self, sink: Arc<dyn ActionSink>) {
        if self.action_sink.set(sink).is_err() {
            panic!("action_sink already initialized");
        }
    }

    /// Read the engine's runtime configuration.
    pub fn config(&self) -> &WorkflowConfig {
        &self.config
    }

    /// Get the action sink reference.
    ///
    /// Returns `Err(WorkflowError)` if the sink has not been initialized via
    /// [`set_action_sink`]. This avoids a panic if the engine is used before
    /// wiring is complete.
    pub(crate) fn action_sink(&self) -> Result<&dyn ActionSink, WorkflowError> {
        self.action_sink.get().map(|s| s.as_ref()).ok_or_else(|| {
            WorkflowError::InvalidDefinition(
                "action_sink not initialized — call set_action_sink() before executing workflows"
                    .into(),
            )
        })
    }

    /// Parse and validate a YAML workflow definition.
    ///
    /// Returns `(WorkflowDef, canonical_json)` on success. The canonical JSON
    /// is suitable for storage in the `definition` column.
    pub fn parse_yaml(yaml: &str) -> Result<(WorkflowDef, String), WorkflowError> {
        schema::parse_yaml(yaml)
    }

    /// Finalize a workflow run after execution completes or fails.
    ///
    /// This is the **single** place that maps an executor result to a DB status
    /// update. All execution paths (event-triggered, manual trigger/webhook,
    /// approval resume) call this instead of duplicating the 3-way match.
    ///
    /// `existing_trace` is prepended to the executor's trace — used by the
    /// approval-resume path where pre-approval steps already have trace entries.
    pub async fn finalize_run(
        &self,
        community_id: CommunityId,
        run_id: uuid::Uuid,
        result: Result<ExecutionResult, (WorkflowError, PartialProgress)>,
        existing_trace: Option<Vec<serde_json::Value>>,
    ) {
        let prefix = existing_trace.unwrap_or_default();

        match result {
            Ok(result) => {
                let mut full_trace = prefix;
                full_trace.extend(result.trace);
                let step_count = result.step_index as i32;

                let dispatched_routine = full_trace.iter().any(|entry| {
                    entry
                        .get("output")
                        .and_then(|o| o.get("dispatched"))
                        .and_then(|d| d.as_bool())
                        == Some(true)
                });

                let trace_json = serde_json::Value::Array(full_trace);

                if dispatched_routine {
                    tracing::info!(run_id = %run_id, "routine fired — run left Running pending settlement");
                    if let Err(e) = self
                        .db
                        .update_workflow_run(
                            community_id,
                            run_id,
                            RunStatus::Running,
                            step_count,
                            &trace_json,
                            None,
                        )
                        .await
                    {
                        tracing::error!(
                            run_id = %run_id,
                            "Failed to update run to Running (routine dispatch): {e}"
                        );
                    }
                } else if result.approval_token.is_some() {
                    // Approval gates are not yet implemented (WF-08).
                    // Fail explicitly rather than creating unreachable WaitingApproval rows.
                    tracing::warn!(
                        run_id = %run_id,
                        step_index = result.step_index,
                        "Workflow hit approval gate — not yet implemented, marking as failed"
                    );
                    if let Err(e) = self
                        .db
                        .update_workflow_run(
                            community_id,
                            run_id,
                            RunStatus::Failed,
                            step_count,
                            &trace_json,
                            Some(buzz_db::workflow::WorkflowRunFailure {
                                code: "approval_not_supported",
                                message: "approval gates not yet implemented — see WF-08",
                            }),
                        )
                        .await
                    {
                        tracing::error!(
                            run_id = %run_id,
                            "Failed to update run to Failed (approval gate): {e}"
                        );
                    }
                } else {
                    tracing::info!(run_id = %run_id, "Workflow run completed");
                    if let Err(e) = self
                        .db
                        .update_workflow_run(
                            community_id,
                            run_id,
                            RunStatus::Completed,
                            step_count,
                            &trace_json,
                            None,
                        )
                        .await
                    {
                        tracing::error!(
                            run_id = %run_id,
                            "Failed to update run to Completed: {e}"
                        );
                    }
                }
            }
            Err((e, progress)) => {
                tracing::error!(run_id = %run_id, "Workflow run failed: {e}");
                let mut full_trace = prefix;
                full_trace.extend(progress.trace);
                let trace_json = serde_json::Value::Array(full_trace);
                if let Err(db_err) = self
                    .db
                    .update_workflow_run(
                        community_id,
                        run_id,
                        RunStatus::Failed,
                        progress.step_index as i32,
                        &trace_json,
                        Some(buzz_db::workflow::WorkflowRunFailure {
                            code: e.code(),
                            message: &e.to_string(),
                        }),
                    )
                    .await
                {
                    tracing::error!(
                        run_id = %run_id,
                        "Failed to update run to Failed: {db_err}"
                    );
                }
            }
        }
    }

    /// Settle an open routine dispatch from an agent-signed outcome event.
    ///
    /// Implements the six settlement rules (2.5.1-2.5.6): unknown/settled run
    /// and signer-mismatch events are rejected and logged without touching
    /// `consecutive_failures`; a recognised outcome updates the run, strikes,
    /// and (on strike-10 or a first-of-day budget breach) auto-pauses the
    /// workflow and posts exactly one notice.
    async fn settle_routine_outcome(
        self: &Arc<Self>,
        community_id: CommunityId,
        run_id: Uuid,
        event: &buzz_core::StoredEvent,
    ) {
        let dispatch = match self.db.get_open_routine_dispatch(community_id, run_id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::info!(run_id = %run_id, reason = "unknown_run", "routine_outcome_rejected");
                return;
            }
            Err(e) => {
                tracing::error!(run_id = %run_id, "routine settlement lookup failed: {e}");
                return;
            }
        };

        if event.event.pubkey.to_bytes().as_slice() != dispatch.agent_pubkey.as_slice() {
            tracing::info!(run_id = %run_id, reason = "signer_mismatch", "routine_outcome_rejected");
            return;
        }

        let Some(outcome) = tag_value(&event.event, "buzz:routine-outcome").filter(|v| {
            matches!(
                *v,
                "succeeded" | "failed" | "budget_exceeded_per_run" | "budget_exceeded_daily"
            )
        }) else {
            tracing::info!(run_id = %run_id, reason = "bad_outcome", "routine_outcome_rejected");
            return;
        };

        let run_status = if outcome == "succeeded" {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        };
        let error_code = (outcome != "succeeded").then(|| format!("routine_{outcome}"));

        let settlement = match self
            .db
            .settle_routine_dispatch(
                community_id,
                run_id,
                outcome,
                run_status,
                error_code.as_deref(),
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(run_id = %run_id, "routine settlement transaction failed: {e}");
                return;
            }
        };

        let latency_ms = settlement
            .dispatched_at
            .map(|d| (chrono::Utc::now() - d).num_milliseconds());
        tracing::info!(
            workflow_id = %settlement.workflow_id,
            run_id = %run_id,
            outcome,
            strikes = settlement.consecutive_failures,
            latency_ms,
            "routine settled"
        );

        self.post_routine_auto_pause_notice(community_id, &settlement)
            .await;
    }

    /// Post the (at most one) auto-pause notice for a settlement, if due, and
    /// audit `routine_auto_paused`. Shared by the outcome-event settlement
    /// path and the sweeper's timeout settlement path.
    async fn post_routine_auto_pause_notice(
        &self,
        community_id: CommunityId,
        settlement: &buzz_db::routine::RoutineSettlement,
    ) {
        if !settlement.notice_due {
            return;
        }
        let notice_text = match settlement.paused_reason.as_deref() {
            Some("strikes") => format!(
                "Routine \"{}\" was auto-paused after 10 consecutive failures. Re-enable it from the Workflows screen.",
                settlement.workflow_name
            ),
            _ => format!(
                "Routine \"{}\" was auto-paused: daily token budget reached. It stays paused until you re-enable it.",
                settlement.workflow_name
            ),
        };
        let owner_pubkey_hex = hex::encode(&settlement.owner_pubkey);
        if let Ok(sink) = self.action_sink() {
            if let Err(e) = sink
                .send_message(
                    community_id,
                    &settlement.result_channel.to_string(),
                    &notice_text,
                    &notice_text,
                    &owner_pubkey_hex,
                    None,
                )
                .await
            {
                tracing::error!(workflow_id = %settlement.workflow_id, "failed to post routine auto-pause notice: {e}");
            }
        }
        tracing::info!(
            workflow_id = %settlement.workflow_id,
            paused_reason = settlement.paused_reason.as_deref().unwrap_or(""),
            "routine_auto_paused"
        );
    }

    /// Called from the event handler post-store hook for every stored event.
    ///
    /// Checks whether any workflow in the event's channel has a matching trigger.
    /// Workflow execution events (kinds 46001–46012) are excluded to prevent loops.
    ///
    /// `community_id` is the server-resolved community the event was stored
    /// under — `StoredEvent` does not carry it, and the same channel UUID can
    /// exist in two communities, so the workflow lookup/run-creation must be
    /// scoped to the caller's tenant or community B could trigger community A's
    /// workflow on a colliding channel id.
    ///
    /// The method takes `self: &Arc<Self>` so that the spawned task can hold a
    /// clone of the `Arc` without requiring `'static` on `&self`.
    pub async fn on_event(
        self: &Arc<Self>,
        community_id: CommunityId,
        event: &buzz_core::StoredEvent,
    ) -> Result<(), WorkflowError> {
        let Some(channel_id) = event.channel_id else {
            tracing::debug!(
                event_id = %event.event.id.to_hex(),
                kind = event_kind_u32(&event.event),
                "Skipping workflow trigger — event has no channel_id"
            );
            return Ok(());
        };

        let kind_u32 = event_kind_u32(&event.event);

        // Settlement (G1R F-3 / I-16): this MUST be the first statement after
        // the channel_id guard, strictly before is_workflow_execution_kind,
        // the workflow-cache lookup, and the workflows.is_empty() early
        // return below. An outcome event's result_channel commonly has no
        // enabled workflow of its own — the cache lookup would return early
        // and the dispatch would hang Running until the sweeper times it out.
        if kind_u32 == buzz_core::kind::KIND_STREAM_MESSAGE {
            if let Some(run_id) = single_routine_run_tag(&event.event) {
                self.settle_routine_outcome(community_id, run_id, event).await;
                return Ok(());
            }
        }

        // Exclude workflow execution events to prevent infinite loops.
        if is_workflow_execution_kind(kind_u32) {
            return Ok(());
        }

        let cache_key = (community_id, channel_id);
        let workflows = match self.workflow_cache.get(&cache_key) {
            Some(cached) => cached,
            None => {
                let fresh = Arc::new(
                    self.db
                        .list_enabled_channel_workflows(community_id, channel_id)
                        .await
                        .map_err(WorkflowError::from)?,
                );
                self.workflow_cache.insert(cache_key, Arc::clone(&fresh));
                fresh
            }
        };

        if workflows.is_empty() {
            return Ok(());
        }

        let trigger_ctx = build_trigger_context(event);

        let trigger_ctx_json: serde_json::Value = match serde_json::to_value(&trigger_ctx) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize trigger context: {e}");
                return Ok(());
            }
        };

        for workflow in workflows.iter() {
            let def: WorkflowDef = match serde_json::from_value(workflow.definition.clone()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(workflow_id = %workflow.id, "Failed to parse definition: {e}");
                    continue;
                }
            };

            if !def.enabled || !trigger_matches_event(&def.trigger, kind_u32) {
                continue;
            }

            if !should_fire_workflow(&def, &trigger_ctx, workflow.id).await {
                continue;
            }

            // SEC-006: recheck the owner's *current* channel authority
            // immediately before run creation. The cached workflow list can be
            // up to 10s stale, and disable-on-removal can race a concurrent
            // event — this per-fire gate is the authoritative, fail-closed
            // check that a removed (or under-privileged, for exfiltration
            // definitions) owner cannot cause a run.
            if let Err(e) = self
                .check_owner_authority(community_id, channel_id, &workflow.owner_pubkey, &def)
                .await
            {
                tracing::warn!(
                    workflow_id = %workflow.id,
                    "Skipping workflow — owner authority check failed: {e}"
                );
                continue;
            }

            let trigger_event_id_bytes = event.event.id.as_bytes().to_vec();
            let run_id = match self
                .db
                .create_workflow_run(
                    community_id,
                    workflow.id,
                    Some(&trigger_event_id_bytes),
                    Some(&trigger_ctx_json),
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(workflow_id = %workflow.id, "Failed to create run: {e}");
                    continue;
                }
            };

            tracing::debug!(
                workflow_id = %workflow.id,
                run_id = %run_id,
                "Workflow triggered — spawning execution"
            );

            let engine = Arc::clone(self);
            let def_clone = def.clone();
            let ctx_clone = trigger_ctx.clone();

            tokio::spawn(async move {
                let result =
                    executor::execute_run(&engine, community_id, run_id, &def_clone, &ctx_clone)
                        .await;
                engine
                    .finalize_run(community_id, run_id, result, None)
                    .await;
            });
        }

        Ok(())
    }

    /// Interval prefilter: decide whether the interval workflow should fire this
    /// tick, applying the cold-start anchor seed as a side effect.
    ///
    /// `last` is the resolved anchor (in-memory entry if present, else the
    /// durable `latest_scheduled_workflow_fire` read). Returns `true` to proceed
    /// to the durable claim, `false` to suppress this tick.
    ///
    /// Cold-start liveness: a brand-new interval workflow has no in-memory entry
    /// AND no prior claim, so `last` is `None`. `interval_should_fire` then reads
    /// `last = now` and suppresses — correct for the first tick (wait a full
    /// interval), but the in-memory anchor is only written *after* a successful
    /// claim, and no claim is attempted until the prefilter passes. Without
    /// seeding, every subsequent tick repeats with `last = None` and the workflow
    /// suppresses forever. So on the `None` suppress path we seed `now`: the next
    /// tick counts from a real anchor and the workflow fires after one interval.
    /// We seed ONLY when `last` was `None`; when `last` is `Some` we are correctly
    /// mid-interval and must not advance the anchor, or it would never elapse.
    fn interval_prefilter_should_fire(
        &self,
        community_id: CommunityId,
        workflow_id: Uuid,
        dur: &str,
        last: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        interval_prefilter_should_fire(&self.last_fired, community_id, workflow_id, dur, last, now)
    }

    /// Background loop for scheduled (cron/interval) triggers.
    ///
    /// Ticks every 60 seconds. For each active workflow with a `Schedule`
    /// trigger, checks whether the cron expression or interval has elapsed
    /// and spawns execution if so.
    ///
    /// Uses window-based matching for cron expressions to handle tick drift:
    /// `schedule.after(&(now - 60s)).next() <= now` instead of `includes(now)`.
    ///
    /// Interval tracking is anchored on the durable scheduled-fire claim:
    /// `last_fired` is an in-memory pre-filter, but the
    /// `(community_id, workflow_id, scheduled_for)` claim row is the
    /// at-most-once boundary across pods and restarts. On the first tick after
    /// a restart the interval anchor is seeded from
    /// `latest_scheduled_workflow_fire` so a process bounce cannot double-fire
    /// within an interval.
    pub async fn run(self: &Arc<Self>) {
        tracing::info!("WorkflowEngine cron loop started (60s tick)");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;

            let now = Utc::now();

            // Sweeper (2.6, I-17): the sole bound on `Running` routine runs.
            // Every dispatch older than the deadline is failed `timeout`,
            // which the settlement rule's `routine_<outcome>` mapping turns
            // into `error_code = "routine_timeout"` with no special-casing.
            let deadline = now
                - chrono::Duration::seconds(self.config.routine_outcome_deadline_secs as i64);
            match self.db.expire_routine_dispatches(deadline).await {
                Ok(expired) => {
                    for (expired_community, expired_run_id) in expired {
                        match self
                            .db
                            .settle_routine_dispatch(
                                expired_community,
                                expired_run_id,
                                "timeout",
                                RunStatus::Failed,
                                Some("routine_timeout"),
                            )
                            .await
                        {
                            Ok(settlement) => {
                                tracing::info!(
                                    workflow_id = %settlement.workflow_id,
                                    run_id = %expired_run_id,
                                    outcome = "timeout",
                                    strikes = settlement.consecutive_failures,
                                    "routine settled outcome=timeout"
                                );
                                self.post_routine_auto_pause_notice(expired_community, &settlement)
                                    .await;
                            }
                            Err(e) => {
                                tracing::error!(
                                    run_id = %expired_run_id,
                                    "sweeper: failed to settle expired routine dispatch: {e}"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("sweeper: failed to list expired routine dispatches: {e}");
                }
            }

            let workflows = match self.db.list_all_enabled_workflows().await {
                Ok(wf) => wf,
                Err(e) => {
                    tracing::error!("Cron tick: failed to load workflows: {e}");
                    continue;
                }
            };

            for workflow in &workflows {
                // The same workflow UUID may exist in another community; carry
                // the row's owning community through fire-tracking, run creation,
                // and execution so a fire/run never crosses tenants.
                let community_id = workflow.community_id;
                let def: schema::WorkflowDef =
                    match serde_json::from_value(workflow.definition.clone()) {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(
                                workflow_id = %workflow.id,
                                "Cron tick: failed to parse workflow definition: {e}"
                            );
                            continue;
                        }
                    };

                if !def.enabled {
                    continue;
                }

                // Fix 2: skip workflows with no channel_id — an empty channel_id
                // causes silent downstream failures when the run tries to act on a channel.
                let Some(channel_id) = workflow.channel_id else {
                    tracing::warn!(
                        workflow_id = %workflow.id,
                        "Cron tick: skipping schedule workflow with no channel_id"
                    );
                    continue;
                };

                // Resolve the *deterministic* schedule instant this tick is
                // firing for. `scheduled_for` is computed identically on every
                // pod (cron's own scheduled time, or the interval bucket
                // boundary) so all pods collide on a single durable claim —
                // never `now`, which is per-pod and would let every pod fire.
                let (scheduled_for, trigger_type) = match &def.trigger {
                    schema::TriggerDef::Schedule {
                        cron: Some(expr),
                        interval: None,
                    } => match cron_fire_instant(expr, now, 60, workflow.id) {
                        Some(instant) => (instant, "cron"),
                        None => continue,
                    },
                    schema::TriggerDef::Schedule {
                        cron: None,
                        interval: Some(dur),
                    } => {
                        // Cheap pre-filter: skip the claim attempt when the
                        // in-memory clock says we're clearly mid-interval. The
                        // durable claim below is the real at-most-once boundary;
                        // this only avoids a DB write every tick. Seed the
                        // anchor from the DB on the first tick after restart so
                        // a process bounce can't double-fire within an interval.
                        let last = match self.last_fired.get(&(community_id, workflow.id)) {
                            Some(t) => Some(*t),
                            None => match self
                                .db
                                .latest_scheduled_workflow_fire(community_id, workflow.id)
                                .await
                            {
                                Ok(anchor) => anchor,
                                Err(e) => {
                                    // Fail closed: a missing anchor reads as
                                    // last_fired = now in interval_should_fire,
                                    // so this tick is suppressed and the next
                                    // tick retries. Surface the read failure so
                                    // a persistently-unreadable anchor is visible
                                    // rather than silently stalling the schedule.
                                    tracing::warn!(
                                        community_id = %community_id,
                                        workflow_id = %workflow.id,
                                        "Cron tick: failed to read interval restart anchor, \
                                         suppressing this tick: {e}"
                                    );
                                    None
                                }
                            },
                        };
                        if !self.interval_prefilter_should_fire(
                            community_id,
                            workflow.id,
                            dur,
                            last,
                            now,
                        ) {
                            continue;
                        }
                        match interval_fire_instant(dur, now, workflow.id) {
                            Some(instant) => (instant, "interval"),
                            None => continue,
                        }
                    }
                    _ => continue, // Non-schedule triggers handled by on_event()
                };

                // SEC-006: recheck the owner's current channel authority
                // BEFORE the durable claim. Placing the gate after the claim
                // would let a revoked owner's workflow consume the
                // at-most-once fire slot (claims are never re-fired), turning
                // revocation into a denial-of-fire for a later re-enable.
                if let Err(e) = self
                    .check_owner_authority(community_id, channel_id, &workflow.owner_pubkey, &def)
                    .await
                {
                    tracing::warn!(
                        workflow_id = %workflow.id,
                        "Cron tick: skipping workflow — owner authority check failed: {e}"
                    );
                    continue;
                }

                // Busy-skip (2.6, N6): a routine with an open (unsettled)
                // dispatch must not fire again. Still consume the claim so
                // this instant is not re-attempted every tick; this is not a
                // strike — consecutive_failures is untouched.
                if def.invokes_agent() {
                    match self.db.has_open_routine_dispatch(community_id, workflow.id).await {
                        Ok(true) => {
                            match self
                                .db
                                .claim_scheduled_workflow_fire(community_id, workflow.id, scheduled_for)
                                .await
                            {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::error!(
                                        workflow_id = %workflow.id,
                                        "Cron tick: busy-skip claim failed: {e}"
                                    );
                                }
                            }
                            tracing::info!(workflow_id = %workflow.id, "routine_skipped_busy");
                            if trigger_type == "interval" {
                                self.last_fired.insert((community_id, workflow.id), now);
                            }
                            continue;
                        }
                        Ok(false) => {}
                        Err(e) => {
                            tracing::error!(
                                workflow_id = %workflow.id,
                                "Cron tick: has_open_routine_dispatch check failed: {e}"
                            );
                            continue;
                        }
                    }
                }

                // Durable at-most-once claim — the cross-pod fire boundary.
                // The loser receives `None` and skips BEFORE any run creation or
                // side effect. `community_id` is the workflow row's own
                // community (server provenance from the scan), never client
                // input; the claim binds `(community_id, workflow_id,
                // scheduled_for)` so a duplicate workflow UUID in another
                // community claims independently.
                match self
                    .db
                    .claim_scheduled_workflow_fire(community_id, workflow.id, scheduled_for)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        // Another pod (or an earlier tick this pod) already
                        // claimed this instant. Still advance the in-memory
                        // interval clock so we don't re-attempt the claim every
                        // tick for the rest of the interval.
                        if trigger_type == "interval" {
                            self.last_fired.insert((community_id, workflow.id), now);
                        }
                        continue;
                    }
                    Err(e) => {
                        tracing::error!(
                            workflow_id = %workflow.id,
                            "Cron tick: scheduled-fire claim failed: {e}"
                        );
                        continue;
                    }
                }

                // Fix 5: handle serialization errors explicitly rather than silently
                // dropping the trigger context with .ok().
                let trigger_ctx = executor::TriggerContext {
                    channel_id: channel_id.to_string(),
                    timestamp: now.timestamp().to_string(),
                    ..Default::default()
                };
                let trigger_ctx_json = match serde_json::to_value(&trigger_ctx) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!(
                            workflow_id = %workflow.id,
                            "Cron tick: failed to serialize trigger context: {e}"
                        );
                        continue;
                    }
                };

                let run_id = match self
                    .db
                    .create_workflow_run(
                        community_id,
                        workflow.id,
                        None, // no trigger event for cron
                        trigger_ctx_json.as_ref(),
                    )
                    .await
                {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::error!(
                            workflow_id = %workflow.id,
                            "Cron tick: failed to create workflow run: {e}"
                        );
                        // The claim is held but the run failed to create. The
                        // claim row intentionally stays (its `workflow_run_id`
                        // NULL) so this instant is not re-fired: at-most-once is
                        // preserved over exactly-once on transient run-insert
                        // failures.
                        continue;
                    }
                };

                // Link the won claim to its run for ops/audit forensics. The
                // claim row already guarantees dedupe; this is best-effort.
                if let Err(e) = self
                    .db
                    .attach_scheduled_workflow_run(community_id, workflow.id, scheduled_for, run_id)
                    .await
                {
                    tracing::warn!(
                        workflow_id = %workflow.id,
                        run_id = %run_id,
                        "Cron tick: failed to attach run to scheduled-fire claim: {e}"
                    );
                }

                // Update last_fired AFTER a successful claim+insert so that a
                // failure doesn't suppress the next tick for the full interval.
                // Only needed for interval triggers — cron uses window-based
                // matching which already prevents double-fire within the same
                // minute, and the durable claim backstops both.
                if trigger_type == "interval" {
                    self.last_fired.insert((community_id, workflow.id), now);
                }

                // Fix 6: log the specific trigger type (cron vs interval).
                tracing::info!(
                    workflow_id = %workflow.id,
                    run_id = %run_id,
                    trigger = trigger_type,
                    "Cron trigger fired"
                );

                let engine = Arc::clone(self);
                let def_clone = def.clone();
                let ctx_clone = trigger_ctx.clone();
                tokio::spawn(async move {
                    let result = executor::execute_run(
                        &engine,
                        community_id,
                        run_id,
                        &def_clone,
                        &ctx_clone,
                    )
                    .await;
                    engine
                        .finalize_run(community_id, run_id, result, None)
                        .await;
                });
            }

            // Fix 1: prune stale last_fired entries for workflows that are no longer
            // active/enabled. Without this the DashMap grows monotonically as
            // workflows are deleted or disabled. Keyed by `(community_id, id)` so
            // entries are matched to the same scope they were inserted under.
            let active_ids: std::collections::HashSet<(CommunityId, Uuid)> =
                workflows.iter().map(|w| (w.community_id, w.id)).collect();
            self.last_fired.retain(|key, _| active_ids.contains(key));
        }
    }
}

/// First value of the named tag on an event, if present.
fn tag_value<'a>(event: &'a nostr::Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        if s.first().map(|f| f.as_str()) == Some(name) {
            s.get(1).map(|v| v.as_str())
        } else {
            None
        }
    })
}

/// The event's `buzz:routine-run` tag value, parsed as a UUID, when the event
/// carries exactly one such tag.
fn single_routine_run_tag(event: &nostr::Event) -> Option<Uuid> {
    let mut matches = event.tags.iter().filter(|t| {
        t.as_slice().first().map(|f| f.as_str()) == Some("buzz:routine-run")
    });
    let only = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    only.as_slice().get(1)?.parse::<Uuid>().ok()
}

/// Find the cron schedule instant that fired within the `window_secs`-wide
/// window ending at `now`, if any.
///
/// Uses window-based matching: finds the next scheduled time after
/// `(now - window_secs)` and returns it when it falls at or before `now`.
/// This tolerates tick drift gracefully — a 61s tick won't miss a
/// minute-granularity cron expression. The returned instant is the cron's own
/// scheduled time (not `now`), so every pod evaluating the same expression in
/// the same window computes the *same* value — making it a safe, deterministic
/// claim anchor for cross-pod at-most-once firing.
///
/// Returns `None` (and logs a warning) if the expression is invalid or nothing
/// is due in the window.
fn cron_fire_instant(
    expr: &str,
    now: DateTime<Utc>,
    window_secs: i64,
    workflow_id: Uuid,
) -> Option<DateTime<Utc>> {
    let normalized = schema::normalize_cron(expr);
    match normalized.parse::<cron::Schedule>() {
        Ok(sched) => {
            let window_start = now - chrono::Duration::seconds(window_secs);
            sched.after(&window_start).next().filter(|t| *t <= now)
        }
        Err(e) => {
            tracing::warn!(
                workflow_id = %workflow_id,
                "Cron tick: invalid cron expression '{expr}': {e}"
            );
            None
        }
    }
}

/// Quantize `now` to the interval bucket boundary, yielding a deterministic
/// claim anchor that every pod computes identically within the same bucket.
///
/// The boundary is `floor(now / interval) * interval` from the Unix epoch.
/// Because the scheduler ticks every 60s and interval schedules are minutes or
/// longer, bounded cross-pod clock skew keeps all pods inside the same bucket,
/// so they collide on one `(community, workflow, scheduled_for)` claim — only
/// one wins and creates the run. Returns `None` if the duration is unparseable
/// or non-positive (the caller skips firing).
fn interval_fire_instant(
    dur: &str,
    now: DateTime<Utc>,
    workflow_id: Uuid,
) -> Option<DateTime<Utc>> {
    match executor::parse_duration_secs(dur) {
        Ok(interval_secs) if interval_secs > 0 => {
            let secs = interval_secs as i64;
            let bucket = (now.timestamp().div_euclid(secs)) * secs;
            DateTime::from_timestamp(bucket, 0)
        }
        Ok(_) => {
            tracing::warn!(
                workflow_id = %workflow_id,
                "Cron tick: interval duration is zero — skipping"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                workflow_id = %workflow_id,
                "Cron tick: invalid interval '{dur}': {e}"
            );
            None
        }
    }
}

/// Check whether an interval trigger should fire based on the last-fired time.
///
/// `last_fired` is `None` on the first tick after startup — in that case we
/// default to `now`, which prevents an immediate fire and waits a full interval.
///
/// Returns `false` (and logs a warning) if the duration string is invalid.
fn interval_should_fire(
    dur: &str,
    last_fired: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    workflow_id: Uuid,
) -> bool {
    match executor::parse_duration_secs(dur) {
        Ok(interval_secs) => {
            // Default to now on first tick — prevents immediate fire after startup.
            let last = last_fired.unwrap_or(now);
            let elapsed = (now - last).num_seconds().unsigned_abs();
            elapsed >= interval_secs
        }
        Err(e) => {
            tracing::warn!(
                workflow_id = %workflow_id,
                "Cron tick: invalid interval '{dur}': {e}"
            );
            false
        }
    }
}

/// Interval prefilter decision + cold-start anchor seed. See the
/// [`WorkflowEngine::interval_prefilter_should_fire`] wrapper for the liveness
/// rationale. Free function over the `last_fired` map so it is unit-testable
/// without a `Db`/Postgres: the only state it touches is the in-memory anchor.
///
/// Returns `true` to fire, `false` to suppress. On the cold-start `None` suppress
/// path it seeds `now` so the next tick has a real anchor; it never advances an
/// existing (`Some`) anchor, which is mid-interval and must elapse on its own.
fn interval_prefilter_should_fire(
    last_fired: &DashMap<(CommunityId, Uuid), DateTime<Utc>>,
    community_id: CommunityId,
    workflow_id: Uuid,
    dur: &str,
    last: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if interval_should_fire(dur, last, now, workflow_id) {
        return true;
    }
    if last.is_none() {
        last_fired.insert((community_id, workflow_id), now);
    }
    false
}

/// Check emoji and filter-expression conditions that determine whether a
/// matched workflow should actually fire. Extracted from `on_event` to keep
/// the per-workflow loop body small.
///
/// Returns `true` if the workflow should fire, `false` to skip.
async fn should_fire_workflow(
    def: &WorkflowDef,
    trigger_ctx: &executor::TriggerContext,
    workflow_id: uuid::Uuid,
) -> bool {
    if let TriggerDef::ReactionAdded {
        emoji: Some(ref expected),
        ..
    } = def.trigger
    {
        if &trigger_ctx.emoji != expected {
            tracing::debug!(
                workflow_id = %workflow_id,
                expected_emoji = %expected,
                actual_emoji = %trigger_ctx.emoji,
                "Reaction emoji mismatch — skipping workflow"
            );
            return false;
        }
    }

    let filter = match &def.trigger {
        TriggerDef::MessagePosted { filter }
        | TriggerDef::ReactionAdded { filter, .. }
        | TriggerDef::DiffPosted { filter } => filter.as_ref(),
        TriggerDef::Schedule { .. } | TriggerDef::Webhook => None,
    };
    if let Some(expr) = filter {
        match executor::evaluate_condition(expr, trigger_ctx, &HashMap::new()).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(
                    workflow_id = %workflow_id,
                    "Trigger filter evaluated false — skipping workflow"
                );
                return false;
            }
            Err(e) => {
                tracing::warn!(
                    workflow_id = %workflow_id,
                    "Trigger filter error: {e} — skipping workflow"
                );
                return false;
            }
        }
    }

    true
}

/// Build a [`executor::TriggerContext`] from a [`buzz_core::StoredEvent`].
///
/// - `text` — event content (message body or reaction emoji character)
/// - `author` — pubkey hex string
/// - `channel_id` — channel UUID as string (empty if no channel scope)
/// - `timestamp` — Unix timestamp as string
/// - `emoji` — for `KIND_REACTION` events, the content is the emoji; otherwise empty
/// - `message_id` — for reactions, the target message's event ID (from `e` tag);
///   for all other events, the event's own ID
pub fn build_trigger_context(event: &buzz_core::StoredEvent) -> executor::TriggerContext {
    let kind_u32 = event_kind_u32(&event.event);
    let content = event.event.content.clone();

    // Workflow conditions make authorization decisions from `trigger_author`,
    // so it must come from the event signature. An `actor` tag is ordinary
    // signer-controlled metadata and cannot speak for another pubkey.
    let author = event.event.pubkey.to_hex();

    // For reaction events (NIP-25), the content field holds the emoji character
    // or shortcode (e.g. "👍", "+", "-"). Expose it as `emoji`.
    let emoji = if kind_u32 == KIND_REACTION {
        content.clone()
    } else {
        String::new()
    };

    // For reactions (NIP-25), `message_id` should be the target message, not
    // the reaction event itself. NIP-25 stores the target in an `e` tag whose
    // value is a 64-char hex event ID (not a UUID channel reference).
    // Per NIP-25, the last `e` tag is the direct target (earlier ones may be thread roots).
    let message_id = if kind_u32 == KIND_REACTION {
        event
            .event
            .tags
            .iter()
            .rev()
            .find_map(|tag| {
                let key = tag.kind().to_string();
                if key == "e" {
                    tag.content().and_then(|v| {
                        // Distinguish hex event IDs (64 chars) from UUID channel refs.
                        if v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()) {
                            Some(v.to_string())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            })
            // Fallback to the reaction event's own ID if no valid `e` tag found.
            .unwrap_or_else(|| event.event.id.to_hex())
    } else {
        event.event.id.to_hex()
    };

    executor::TriggerContext {
        text: content,
        author,
        channel_id: event
            .channel_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        timestamp: event.event.created_at.as_secs().to_string(),
        emoji,
        message_id,
        is_reply: event_is_reply(&event.event),
        webhook_fields: HashMap::new(),
    }
}

/// True when an event is a threaded reply — it carries a valid NIP-10 `reply`
/// marker. Delegates to the shared [`buzz_core::nip10`] parser so this stays in
/// lockstep with ingest's `resolve_nip10_thread_meta`: a `root` marker alone is
/// top-level, and a marker with a malformed (non-64-hex) event id is ignored by
/// ingest, so it must not flip `trigger_is_reply` either — else a
/// `trigger_is_reply == false` workflow would skip a message ingest stored as a
/// new top-level post.
fn event_is_reply(event: &nostr::Event) -> bool {
    buzz_core::nip10::parse_thread_markers(&event.tags)
        .reply
        .is_some()
}

/// Pure authority decision for [`WorkflowEngine::check_owner_authority`].
///
/// `role` is the owner's *current* active role in the workflow's channel
/// (`None` = not an active member — removed, left, or never joined).
/// `needs_elevated` is true when the definition contains an
/// exfiltration-capable action (see `WorkflowDef::requires_elevated_authority`).
///
/// Rules:
/// - not a member ⇒ deny, always;
/// - member ⇒ allowed for ordinary definitions;
/// - elevated definitions ⇒ only `owner` / `admin` roles.
fn owner_authority_allows(role: Option<&str>, needs_elevated: bool) -> bool {
    match role {
        None => false,
        Some(r) if needs_elevated => matches!(r, "owner" | "admin"),
        Some(_) => true,
    }
}

/// Returns `true` if the trigger type matches the given event kind.
fn trigger_matches_event(trigger: &TriggerDef, kind_u32: u32) -> bool {
    use buzz_core::kind::{KIND_REACTION, KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_DIFF};
    match trigger {
        TriggerDef::MessagePosted { .. } => kind_u32 == KIND_STREAM_MESSAGE,
        TriggerDef::ReactionAdded { .. } => kind_u32 == KIND_REACTION,
        TriggerDef::DiffPosted { .. } => kind_u32 == KIND_STREAM_MESSAGE_DIFF,
        // Schedule and Webhook triggers are not fired by channel events.
        TriggerDef::Schedule { .. } | TriggerDef::Webhook => false,
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;

    #[test]
    fn cron_fire_instant_matches_within_window() {
        // "every minute" cron — should always fire within a 60s window.
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T12:00:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let wf_id = Uuid::new_v4();
        // The matched instant is the minute boundary 12:00:00, NOT `now`.
        assert_eq!(
            cron_fire_instant("* * * * *", now, 60, wf_id),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-06-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            ),
            "every-minute cron should return the minute boundary as the anchor"
        );
    }

    #[test]
    fn cron_fire_instant_returns_none_for_invalid_expr() {
        let now = Utc::now();
        let wf_id = Uuid::new_v4();
        assert!(
            cron_fire_instant("not-a-cron", now, 60, wf_id).is_none(),
            "invalid cron should return None"
        );
    }

    #[test]
    fn cron_fire_instant_returns_none_outside_window() {
        // Fixed time: 2026-06-15 14:30:00 UTC (a Sunday in June)
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T14:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let wf_id = Uuid::new_v4();
        // "0 0 1 1 *" = midnight on Jan 1 only — June 15 is definitely outside.
        assert!(
            cron_fire_instant("0 0 1 1 *", now, 60, wf_id).is_none(),
            "Jan-1-only cron should not fire on June 15"
        );
    }

    #[test]
    fn cron_fire_instant_at_exact_minute_boundary() {
        // Fixed time: exactly 09:00:00 UTC. Cron "0 9 * * *" fires at 09:00.
        // Window [08:59:00, 09:00:00] should contain the fire time.
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let wf_id = Uuid::new_v4();
        assert_eq!(
            cron_fire_instant("0 9 * * *", now, 60, wf_id),
            Some(now),
            "cron should fire at exact minute boundary, anchored on 09:00:00"
        );
    }

    #[test]
    fn cron_fire_instant_within_drift_window_anchors_on_scheduled_time() {
        // Fixed time: 09:00:45 UTC (45s drift). Cron "0 9 * * *" fires at 09:00.
        // Window [08:59:45, 09:00:45] should still contain 09:00:00. Critically,
        // the anchor is the *scheduled* 09:00:00 — not the drifted `now` — so a
        // second pod ticking at 09:00:50 computes the identical claim key.
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T09:00:45Z")
            .unwrap()
            .with_timezone(&Utc);
        let wf_id = Uuid::new_v4();
        assert_eq!(
            cron_fire_instant("0 9 * * *", now, 60, wf_id),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-06-15T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            ),
            "cron anchor must be the scheduled instant, stable across pod tick drift"
        );
    }

    #[test]
    fn cron_fire_instant_returns_none_just_outside_window() {
        // Fixed time: 09:01:01 UTC. Cron "0 9 * * *" fires at 09:00:00.
        // Window [09:00:01, 09:01:01] does NOT contain 09:00:00.
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T09:01:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let wf_id = Uuid::new_v4();
        assert!(
            cron_fire_instant("0 9 * * *", now, 60, wf_id).is_none(),
            "cron should not fire 61s after the scheduled time"
        );
    }

    #[test]
    fn interval_fire_instant_quantizes_to_bucket_boundary() {
        // Two pods ticking at different sub-interval offsets must compute the
        // *same* bucket boundary so they collide on one claim. 1h interval,
        // epoch-aligned: 12:34:56 and 12:59:01 both floor to 12:00:00.
        let wf_id = Uuid::new_v4();
        let a = chrono::DateTime::parse_from_rfc3339("2026-06-15T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        let b = chrono::DateTime::parse_from_rfc3339("2026-06-15T12:59:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let bucket = chrono::DateTime::parse_from_rfc3339("2026-06-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(interval_fire_instant("1h", a, wf_id), Some(bucket));
        assert_eq!(interval_fire_instant("1h", b, wf_id), Some(bucket));
        // Next hour is a distinct bucket.
        let c = chrono::DateTime::parse_from_rfc3339("2026-06-15T13:00:10Z")
            .unwrap()
            .with_timezone(&Utc);
        let next_bucket = chrono::DateTime::parse_from_rfc3339("2026-06-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(interval_fire_instant("1h", c, wf_id), Some(next_bucket));
    }

    #[test]
    fn interval_fire_instant_returns_none_for_invalid_duration() {
        let now = Utc::now();
        let wf_id = Uuid::new_v4();
        assert!(interval_fire_instant("not-a-duration", now, wf_id).is_none());
    }

    #[test]
    fn interval_should_fire_returns_false_on_first_tick() {
        // When last_fired is None (first tick), defaults to now → elapsed = 0 → false.
        let now = Utc::now();
        let wf_id = Uuid::new_v4();
        assert!(
            !interval_should_fire("1h", None, now, wf_id),
            "first tick should not fire immediately"
        );
    }

    #[test]
    fn interval_should_fire_returns_true_after_interval_elapsed() {
        let wf_id = Uuid::new_v4();
        let now = Utc::now();
        // last_fired was 2 hours ago; interval is 1h → should fire.
        let last = now - chrono::Duration::hours(2);
        assert!(
            interval_should_fire("1h", Some(last), now, wf_id),
            "should fire after interval elapsed"
        );
    }

    #[test]
    fn interval_should_fire_returns_false_before_interval_elapsed() {
        let wf_id = Uuid::new_v4();
        let now = Utc::now();
        // last_fired was 30 minutes ago; interval is 1h → should not fire.
        let last = now - chrono::Duration::minutes(30);
        assert!(
            !interval_should_fire("1h", Some(last), now, wf_id),
            "should not fire before interval elapsed"
        );
    }

    #[test]
    fn interval_should_fire_returns_false_for_invalid_duration() {
        let now = Utc::now();
        let wf_id = Uuid::new_v4();
        assert!(
            !interval_should_fire("not-a-duration", None, now, wf_id),
            "invalid duration should return false"
        );
    }

    #[test]
    fn interval_should_fire_at_exact_boundary() {
        let wf_id = Uuid::new_v4();
        let now = Utc::now();
        // last_fired was exactly 1 hour ago; interval is 1h → should fire (elapsed >= interval).
        let last = now - chrono::Duration::hours(1);
        assert!(
            interval_should_fire("1h", Some(last), now, wf_id),
            "should fire at exact interval boundary"
        );
    }

    // ── Interval cold-start liveness (Max's blocker on the scheduled lane) ──
    // A brand-new interval workflow has no in-memory anchor and no prior durable
    // claim, so the prefilter resolves `last = None`. Without seeding, every tick
    // reads `None`, suppresses, and writes nothing — the workflow never fires.
    // `interval_prefilter_should_fire` must seed `now` on that first suppress so a
    // real anchor exists for the next tick.

    #[test]
    fn interval_cold_start_seeds_anchor_then_fires_after_one_interval() {
        let map: DashMap<(CommunityId, Uuid), DateTime<Utc>> = DashMap::new();
        let community = CommunityId::from_uuid(Uuid::new_v4());
        let wf = Uuid::new_v4();
        let t0 = Utc::now();

        // Tick 1 (cold start): no in-memory entry, DB anchor is None → last = None.
        let fired_1 = interval_prefilter_should_fire(&map, community, wf, "1h", None, t0);
        assert!(!fired_1, "first tick must suppress (wait a full interval)");
        let seeded = map.get(&(community, wf)).map(|v| *v);
        assert_eq!(
            seeded,
            Some(t0),
            "first suppressed tick must seed the anchor to `now`, else it suppresses forever"
        );

        // Tick 2, mid-interval: caller now passes the seeded anchor as `last`.
        let t1 = t0 + chrono::Duration::minutes(30);
        let last = map.get(&(community, wf)).map(|v| *v);
        let fired_2 = interval_prefilter_should_fire(&map, community, wf, "1h", last, t1);
        assert!(!fired_2, "still mid-interval → suppress");
        assert_eq!(
            map.get(&(community, wf)).map(|v| *v),
            Some(t0),
            "mid-interval suppress must NOT advance the anchor (or it would never elapse)"
        );

        // Tick 3, one interval elapsed → fire.
        let t2 = t0 + chrono::Duration::hours(1);
        let last = map.get(&(community, wf)).map(|v| *v);
        let fired_3 = interval_prefilter_should_fire(&map, community, wf, "1h", last, t2);
        assert!(
            fired_3,
            "after one full interval the cold-started workflow must fire"
        );
    }

    #[test]
    fn interval_prefilter_does_not_advance_existing_anchor_on_suppress() {
        // Regression for the inverse bug: if a `Some` anchor were re-seeded to
        // `now` on every suppressed tick, the interval would never elapse.
        let map: DashMap<(CommunityId, Uuid), DateTime<Utc>> = DashMap::new();
        let community = CommunityId::from_uuid(Uuid::new_v4());
        let wf = Uuid::new_v4();
        let now = Utc::now();
        let anchor = now - chrono::Duration::minutes(10); // 10m into a 1h interval
        map.insert((community, wf), anchor);

        let fired = interval_prefilter_should_fire(&map, community, wf, "1h", Some(anchor), now);
        assert!(!fired, "mid-interval suppress");
        assert_eq!(
            map.get(&(community, wf)).map(|v| *v),
            Some(anchor),
            "existing anchor must be preserved exactly, not advanced to now"
        );
    }

    #[test]
    fn interval_prefilter_passes_through_a_due_fire_without_touching_anchor() {
        // When the interval has elapsed the prefilter returns true and leaves the
        // anchor to the post-claim update path (which writes `now` only on a won
        // claim), so the prefilter must not seed here.
        let map: DashMap<(CommunityId, Uuid), DateTime<Utc>> = DashMap::new();
        let community = CommunityId::from_uuid(Uuid::new_v4());
        let wf = Uuid::new_v4();
        let now = Utc::now();
        let anchor = now - chrono::Duration::hours(2); // overdue on a 1h interval

        let fired = interval_prefilter_should_fire(&map, community, wf, "1h", Some(anchor), now);
        assert!(fired, "overdue interval must fire");
        assert!(
            map.get(&(community, wf)).is_none(),
            "a firing tick must not seed via the prefilter; the post-claim path owns the write"
        );
    }

    #[test]
    fn workflow_config_defaults() {
        let cfg = WorkflowConfig::default();
        assert_eq!(cfg.max_concurrent, 100);
        assert_eq!(cfg.default_timeout_secs, 300);
    }

    #[test]
    fn parse_yaml_roundtrip() {
        let yaml = r#"
name: "Test Workflow"
trigger:
  on: message_posted
steps:
  - id: s1
    action: send_message
    text: "Hello {{trigger.author}}"
"#;
        let (def, json) = WorkflowEngine::parse_yaml(yaml).expect("parse failed");
        assert_eq!(def.name, "Test Workflow");

        let reparsed: WorkflowDef = serde_json::from_str(&json).expect("json round-trip");
        assert_eq!(reparsed.name, def.name);
        assert_eq!(reparsed.steps.len(), 1);
    }

    #[test]
    fn trigger_matches_stream_message() {
        let trigger = TriggerDef::MessagePosted { filter: None };
        assert!(trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_STREAM_MESSAGE
        ));
        assert!(!trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_REACTION
        ));
    }

    #[test]
    fn trigger_matches_reaction() {
        let trigger = TriggerDef::ReactionAdded {
            emoji: None,
            filter: None,
        };
        assert!(trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_REACTION
        ));
        assert!(!trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_STREAM_MESSAGE
        ));
    }

    #[tokio::test]
    async fn reaction_filter_matches_target_message() {
        let yaml = r#"
name: "React to one message"
trigger:
  on: reaction_added
  filter: 'trigger_message_id == "target-message"'
steps:
  - id: wait
    action: delay
    duration: 1s
"#;
        let (def, _) = WorkflowEngine::parse_yaml(yaml).expect("parse failed");
        let mut trigger_ctx = executor::TriggerContext {
            message_id: "target-message".to_owned(),
            ..Default::default()
        };

        assert!(
            should_fire_workflow(&def, &trigger_ctx, Uuid::new_v4()).await,
            "reaction to the selected message should fire"
        );

        trigger_ctx.message_id = "different-message".to_owned();
        assert!(
            !should_fire_workflow(&def, &trigger_ctx, Uuid::new_v4()).await,
            "reaction to a different message should be filtered out"
        );
    }

    #[test]
    fn schedule_trigger_never_matches_events() {
        let trigger = TriggerDef::Schedule {
            cron: Some("0 9 * * 1-5".to_owned()),
            interval: None,
        };
        // Schedule triggers are fired by the cron loop, not by events.
        assert!(!trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_STREAM_MESSAGE
        ));
        assert!(!trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_REACTION
        ));
        assert!(!trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_WORKFLOW_TRIGGERED
        ));
    }

    #[test]
    fn webhook_trigger_never_matches_events() {
        let trigger = TriggerDef::Webhook;
        assert!(!trigger_matches_event(
            &trigger,
            buzz_core::kind::KIND_STREAM_MESSAGE
        ));
        assert!(!trigger_matches_event(&trigger, 0));
    }

    #[test]
    fn message_posted_matches_kind_9_only() {
        let trigger = TriggerDef::MessagePosted { filter: None };
        // Must match KIND_STREAM_MESSAGE = 9.
        assert!(trigger_matches_event(&trigger, 9));
        // Must NOT match reaction (kind 7).
        assert!(!trigger_matches_event(&trigger, 7));
        // Must NOT match forum post (kind 45001).
        assert!(!trigger_matches_event(&trigger, 45001));
        // Must NOT match stream message v2 (kind 40002).
        assert!(!trigger_matches_event(&trigger, 40002));
    }

    #[test]
    fn reaction_added_matches_kind_7_only() {
        let trigger = TriggerDef::ReactionAdded {
            emoji: None,
            filter: None,
        };
        // Must match KIND_REACTION = 7.
        assert!(trigger_matches_event(&trigger, 7));
        // Must NOT match stream message (kind 9).
        assert!(!trigger_matches_event(&trigger, 9));
        // Must NOT match forum post (kind 45001).
        assert!(!trigger_matches_event(&trigger, 45001));
    }

    #[test]
    fn reaction_added_with_emoji_filter_still_matches_kind_7() {
        // The emoji filter is evaluated at execution time, not trigger-matching time.
        // trigger_matches_event only checks the kind number.
        let trigger = TriggerDef::ReactionAdded {
            emoji: Some("thumbsup".to_owned()),
            filter: None,
        };
        assert!(trigger_matches_event(&trigger, 7));
        assert!(!trigger_matches_event(&trigger, 9));
    }

    #[test]
    fn message_posted_with_filter_still_matches_kind_9() {
        // The filter expression is evaluated at execution time, not trigger-matching time.
        let trigger = TriggerDef::MessagePosted {
            filter: Some("str_contains(trigger_text, \"P1\")".to_owned()),
        };
        assert!(trigger_matches_event(&trigger, 9));
        assert!(!trigger_matches_event(&trigger, 7));
    }

    #[test]
    fn workflow_execution_kinds_do_not_match_any_trigger() {
        // Workflow execution events (46001–46012) must never match triggers
        // to prevent infinite loops. The on_event() method filters these out
        // before calling trigger_matches_event, but verify the function itself
        // also returns false for these kinds.
        let msg_trigger = TriggerDef::MessagePosted { filter: None };
        let react_trigger = TriggerDef::ReactionAdded {
            emoji: None,
            filter: None,
        };

        for kind in buzz_core::kind::KIND_WORKFLOW_TRIGGERED
            ..=buzz_core::kind::KIND_WORKFLOW_APPROVAL_DENIED
        {
            assert!(
                !trigger_matches_event(&msg_trigger, kind),
                "message_posted should not match workflow execution kind {kind}"
            );
            assert!(
                !trigger_matches_event(&react_trigger, kind),
                "reaction_added should not match workflow execution kind {kind}"
            );
        }
    }

    #[test]
    fn trigger_matches_event_kind_zero_matches_nothing() {
        // Kind 0 is a profile event — no trigger should match it.
        let msg_trigger = TriggerDef::MessagePosted { filter: None };
        let react_trigger = TriggerDef::ReactionAdded {
            emoji: None,
            filter: None,
        };
        let sched_trigger = TriggerDef::Schedule {
            cron: None,
            interval: Some("1h".to_owned()),
        };
        let webhook_trigger = TriggerDef::Webhook;

        assert!(!trigger_matches_event(&msg_trigger, 0));
        assert!(!trigger_matches_event(&react_trigger, 0));
        assert!(!trigger_matches_event(&sched_trigger, 0));
        assert!(!trigger_matches_event(&webhook_trigger, 0));
    }

    #[test]
    fn diff_posted_matches_kind_40008_only() {
        let trigger = TriggerDef::DiffPosted { filter: None };
        assert!(trigger_matches_event(&trigger, 40008));
        assert!(!trigger_matches_event(&trigger, 9));
        assert!(!trigger_matches_event(&trigger, 7));
    }

    #[test]
    fn message_posted_does_not_match_kind_40008() {
        let trigger = TriggerDef::MessagePosted { filter: None };
        assert!(!trigger_matches_event(&trigger, 40008));
        assert!(trigger_matches_event(&trigger, 9));
    }

    #[test]
    fn workflow_config_custom_values() {
        let cfg = WorkflowConfig {
            max_concurrent: 50,
            default_timeout_secs: 600,
            invoke_agent_enabled: false,
            routine_outcome_deadline_secs: 1800,
        };
        assert_eq!(cfg.max_concurrent, 50);
        assert_eq!(cfg.default_timeout_secs, 600);
    }

    fn make_message_event() -> buzz_core::StoredEvent {
        use nostr::{EventBuilder, Keys, Kind};
        use uuid::Uuid;
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "hello world")
            .tags([])
            .sign_with_keys(&keys)
            .expect("sign");
        buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()))
    }

    /// Create a reaction event with an `e` tag pointing to a target message.
    fn make_reaction_event() -> (buzz_core::StoredEvent, String) {
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let keys = Keys::generate();
        // Create a dummy target message ID (64-char hex).
        let target_keys = Keys::generate();
        let target_event = EventBuilder::new(Kind::Custom(9), "target msg")
            .tags([])
            .sign_with_keys(&target_keys)
            .expect("sign target");
        let target_id_hex = target_event.id.to_hex();
        // NIP-25: reaction references the target via an `e` tag.
        let e_tag = Tag::parse(["e", &target_id_hex]).expect("tag parse");
        let event = EventBuilder::new(Kind::Reaction, "👍")
            .tags([e_tag])
            .sign_with_keys(&keys)
            .expect("sign");
        (
            buzz_core::StoredEvent::new(event, Some(Uuid::new_v4())),
            target_id_hex,
        )
    }

    #[test]
    fn build_trigger_context_message_event() {
        let stored = make_message_event();
        let ctx = build_trigger_context(&stored);

        assert_eq!(ctx.text, "hello world");
        assert_eq!(ctx.author, stored.event.pubkey.to_hex());
        assert_eq!(ctx.channel_id, stored.channel_id.unwrap().to_string());
        assert_eq!(ctx.timestamp, stored.event.created_at.as_secs().to_string());
        assert_eq!(ctx.message_id, stored.event.id.to_hex());
        // Non-reaction events have empty emoji.
        assert_eq!(ctx.emoji, "");
        assert!(ctx.webhook_fields.is_empty());
        // A top-level message (no e-tags) is not a reply.
        assert!(!ctx.is_reply);
    }

    #[test]
    fn build_trigger_context_is_reply_true_for_threaded_message() {
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let root = Keys::generate();
        let root_event = EventBuilder::new(Kind::Custom(9), "root")
            .tags([])
            .sign_with_keys(&root)
            .expect("sign root");
        let root_hex = root_event.id.to_hex();

        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "a threaded reply")
            .tags([
                Tag::parse(["e", &root_hex, "", "root"]).expect("root tag"),
                Tag::parse(["e", &root_hex, "", "reply"]).expect("reply tag"),
            ])
            .sign_with_keys(&keys)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);
        assert!(ctx.is_reply, "message with reply/root e-tags is a reply");
    }

    #[test]
    fn build_trigger_context_is_reply_true_for_reply_only_marker() {
        // A NIP-10 `reply` marker without a `root` marker (the fallback ingest
        // treats as `root == reply`) is still a threaded reply.
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let parent = Keys::generate();
        let parent_event = EventBuilder::new(Kind::Custom(9), "parent")
            .sign_with_keys(&parent)
            .expect("sign parent");
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "reply only")
            .tags([Tag::parse(["e", &parent_event.id.to_hex(), "", "reply"]).expect("reply tag")])
            .sign_with_keys(&keys)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);
        assert!(ctx.is_reply, "a lone `reply` marker is a reply");
    }

    #[test]
    fn build_trigger_context_is_reply_false_for_root_only_marker() {
        // Ingest treats `(root=Some, reply=None)` as top-level, so
        // `event_is_reply` must too — otherwise `trigger_is_reply == false`
        // would skip a message the relay stored as a new top-level post.
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let root = Keys::generate();
        let root_event = EventBuilder::new(Kind::Custom(9), "root")
            .sign_with_keys(&root)
            .expect("sign root");
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "root marker only")
            .tags([Tag::parse(["e", &root_event.id.to_hex(), "", "root"]).expect("root tag")])
            .sign_with_keys(&keys)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);
        assert!(
            !ctx.is_reply,
            "a lone `root` marker is top-level to ingest, not a reply"
        );
    }

    #[test]
    fn build_trigger_context_is_reply_false_for_unmarked_e_tag() {
        // A bare `e` tag with no NIP-10 marker (e.g. a plain mention/quote) is
        // not treated as a thread reply — only `reply`/`root` markers count.
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let other = Keys::generate();
        let other_event = EventBuilder::new(Kind::Custom(9), "other")
            .tags([])
            .sign_with_keys(&other)
            .expect("sign");
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "quotes another")
            .tags([Tag::parse(["e", &other_event.id.to_hex()]).expect("bare e tag")])
            .sign_with_keys(&keys)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);
        assert!(!ctx.is_reply, "unmarked e-tag must not count as a reply");
    }

    #[test]
    fn build_trigger_context_is_reply_false_for_malformed_reply_id() {
        // Ingest gates a marker on a valid 64-hex event id; a malformed reply
        // id is not a thread link, so ingest stores the event top-level. The
        // predicate must agree, or `trigger_is_reply == false` would skip it.
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "malformed reply marker")
            .tags([Tag::parse(["e", "bad", "", "reply"]).expect("reply tag")])
            .sign_with_keys(&keys)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);
        assert!(
            !ctx.is_reply,
            "a malformed reply id is ignored by ingest, so it is top-level"
        );
    }

    #[test]
    fn build_trigger_context_is_reply_false_for_valid_root_malformed_reply() {
        // A valid `root` marker but a malformed `reply` id: ingest ignores the
        // reply and stores the event as root-only, i.e. top-level. The predicate
        // must not flip to reply on the malformed marker.
        use nostr::{EventBuilder, Keys, Kind, Tag};
        use uuid::Uuid;
        let root = Keys::generate();
        let root_event = EventBuilder::new(Kind::Custom(9), "root")
            .sign_with_keys(&root)
            .expect("sign root");
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "valid root, malformed reply")
            .tags([
                Tag::parse(["e", &root_event.id.to_hex(), "", "root"]).expect("root tag"),
                Tag::parse(["e", "bad", "", "reply"]).expect("reply tag"),
            ])
            .sign_with_keys(&keys)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);
        assert!(
            !ctx.is_reply,
            "a valid root with a malformed reply id is top-level to ingest"
        );
    }

    #[test]
    fn build_trigger_context_reaction_event() {
        let (stored, target_id_hex) = make_reaction_event();
        let ctx = build_trigger_context(&stored);

        // For reactions, content IS the emoji.
        assert_eq!(ctx.text, "👍");
        assert_eq!(ctx.emoji, "👍");
        assert_eq!(ctx.author, stored.event.pubkey.to_hex());
        // message_id should be the TARGET message, not the reaction event itself.
        assert_eq!(ctx.message_id, target_id_hex);
        assert_ne!(ctx.message_id, stored.event.id.to_hex());
        assert!(ctx.webhook_fields.is_empty());
    }

    #[test]
    fn build_trigger_context_no_channel_id() {
        use nostr::{EventBuilder, Keys, Kind};
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "msg")
            .tags([])
            .sign_with_keys(&keys)
            .expect("sign");
        // channel_id = None (global/DM event)
        let stored = buzz_core::StoredEvent::new(event, None);
        let ctx = build_trigger_context(&stored);

        assert_eq!(ctx.channel_id, "");
        assert_eq!(ctx.text, "msg");
    }

    #[test]
    fn build_trigger_context_author_is_hex_pubkey() {
        let stored = make_message_event();
        let ctx = build_trigger_context(&stored);
        // Pubkey hex is 64 lowercase hex characters.
        assert_eq!(ctx.author.len(), 64);
        assert!(ctx.author.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn build_trigger_context_ignores_actor_tag() {
        use nostr::{EventBuilder, Keys, Kind, Tag};

        let signer = Keys::generate();
        let impersonated = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "forged actor")
            .tags([Tag::parse(["actor", &impersonated.public_key().to_hex()]).expect("actor tag")])
            .sign_with_keys(&signer)
            .expect("sign");
        let stored = buzz_core::StoredEvent::new(event, Some(uuid::Uuid::new_v4()));

        let ctx = build_trigger_context(&stored);

        assert_eq!(ctx.author, signer.public_key().to_hex());
        assert_ne!(ctx.author, impersonated.public_key().to_hex());
    }

    #[test]
    fn build_trigger_context_message_id_is_hex() {
        let stored = make_message_event();
        let ctx = build_trigger_context(&stored);
        // Event ID hex is 64 lowercase hex characters.
        assert_eq!(ctx.message_id.len(), 64);
        assert!(ctx.message_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn build_trigger_context_timestamp_is_numeric_string() {
        let stored = make_message_event();
        let ctx = build_trigger_context(&stored);
        // Timestamp must parse as a u64.
        ctx.timestamp
            .parse::<u64>()
            .expect("timestamp should be a u64 string");
    }

    #[test]
    fn test_build_trigger_context_reaction_multiple_e_tags() {
        // NIP-25: last e tag is the direct target, first may be thread root
        use nostr::{EventBuilder, EventId, Keys, Kind, Tag};
        use uuid::Uuid;

        let keys = Keys::generate();
        let thread_root_id = EventId::all_zeros();
        let direct_target_id = EventId::from_byte_array([0x42; 32]);

        let event = EventBuilder::new(Kind::Reaction, "👍")
            .tags([
                Tag::parse(["e", &thread_root_id.to_hex()]).unwrap(),
                Tag::parse(["e", &direct_target_id.to_hex()]).unwrap(),
            ])
            .sign_with_keys(&keys)
            .expect("sign");

        let stored = buzz_core::StoredEvent::new(event, Some(Uuid::new_v4()));
        let ctx = build_trigger_context(&stored);

        // Should pick the LAST e tag (direct target), not the first (thread root)
        assert_eq!(ctx.message_id, direct_target_id.to_hex());
    }

    // -- SEC-006: owner authority decision --------------------------------

    #[test]
    fn owner_authority_denies_non_members_always() {
        assert!(!owner_authority_allows(None, false));
        assert!(!owner_authority_allows(None, true));
    }

    #[test]
    fn owner_authority_allows_any_member_for_ordinary_definitions() {
        assert!(owner_authority_allows(Some("member"), false));
        assert!(owner_authority_allows(Some("admin"), false));
        assert!(owner_authority_allows(Some("owner"), false));
    }

    #[test]
    fn owner_authority_requires_elevated_role_for_exfiltration_definitions() {
        assert!(!owner_authority_allows(Some("member"), true));
        assert!(owner_authority_allows(Some("admin"), true));
        assert!(owner_authority_allows(Some("owner"), true));
    }

    #[test]
    fn requires_elevated_authority_detects_call_webhook() {
        let (plain, _) = WorkflowEngine::parse_yaml(concat!(
            "name: plain\n",
            "trigger:\n  on: message_posted\n",
            "steps:\n  - id: s1\n    action: send_message\n    text: hi\n",
        ))
        .expect("parse plain");
        assert!(!plain.requires_elevated_authority());

        let (hook, _) = WorkflowEngine::parse_yaml(concat!(
            "name: hook\n",
            "trigger:\n  on: message_posted\n",
            "steps:\n  - id: s1\n    action: send_message\n    text: hi\n",
            "  - id: s2\n    action: call_webhook\n    url: https://example.com/x\n",
        ))
        .expect("parse hook");
        assert!(hook.requires_elevated_authority());
    }

    // -- SEC-006: event-path regression (requires Postgres) ----------------

    async fn setup_db() -> buzz_db::Db {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            // Local-only test default; this is not a production credential.
            .unwrap_or_else(|_| {
                let local_test_database = "postgres://buzz:buzz_dev@localhost:5432/buzz"; // sadscan:disable np.postgres.1
                local_test_database.to_owned()
            });
        buzz_db::Db::new(&buzz_db::DbConfig {
            database_url,
            ..Default::default()
        })
        .await
        .expect("connect test DB")
    }

    /// Create a community, a channel owned by `creator`, and add `member` as a
    /// plain member. Returns `(community, channel)`.
    async fn setup_channel(db: &buzz_db::Db, creator: &[u8], member: &[u8]) -> (CommunityId, Uuid) {
        let host = format!("sec006-{}.example", Uuid::new_v4().simple());
        let community = match db
            .create_community_with_owner(&host, &hex::encode(creator))
            .await
            .expect("create community")
        {
            buzz_db::CreateCommunityWithOwnerResult::Created(rec) => rec.id,
            other => panic!("unexpected community create result: {other:?}"),
        };
        db.ensure_user(community, creator)
            .await
            .expect("creator user");
        db.ensure_user(community, member)
            .await
            .expect("member user");
        let channel_id = Uuid::new_v4();
        db.create_channel_with_id(
            community,
            channel_id,
            &format!("ch-{}", channel_id.simple()),
            buzz_db::channel::ChannelType::Stream,
            buzz_db::channel::ChannelVisibility::Open,
            None,
            creator,
            None,
        )
        .await
        .expect("create channel");
        db.add_member(
            community,
            channel_id,
            member,
            buzz_db::channel::MemberRole::Member,
            Some(creator),
        )
        .await
        .expect("add member");
        (community, channel_id)
    }

    fn message_event(channel_id: Uuid) -> buzz_core::StoredEvent {
        let keys = nostr::Keys::generate();
        let event = nostr::EventBuilder::new(nostr::Kind::Custom(9), "hello")
            .sign_with_keys(&keys)
            .expect("sign");
        buzz_core::StoredEvent::new(event, Some(channel_id))
    }

    /// The event path must stop creating runs the moment the workflow's owner
    /// loses channel membership — even while the workflow row is still
    /// `enabled` (the disable-on-removal side effect is a separate, relay-side
    /// write; this gate must hold on its own).
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn on_event_denies_run_after_owner_removed() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let member = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator, &member).await;

        let def_json = serde_json::json!({
            "name": "sec006-event",
            "trigger": {"on": "message_posted"},
            "steps": [{"id": "s1", "action": "send_message", "text": "hi"}],
            "enabled": true,
        })
        .to_string();
        let workflow_id = db
            .create_workflow(
                community,
                Some(channel_id),
                &member,
                "sec006-event",
                &def_json,
                &[0u8; 32],
            )
            .await
            .expect("create workflow");

        let engine = Arc::new(WorkflowEngine::new(db.clone(), WorkflowConfig::default()));

        // Owner is an active member: the event fires the workflow.
        engine
            .on_event(community, &message_event(channel_id))
            .await
            .expect("on_event while member");
        let runs = db
            .list_workflow_runs(community, workflow_id, 10)
            .await
            .expect("list runs");
        assert_eq!(runs.len(), 1, "member owner's workflow must fire");

        // Remove the owner (actor = channel creator, an owner-role member).
        db.remove_member(community, channel_id, &member, &creator)
            .await
            .expect("remove member");

        // Workflow row is still enabled — only the authority gate stands.
        engine
            .on_event(community, &message_event(channel_id))
            .await
            .expect("on_event after removal");
        let runs = db
            .list_workflow_runs(community, workflow_id, 10)
            .await
            .expect("list runs after removal");
        assert_eq!(
            runs.len(),
            1,
            "no new run may be created after the owner lost membership"
        );
    }

    /// Exfiltration-capable definitions (call_webhook) require the owner to
    /// currently hold an elevated role — a plain member's workflow must not
    /// fire even though the owner is still an active channel member.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn on_event_denies_webhook_definition_for_plain_member_owner() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let member = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator, &member).await;

        let def_json = serde_json::json!({
            "name": "sec006-hook",
            "trigger": {"on": "message_posted"},
            "steps": [{"id": "s1", "action": "call_webhook", "url": "https://example.com/x"}],
            "enabled": true,
        })
        .to_string();

        // Same definition, two owners: plain member vs channel owner.
        let wf_member = db
            .create_workflow(
                community,
                Some(channel_id),
                &member,
                "hook-member",
                &def_json,
                &[0u8; 32],
            )
            .await
            .expect("create member workflow");
        let wf_owner = db
            .create_workflow(
                community,
                Some(channel_id),
                &creator,
                "hook-owner",
                &def_json,
                &[1u8; 32],
            )
            .await
            .expect("create owner workflow");

        let engine = Arc::new(WorkflowEngine::new(db.clone(), WorkflowConfig::default()));
        engine
            .on_event(community, &message_event(channel_id))
            .await
            .expect("on_event");

        let member_runs = db
            .list_workflow_runs(community, wf_member, 10)
            .await
            .expect("member runs");
        assert!(
            member_runs.is_empty(),
            "plain member's call_webhook workflow must not fire"
        );
        let owner_runs = db
            .list_workflow_runs(community, wf_owner, 10)
            .await
            .expect("owner runs");
        assert_eq!(
            owner_runs.len(),
            1,
            "channel owner's call_webhook workflow fires"
        );
    }
}

#[cfg(test)]
mod routine_tests {
    use super::*;
    use crate::action_sink::{
        ActionSink, ActionSinkError, InvokeAgentOutcome, InvokeAgentRequest,
    };
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Records calls and returns a caller-configured outcome. Real dedupe is
    /// exercised through the DB (`insert_routine_dispatch`), not this sink —
    /// these tests drive the DB layer and settlement directly, mirroring the
    /// relay's actual call sequence, since `RelayActionSink` itself is Step 4.
    #[derive(Default)]
    struct RecordingSink {
        sent_messages: Mutex<Vec<(String, String)>>,
    }

    impl ActionSink for RecordingSink {
        fn send_message(
            &self,
            _community_id: CommunityId,
            channel_id: &str,
            text: &str,
            _authored_text: &str,
            _author_pubkey: &str,
            _reply_to: Option<&str>,
        ) -> Pin<Box<dyn Future<Output = Result<String, ActionSinkError>> + Send + '_>> {
            self.sent_messages
                .lock()
                .unwrap()
                .push((channel_id.to_owned(), text.to_owned()));
            Box::pin(async { Ok(Uuid::new_v4().to_string()) })
        }

        fn invoke_agent(
            &self,
            _request: InvokeAgentRequest,
        ) -> Pin<Box<dyn Future<Output = Result<InvokeAgentOutcome, ActionSinkError>> + Send + '_>>
        {
            Box::pin(async {
                Ok(InvokeAgentOutcome::Dispatched {
                    wake_event_id: "a".repeat(64),
                })
            })
        }
    }

    async fn setup_db() -> buzz_db::Db {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| {
                let local_test_database = "postgres://buzz:buzz_dev@localhost:5432/buzz"; // sadscan:disable np.postgres.1
                local_test_database.to_owned()
            });
        buzz_db::Db::new(&buzz_db::DbConfig {
            database_url,
            ..Default::default()
        })
        .await
        .expect("connect test DB")
    }

    async fn setup_channel(db: &buzz_db::Db, creator: &[u8]) -> (CommunityId, Uuid) {
        let host = format!("routine-{}.example", Uuid::new_v4().simple());
        let community = match db
            .create_community_with_owner(&host, &hex::encode(creator))
            .await
            .expect("create community")
        {
            buzz_db::CreateCommunityWithOwnerResult::Created(rec) => rec.id,
            other => panic!("unexpected community create result: {other:?}"),
        };
        db.ensure_user(community, creator).await.expect("creator user");
        let channel_id = Uuid::new_v4();
        db.create_channel_with_id(
            community,
            channel_id,
            &format!("ch-{}", channel_id.simple()),
            buzz_db::channel::ChannelType::Stream,
            buzz_db::channel::ChannelVisibility::Open,
            None,
            creator,
            None,
        )
        .await
        .expect("create channel");
        (community, channel_id)
    }

    fn invoke_agent_def_json(agent_pubkey_hex: &str, result_channel: Uuid, idempotency_key: &str) -> String {
        serde_json::json!({
            "name": "routine",
            "trigger": {"on": "schedule", "interval": "15m"},
            "steps": [{
                "id": "invoke",
                "action": "invoke_agent",
                "agent_pubkey": agent_pubkey_hex,
                "prompt": "do work",
                "result_channel": result_channel.to_string(),
                "idempotency_key": idempotency_key,
                "token_budget_per_run": 100000,
                "token_budget_per_day": 1000000,
            }],
            "enabled": true,
        })
        .to_string()
    }

    /// Create a channel in `community` that no workflow is ever saved onto.
    ///
    /// G1R F-3 / I-16: outcome-settlement tests must post the outcome to a
    /// BARE result channel (zero enabled workflows), otherwise the
    /// `workflows.is_empty()` early return in `on_event` is never exercised
    /// and a regression of the settlement-branch placement would go unnoticed.
    async fn create_bare_channel(db: &buzz_db::Db, community: CommunityId, creator: &[u8]) -> Uuid {
        let channel_id = Uuid::new_v4();
        db.create_channel_with_id(
            community,
            channel_id,
            &format!("ch-{}", channel_id.simple()),
            buzz_db::channel::ChannelType::Stream,
            buzz_db::channel::ChannelVisibility::Open,
            None,
            creator,
            None,
        )
        .await
        .expect("create bare result channel");
        channel_id
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn invoke_agent_env_off_is_not_implemented() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        let agent_pubkey_hex = nostr::Keys::generate().public_key().to_string();
        let def_json = invoke_agent_def_json(&agent_pubkey_hex, channel_id, "run-1");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");

        let engine = Arc::new(WorkflowEngine::new(db.clone(), WorkflowConfig::default()));
        engine.set_action_sink(Arc::new(RecordingSink::default()));

        let def: WorkflowDef = serde_json::from_str(&def_json).unwrap();
        let ctx = executor::TriggerContext {
            channel_id: channel_id.to_string(),
            ..Default::default()
        };
        let run_id = db
            .create_workflow_run(community, workflow_id, None, None)
            .await
            .expect("create run");
        let result = executor::execute_run(&engine, community, run_id, &def, &ctx).await;
        assert!(
            matches!(&result, Err((WorkflowError::NotImplemented(action), _)) if action == "InvokeAgent"),
            "invoke_agent must be NotImplemented when the env switch is off: {result:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn invoke_agent_dispatch_leaves_run_running() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        let agent_pubkey_hex = nostr::Keys::generate().public_key().to_string();
        let def_json = invoke_agent_def_json(&agent_pubkey_hex, channel_id, "run-1");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");

        let mut config = WorkflowConfig::default();
        config.invoke_agent_enabled = true;
        let engine = Arc::new(WorkflowEngine::new(db.clone(), config));
        engine.set_action_sink(Arc::new(RecordingSink::default()));

        let def: WorkflowDef = serde_json::from_str(&def_json).unwrap();
        let ctx = executor::TriggerContext {
            channel_id: channel_id.to_string(),
            ..Default::default()
        };
        let run_id = db
            .create_workflow_run(community, workflow_id, None, None)
            .await
            .expect("create run");
        let result = executor::execute_run(&engine, community, run_id, &def, &ctx).await;
        let exec_result = result.expect("dispatch should succeed at the executor level");
        engine.finalize_run(community, run_id, Ok(exec_result), None).await;

        let run = db.get_workflow_run(community, run_id).await.expect("get run");
        assert_eq!(run.status, buzz_db::workflow::RunStatus::Running);
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn invoke_agent_dedup_and_busy_outcomes_complete() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        let agent_pubkey_hex = nostr::Keys::generate().public_key().to_string();
        let def_json = invoke_agent_def_json(&agent_pubkey_hex, channel_id, "same-key");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");

        // Deduplicated: a second insert with the same idempotency key fails.
        let run_a = db
            .create_workflow_run(community, workflow_id, None, None)
            .await
            .expect("create run a");
        let inserted_a = db
            .insert_routine_dispatch(
                community,
                run_a,
                workflow_id,
                &[1u8; 32],
                channel_id,
                "same-key",
                Utc::now(),
            )
            .await
            .expect("insert a");
        assert!(inserted_a);

        let run_b = db
            .create_workflow_run(community, workflow_id, None, None)
            .await
            .expect("create run b");
        let inserted_b = db
            .insert_routine_dispatch(
                community,
                run_b,
                workflow_id,
                &[1u8; 32],
                channel_id,
                "same-key",
                Utc::now(),
            )
            .await
            .expect("insert b (dedupe)");
        assert!(!inserted_b, "same idempotency key must not insert a second row");

        // Busy: an open dispatch is visible via has_open_routine_dispatch.
        let busy = db
            .has_open_routine_dispatch(community, workflow_id)
            .await
            .expect("check busy");
        assert!(busy, "workflow has an open dispatch from run_a");

        // Settling run_a's dispatch clears the busy flag.
        db.mark_routine_dispatched(community, run_a, b"wake-event-id-bytes")
            .await
            .expect("mark dispatched");
        db.settle_routine_dispatch(
            community,
            run_a,
            "succeeded",
            buzz_db::workflow::RunStatus::Completed,
            None,
        )
        .await
        .expect("settle run a");
        let busy_after = db
            .has_open_routine_dispatch(community, workflow_id)
            .await
            .expect("check busy after settle");
        assert!(!busy_after, "settling the open dispatch clears busy");

        let state = db
            .get_routine_state(community, workflow_id)
            .await
            .expect("get state")
            .expect("state row exists");
        assert_eq!(
            state.consecutive_failures, 0,
            "routine_skipped_busy and dedupe never touch consecutive_failures"
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn settle_rejects_wrong_signer_and_unknown_run() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        // G1R F-3: outcomes settle on a bare result channel (zero enabled
        // workflows) — the definition lives on `channel_id`.
        let result_channel = create_bare_channel(&db, community, &creator).await;
        let agent_keys = nostr::Keys::generate();
        let agent_pubkey_hex = agent_keys.public_key().to_string();
        let def_json = invoke_agent_def_json(&agent_pubkey_hex, result_channel, "run-1");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");
        let run_id = db
            .create_workflow_run(community, workflow_id, None, None)
            .await
            .expect("create run");
        db.insert_routine_dispatch(
            community,
            run_id,
            workflow_id,
            &agent_keys.public_key().to_bytes(),
            result_channel,
            "run-1",
            Utc::now(),
        )
        .await
        .expect("insert dispatch");
        db.mark_routine_dispatched(community, run_id, b"wake-event-id-bytes")
            .await
            .expect("mark dispatched");

        let engine = Arc::new(WorkflowEngine::new(db.clone(), WorkflowConfig::default()));
        engine.set_action_sink(Arc::new(RecordingSink::default()));

        // Wrong signer.
        let wrong_signer_event = nostr::EventBuilder::new(nostr::Kind::Custom(9), "outcome")
            .tags([
                nostr::Tag::parse(["h", &result_channel.to_string()]).unwrap(),
                nostr::Tag::parse(["buzz:routine-run", &run_id.to_string()]).unwrap(),
                nostr::Tag::parse(["buzz:routine-outcome", "succeeded"]).unwrap(),
            ])
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap();
        let stored = buzz_core::StoredEvent::new(wrong_signer_event, Some(result_channel));
        engine.on_event(community, &stored).await.expect("on_event wrong signer");
        let dispatch = db
            .get_open_routine_dispatch(community, run_id)
            .await
            .expect("get dispatch");
        assert!(dispatch.is_some(), "wrong-signer outcome must not settle the dispatch");

        // Unknown run.
        let unknown_run_id = Uuid::new_v4();
        let unknown_run_event = nostr::EventBuilder::new(nostr::Kind::Custom(9), "outcome")
            .tags([
                nostr::Tag::parse(["h", &result_channel.to_string()]).unwrap(),
                nostr::Tag::parse(["buzz:routine-run", &unknown_run_id.to_string()]).unwrap(),
                nostr::Tag::parse(["buzz:routine-outcome", "succeeded"]).unwrap(),
            ])
            .sign_with_keys(&agent_keys)
            .unwrap();
        let stored = buzz_core::StoredEvent::new(unknown_run_event, Some(result_channel));
        engine.on_event(community, &stored).await.expect("on_event unknown run");

        // Correct signer settles it — on the bare result channel, so this
        // fails if the settlement branch ever moves after the workflow-cache
        // lookup / `is_empty` early return in `on_event`.
        let ok_event = nostr::EventBuilder::new(nostr::Kind::Custom(9), "outcome")
            .tags([
                nostr::Tag::parse(["h", &result_channel.to_string()]).unwrap(),
                nostr::Tag::parse(["buzz:routine-run", &run_id.to_string()]).unwrap(),
                nostr::Tag::parse(["buzz:routine-outcome", "succeeded"]).unwrap(),
            ])
            .sign_with_keys(&agent_keys)
            .unwrap();
        let stored = buzz_core::StoredEvent::new(ok_event, Some(result_channel));
        engine.on_event(community, &stored).await.expect("on_event correct signer");
        let dispatch = db
            .get_open_routine_dispatch(community, run_id)
            .await
            .expect("get dispatch");
        assert!(dispatch.is_none(), "correct-signer outcome must settle the dispatch");
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn strikes_reset_on_success_and_pause_at_ten() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        // G1R F-3: outcomes settle on a bare result channel (zero enabled
        // workflows) — the definition lives on `channel_id`.
        let result_channel = create_bare_channel(&db, community, &creator).await;
        let agent_keys = nostr::Keys::generate();
        let def_json = invoke_agent_def_json(&agent_keys.public_key().to_string(), result_channel, "run-1");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");

        let engine = Arc::new(WorkflowEngine::new(db.clone(), WorkflowConfig::default()));
        engine.set_action_sink(Arc::new(RecordingSink::default()));

        let settle = |outcome: &'static str| {
            let db = db.clone();
            let engine = Arc::clone(&engine);
            let agent_keys = agent_keys.clone();
            let channel_id = result_channel;
            let workflow_id = workflow_id;
            let community = community;
            async move {
                let run_id = db
                    .create_workflow_run(community, workflow_id, None, None)
                    .await
                    .expect("create run");
                db.insert_routine_dispatch(
                    community,
                    run_id,
                    workflow_id,
                    &agent_keys.public_key().to_bytes(),
                    channel_id,
                    &run_id.to_string(),
                    Utc::now(),
                )
                .await
                .expect("insert dispatch");
                db.mark_routine_dispatched(community, run_id, b"wake-event-id-bytes")
                    .await
                    .expect("mark dispatched");
                let event = nostr::EventBuilder::new(nostr::Kind::Custom(9), "outcome")
                    .tags([
                        nostr::Tag::parse(["h", &channel_id.to_string()]).unwrap(),
                        nostr::Tag::parse(["buzz:routine-run", &run_id.to_string()]).unwrap(),
                        nostr::Tag::parse(["buzz:routine-outcome", outcome]).unwrap(),
                    ])
                    .sign_with_keys(&agent_keys)
                    .unwrap();
                let stored = buzz_core::StoredEvent::new(event, Some(channel_id));
                engine.on_event(community, &stored).await.expect("on_event");
            }
        };

        for _ in 0..9 {
            settle("failed").await;
        }
        settle("succeeded").await;
        let state = db
            .get_routine_state(community, workflow_id)
            .await
            .expect("get state")
            .expect("state row exists");
        assert_eq!(state.consecutive_failures, 0, "success resets strikes");

        for _ in 0..10 {
            settle("failed").await;
        }
        let state = db
            .get_routine_state(community, workflow_id)
            .await
            .expect("get state")
            .expect("state row exists");
        assert_eq!(state.consecutive_failures, 10);
        assert_eq!(state.paused_reason.as_deref(), Some("strikes"));
        let workflow = db.get_workflow(community, workflow_id).await.expect("get workflow");
        assert_eq!(workflow.status, buzz_db::workflow::WorkflowStatus::Disabled);
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn daily_notice_once_per_day() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        // G1R F-3: outcomes settle on a bare result channel (zero enabled
        // workflows) — the definition lives on `channel_id`.
        let result_channel = create_bare_channel(&db, community, &creator).await;
        let agent_keys = nostr::Keys::generate();
        let def_json = invoke_agent_def_json(&agent_keys.public_key().to_string(), result_channel, "run-1");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");

        let sink = Arc::new(RecordingSink::default());
        let engine = Arc::new(WorkflowEngine::new(db.clone(), WorkflowConfig::default()));
        engine.set_action_sink(Arc::clone(&sink) as Arc<dyn ActionSink>);

        for _ in 0..2 {
            let run_id = db
                .create_workflow_run(community, workflow_id, None, None)
                .await
                .expect("create run");
            db.insert_routine_dispatch(
                community,
                run_id,
                workflow_id,
                &agent_keys.public_key().to_bytes(),
                result_channel,
                &run_id.to_string(),
                Utc::now(),
            )
            .await
            .expect("insert dispatch");
            db.mark_routine_dispatched(community, run_id, b"wake-event-id-bytes")
                .await
                .expect("mark dispatched");
            let event = nostr::EventBuilder::new(nostr::Kind::Custom(9), "outcome")
                .tags([
                    nostr::Tag::parse(["h", &result_channel.to_string()]).unwrap(),
                    nostr::Tag::parse(["buzz:routine-run", &run_id.to_string()]).unwrap(),
                    nostr::Tag::parse(["buzz:routine-outcome", "budget_exceeded_daily"]).unwrap(),
                ])
                .sign_with_keys(&agent_keys)
                .unwrap();
            let stored = buzz_core::StoredEvent::new(event, Some(result_channel));
            engine.on_event(community, &stored).await.expect("on_event");
        }

        let state = db
            .get_routine_state(community, workflow_id)
            .await
            .expect("get state")
            .expect("state row exists");
        assert_eq!(state.paused_reason.as_deref(), Some("daily_budget"));
        let notices = sink.sent_messages.lock().unwrap();
        assert_eq!(notices.len(), 1, "at most one daily-budget notice per UTC day");
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn sweeper_expires_open_dispatch() {
        let db = setup_db().await;
        let creator = nostr::Keys::generate().public_key().to_bytes().to_vec();
        let (community, channel_id) = setup_channel(&db, &creator).await;
        let agent_keys = nostr::Keys::generate();
        let def_json = invoke_agent_def_json(&agent_keys.public_key().to_string(), channel_id, "run-1");
        let workflow_id = db
            .create_workflow(community, Some(channel_id), &creator, "r", &def_json, &[0u8; 32])
            .await
            .expect("create workflow");
        let run_id = db
            .create_workflow_run(community, workflow_id, None, None)
            .await
            .expect("create run");
        db.insert_routine_dispatch(
            community,
            run_id,
            workflow_id,
            &agent_keys.public_key().to_bytes(),
            channel_id,
            "run-1",
            Utc::now(),
        )
        .await
        .expect("insert dispatch");
        db.mark_routine_dispatched(community, run_id, b"wake-event-id-bytes")
            .await
            .expect("mark dispatched");

        // Expire everything dispatched before "the future" — simulates the
        // deadline having elapsed without calling the real sleep. Other
        // tests in this shared DB leave their own open dispatches behind, so
        // assert this run's presence rather than the total count.
        let expired = db
            .expire_routine_dispatches(Utc::now() + chrono::Duration::seconds(5))
            .await
            .expect("expire dispatches");
        assert!(
            expired.iter().any(|(c, r)| *c == community && *r == run_id),
            "this test's dispatch must be among the expired rows"
        );
        let (expired_community, expired_run_id) = (community, run_id);

        let settlement = db
            .settle_routine_dispatch(
                expired_community,
                expired_run_id,
                "timeout",
                buzz_db::workflow::RunStatus::Failed,
                Some("routine_timeout"),
            )
            .await
            .expect("settle timeout");
        assert_eq!(settlement.consecutive_failures, 1);

        let run = db.get_workflow_run(community, run_id).await.expect("get run");
        assert_eq!(run.error_code.as_deref(), Some("routine_timeout"));
    }
}
