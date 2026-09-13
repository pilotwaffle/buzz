//! Routine (invoke_agent workflow) store operations.
//!
//! All IDs are native Postgres UUID columns. Never uses string interpolation
//! for query values — all user data goes through bind parameters.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use buzz_core::CommunityId;

use crate::error::Result;
use crate::workflow::RunStatus;
use crate::Db;
use buzz_datastore_tracing::datastore_span;

// -- Record types --------------------------------------------------------------

/// A row from `routine_dispatches`.
#[derive(Debug, Clone)]
pub struct RoutineDispatchRecord {
    pub community_id: CommunityId,
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub agent_pubkey: Vec<u8>,
    pub result_channel: Uuid,
    pub idempotency_key: String,
    pub fire_instant: DateTime<Utc>,
    pub wake_event_id: Option<Vec<u8>>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub settled_at: Option<DateTime<Utc>>,
    pub outcome: Option<String>,
}

/// Settlement result returned by `settle_routine_dispatch`.
#[derive(Debug, Clone)]
pub struct RoutineSettlement {
    pub workflow_id: Uuid,
    pub consecutive_failures: i32,
    pub paused_reason: Option<String>,
    pub notice_due: bool,
    pub result_channel: Uuid,
    pub workflow_name: String,
    pub owner_pubkey: Vec<u8>,
    pub dispatched_at: Option<DateTime<Utc>>,
}

/// A row from `routine_state`.
#[derive(Debug, Clone)]
pub struct RoutineStateRecord {
    pub community_id: CommunityId,
    pub workflow_id: Uuid,
    pub consecutive_failures: i32,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub last_outcome: Option<String>,
    pub paused_reason: Option<String>,
    pub paused_at: Option<DateTime<Utc>>,
    pub daily_notice_day: Option<NaiveDate>,
    pub status: Option<String>,
}

// -- Free functions ------------------------------------------------------------

/// Insert a routine dispatch row. Returns `true` on insert, `false` on conflict.
#[datastore_span(name = "insert_routine_dispatch", system = "postgresql")]
pub async fn insert_routine_dispatch(
    pool: &PgPool,
    community_id: CommunityId,
    run_id: Uuid,
    workflow_id: Uuid,
    agent_pubkey: &[u8],
    result_channel: Uuid,
    idempotency_key: &str,
    fire_instant: DateTime<Utc>,
) -> Result<bool> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO routine_dispatches \
         (community_id, run_id, workflow_id, agent_pubkey, result_channel, \
          idempotency_key, fire_instant) \
         VALUES ($1,$2,$3,$4,$5,$6,$7) \
         ON CONFLICT (community_id, workflow_id, idempotency_key) DO NOTHING \
         RETURNING run_id",
    )
    .bind(community_id.as_uuid())
    .bind(run_id)
    .bind(workflow_id)
    .bind(agent_pubkey)
    .bind(result_channel)
    .bind(idempotency_key)
    .bind(fire_instant)
    .fetch_optional(pool)
    .await?;
    Ok(inserted.is_some())
}

/// Mark a dispatch as sent with its wake event ID.
#[datastore_span(name = "mark_routine_dispatched", system = "postgresql")]
pub async fn mark_routine_dispatched(
    pool: &PgPool,
    community_id: CommunityId,
    run_id: Uuid,
    wake_event_id: &[u8],
) -> Result<()> {
    sqlx::query(
        "UPDATE routine_dispatches SET wake_event_id=$1, dispatched_at=NOW() \
         WHERE community_id=$2 AND run_id=$3",
    )
    .bind(wake_event_id)
    .bind(community_id.as_uuid())
    .bind(run_id)
    .execute(pool)
    .await?;
    // Upsert routine_state.last_fired_at.
    sqlx::query(
        "INSERT INTO routine_state (community_id, workflow_id, last_fired_at) \
         SELECT rd.community_id, rd.workflow_id, NOW() \
         FROM routine_dispatches rd WHERE rd.community_id=$1 AND rd.run_id=$2 \
         ON CONFLICT (community_id, workflow_id) DO UPDATE SET last_fired_at=EXCLUDED.last_fired_at",
    )
    .bind(community_id.as_uuid())
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// True when the workflow has an open (unsettled) routine dispatch.
#[datastore_span(name = "has_open_routine_dispatch", system = "postgresql")]
pub async fn has_open_routine_dispatch(
    pool: &PgPool,
    community_id: CommunityId,
    workflow_id: Uuid,
) -> Result<bool> {
    let exists: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM routine_dispatches \
         WHERE community_id=$1 AND workflow_id=$2 AND settled_at IS NULL LIMIT 1",
    )
    .bind(community_id.as_uuid())
    .bind(workflow_id)
    .fetch_optional(pool)
    .await?;
    Ok(exists.is_some())
}

/// Fetch an open (unsettled) routine dispatch by run ID.
#[datastore_span(name = "get_open_routine_dispatch", system = "postgresql")]
pub async fn get_open_routine_dispatch(
    pool: &PgPool,
    community_id: CommunityId,
    run_id: Uuid,
) -> Result<Option<RoutineDispatchRecord>> {
    let row = sqlx::query(
        "SELECT community_id, run_id, workflow_id, agent_pubkey, result_channel, \
         idempotency_key, fire_instant, wake_event_id, dispatched_at, settled_at, outcome \
         FROM routine_dispatches \
         WHERE community_id=$1 AND run_id=$2 AND settled_at IS NULL",
    )
    .bind(community_id.as_uuid())
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| RoutineDispatchRecord {
        community_id: CommunityId::from_uuid(r.get("community_id")),
        run_id: r.get("run_id"),
        workflow_id: r.get("workflow_id"),
        agent_pubkey: r.get("agent_pubkey"),
        result_channel: r.get("result_channel"),
        idempotency_key: r.get("idempotency_key"),
        fire_instant: r.get("fire_instant"),
        wake_event_id: r.get("wake_event_id"),
        dispatched_at: r.get("dispatched_at"),
        settled_at: r.get("settled_at"),
        outcome: r.get("outcome"),
    }))
}

/// Settle a routine dispatch in a single transaction.
///
/// Returns the settlement result: strike count, notice status, and metadata
/// for posting an auto-pause notice.
#[datastore_span(name = "settle_routine_dispatch", system = "postgresql")]
pub async fn settle_routine_dispatch(
    pool: &PgPool,
    community_id: CommunityId,
    run_id: Uuid,
    outcome: &str,
    run_status: RunStatus,
    error_code: Option<&str>,
) -> Result<RoutineSettlement> {
    let mut tx = pool.begin().await?;

    // 1. Update routine_dispatches.
    sqlx::query(
        "UPDATE routine_dispatches SET settled_at=NOW(), outcome=$1 \
         WHERE community_id=$2 AND run_id=$3",
    )
    .bind(outcome)
    .bind(community_id.as_uuid())
    .bind(run_id)
    .execute(&mut *tx)
    .await?;

    // 2. Read dispatch metadata for settlement result.
    let dispatch = sqlx::query(
        "SELECT workflow_id, result_channel, dispatched_at FROM routine_dispatches \
         WHERE community_id=$1 AND run_id=$2",
    )
    .bind(community_id.as_uuid())
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await?;
    let workflow_id: Uuid = dispatch.get("workflow_id");
    let result_channel: Uuid = dispatch.get("result_channel");
    let dispatched_at: Option<DateTime<Utc>> = dispatch.get("dispatched_at");

    // 3. Read workflow name + owner for notice.
    let workflow_row = sqlx::query(
        "SELECT name, owner_pubkey FROM workflows WHERE community_id=$1 AND id=$2",
    )
    .bind(community_id.as_uuid())
    .bind(workflow_id)
    .fetch_one(&mut *tx)
    .await?;
    let workflow_name: String = workflow_row.get("name");
    let owner_pubkey: Vec<u8> = workflow_row.get("owner_pubkey");

    // 4. Update workflow_runs.
    let status_str: &str = match run_status {
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        _ => "running",
    };
    if let Some(ec) = error_code {
        sqlx::query(
            "UPDATE workflow_runs SET status=$1::run_status, completed_at=NOW(), \
             error_code=$2, error_message=$3 \
             WHERE community_id=$4 AND id=$5",
        )
        .bind(status_str)
        .bind(ec)
        .bind(outcome)
        .bind(community_id.as_uuid())
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE workflow_runs SET status=$1::run_status, completed_at=NOW() \
             WHERE community_id=$2 AND id=$3",
        )
        .bind(status_str)
        .bind(community_id.as_uuid())
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    }

    // 5. Upsert routine_state with strike counting. The initial-insert
    // branch and the ON CONFLICT branch must agree: a first-ever failure is
    // one strike, a first-ever success is zero.
    let is_failure = outcome != "succeeded";
    let row = sqlx::query(
        "INSERT INTO routine_state (community_id, workflow_id, consecutive_failures, last_outcome) \
         VALUES ($1, $2, $3::int, $4) \
         ON CONFLICT (community_id, workflow_id) DO UPDATE SET \
         consecutive_failures = CASE \
           WHEN $3::int = 1 THEN routine_state.consecutive_failures + 1 \
           ELSE 0 END, \
         last_outcome = EXCLUDED.last_outcome, \
         updated_at = NOW() \
         RETURNING consecutive_failures, paused_reason",
    )
    .bind(community_id.as_uuid())
    .bind(workflow_id)
    .bind(if is_failure { 1i32 } else { 0i32 })
    .bind(outcome)
    .fetch_one(&mut *tx)
    .await?;
    let consecutive_failures: i32 = row.get("consecutive_failures");
    let current_paused_reason: Option<String> = row.get("paused_reason");

    // 6. Check for auto-pause: 10 strikes or daily budget breach.
    // `workflows` has no paused_reason/paused_at columns (those live only on
    // routine_state) — only `status` is written here.
    let (paused_reason, notice_due) = if consecutive_failures >= 10 && current_paused_reason.is_none()
    {
        sqlx::query(
            "UPDATE workflows SET status='disabled' \
             WHERE community_id=$1 AND id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE routine_state SET paused_reason='strikes', paused_at=NOW() \
             WHERE community_id=$1 AND workflow_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        (Some("strikes".to_owned()), true)
    } else if outcome == "budget_exceeded_daily" && current_paused_reason.is_none() {
        sqlx::query(
            "UPDATE workflows SET status='disabled' \
             WHERE community_id=$1 AND id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE routine_state SET paused_reason='daily_budget', paused_at=NOW() \
             WHERE community_id=$1 AND workflow_id=$2",
        )
        .bind(community_id.as_uuid())
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        // Daily notice: at most one per UTC day (I-9).
        let today = Utc::now().date_naive();
        let notice_day: Option<NaiveDate> = sqlx::query_scalar(
            "UPDATE routine_state SET daily_notice_day = $1 \
             WHERE community_id=$2 AND workflow_id=$3 \
             AND (daily_notice_day IS NULL OR daily_notice_day <> $1) \
             RETURNING daily_notice_day",
        )
        .bind(today)
        .bind(community_id.as_uuid())
        .bind(workflow_id)
        .fetch_optional(&mut *tx)
        .await?;
        (Some("daily_budget".to_owned()), notice_day.is_some())
    } else {
        (current_paused_reason, false)
    };

    tx.commit().await?;

    Ok(RoutineSettlement {
        workflow_id,
        consecutive_failures,
        paused_reason,
        notice_due,
        result_channel,
        workflow_name,
        owner_pubkey,
        dispatched_at,
    })
}

/// Return (community_id, run_id) pairs of open dispatches older than `older_than`.
#[datastore_span(name = "expire_routine_dispatches", system = "postgresql")]
pub async fn expire_routine_dispatches(
    pool: &PgPool,
    older_than: DateTime<Utc>,
) -> Result<Vec<(CommunityId, Uuid)>> {
    let rows = sqlx::query(
        "SELECT community_id, run_id FROM routine_dispatches \
         WHERE settled_at IS NULL AND created_at < $1",
    )
    .bind(older_than)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                CommunityId::from_uuid(r.get("community_id")),
                r.get::<Uuid, _>("run_id"),
            )
        })
        .collect())
}

/// Get routine_state for a workflow.
#[datastore_span(name = "get_routine_state", system = "postgresql")]
pub async fn get_routine_state(
    pool: &PgPool,
    community_id: CommunityId,
    workflow_id: Uuid,
) -> Result<Option<RoutineStateRecord>> {
    let row = sqlx::query(
        "SELECT rs.community_id, rs.workflow_id, rs.consecutive_failures, \
         rs.last_fired_at, rs.last_outcome, rs.paused_reason, rs.paused_at, \
         rs.daily_notice_day, w.status::text AS status \
         FROM routine_state rs \
         JOIN workflows w ON w.community_id = rs.community_id AND w.id = rs.workflow_id \
         WHERE rs.community_id=$1 AND rs.workflow_id=$2",
    )
    .bind(community_id.as_uuid())
    .bind(workflow_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| RoutineStateRecord {
        community_id: CommunityId::from_uuid(r.get("community_id")),
        workflow_id: r.get("workflow_id"),
        consecutive_failures: r.get("consecutive_failures"),
        last_fired_at: r.get("last_fired_at"),
        last_outcome: r.get("last_outcome"),
        paused_reason: r.get("paused_reason"),
        paused_at: r.get("paused_at"),
        daily_notice_day: r.get("daily_notice_day"),
        status: r.get("status"),
    }))
}

/// Reset routine state on re-enable. Must set BOTH status and enabled column (G1R F-1).
#[datastore_span(name = "reset_routine_state_on_enable", system = "postgresql")]
pub async fn reset_routine_state_on_enable(
    pool: &PgPool,
    community_id: CommunityId,
    workflow_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE workflows SET status='active', enabled=TRUE \
         WHERE community_id=$1 AND id=$2",
    )
    .bind(community_id.as_uuid())
    .bind(workflow_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO routine_state (community_id, workflow_id, consecutive_failures, paused_reason, paused_at) \
         VALUES ($1, $2, 0, NULL, NULL) \
         ON CONFLICT (community_id, workflow_id) DO UPDATE SET \
         consecutive_failures=0, paused_reason=NULL, paused_at=NULL, updated_at=NOW()",
    )
    .bind(community_id.as_uuid())
    .bind(workflow_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Count enabled invoke_agent workflows for an agent (G1R F-2: count the COLUMN, not JSON).
#[datastore_span(name = "count_enabled_invoke_agent_workflows", system = "postgresql")]
pub async fn count_enabled_invoke_agent_workflows(
    pool: &PgPool,
    community_id: CommunityId,
    agent_pubkey_hex: &str,
) -> Result<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflows w \
         WHERE w.community_id=$1 AND w.status='active' AND w.enabled = TRUE \
         AND EXISTS ( \
           SELECT 1 FROM jsonb_array_elements(w.definition->'steps') s \
           WHERE s->>'action'='invoke_agent' AND s->>'agent_pubkey'=$2 \
         )",
    )
    .bind(community_id.as_uuid())
    .bind(agent_pubkey_hex)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

// -- Db impl wrappers ----------------------------------------------------------

impl Db {
    #[datastore_span(name = "insert_routine_dispatch", system = "postgresql")]
    pub async fn insert_routine_dispatch(
        &self,
        community_id: CommunityId,
        run_id: Uuid,
        workflow_id: Uuid,
        agent_pubkey: &[u8],
        result_channel: Uuid,
        idempotency_key: &str,
        fire_instant: DateTime<Utc>,
    ) -> Result<bool> {
        insert_routine_dispatch(
            &self.pool,
            community_id,
            run_id,
            workflow_id,
            agent_pubkey,
            result_channel,
            idempotency_key,
            fire_instant,
        )
        .await
    }

    #[datastore_span(name = "mark_routine_dispatched", system = "postgresql")]
    pub async fn mark_routine_dispatched(
        &self,
        community_id: CommunityId,
        run_id: Uuid,
        wake_event_id: &[u8],
    ) -> Result<()> {
        mark_routine_dispatched(&self.pool, community_id, run_id, wake_event_id).await
    }

    #[datastore_span(name = "has_open_routine_dispatch", system = "postgresql")]
    pub async fn has_open_routine_dispatch(
        &self,
        community_id: CommunityId,
        workflow_id: Uuid,
    ) -> Result<bool> {
        has_open_routine_dispatch(&self.pool, community_id, workflow_id).await
    }

    #[datastore_span(name = "get_open_routine_dispatch", system = "postgresql")]
    pub async fn get_open_routine_dispatch(
        &self,
        community_id: CommunityId,
        run_id: Uuid,
    ) -> Result<Option<RoutineDispatchRecord>> {
        get_open_routine_dispatch(&self.pool, community_id, run_id).await
    }

    #[datastore_span(name = "settle_routine_dispatch", system = "postgresql")]
    pub async fn settle_routine_dispatch(
        &self,
        community_id: CommunityId,
        run_id: Uuid,
        outcome: &str,
        run_status: RunStatus,
        error_code: Option<&str>,
    ) -> Result<RoutineSettlement> {
        settle_routine_dispatch(
            &self.pool,
            community_id,
            run_id,
            outcome,
            run_status,
            error_code,
        )
        .await
    }

    #[datastore_span(name = "expire_routine_dispatches", system = "postgresql")]
    pub async fn expire_routine_dispatches(
        &self,
        older_than: DateTime<Utc>,
    ) -> Result<Vec<(CommunityId, Uuid)>> {
        expire_routine_dispatches(&self.pool, older_than).await
    }

    #[datastore_span(name = "get_routine_state", system = "postgresql")]
    pub async fn get_routine_state(
        &self,
        community_id: CommunityId,
        workflow_id: Uuid,
    ) -> Result<Option<RoutineStateRecord>> {
        get_routine_state(&self.pool, community_id, workflow_id).await
    }

    #[datastore_span(name = "reset_routine_state_on_enable", system = "postgresql")]
    pub async fn reset_routine_state_on_enable(
        &self,
        community_id: CommunityId,
        workflow_id: Uuid,
    ) -> Result<()> {
        reset_routine_state_on_enable(&self.pool, community_id, workflow_id).await
    }

    #[datastore_span(name = "count_enabled_invoke_agent_workflows", system = "postgresql")]
    pub async fn count_enabled_invoke_agent_workflows(
        &self,
        community_id: CommunityId,
        agent_pubkey_hex: &str,
    ) -> Result<i64> {
        count_enabled_invoke_agent_workflows(&self.pool, community_id, agent_pubkey_hex).await
    }
}