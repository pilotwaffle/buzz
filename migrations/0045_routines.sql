SET LOCAL lock_timeout = '5s';

CREATE TABLE routine_dispatches (
    community_id     UUID NOT NULL REFERENCES communities(id),
    run_id           UUID NOT NULL,
    workflow_id      UUID NOT NULL,
    agent_pubkey     BYTEA NOT NULL,
    result_channel   UUID NOT NULL,
    idempotency_key  TEXT NOT NULL,
    fire_instant     TIMESTAMPTZ NOT NULL,
    wake_event_id    BYTEA,
    dispatched_at    TIMESTAMPTZ,
    settled_at       TIMESTAMPTZ,
    outcome          TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, run_id),
    UNIQUE (community_id, workflow_id, idempotency_key),
    FOREIGN KEY (community_id, run_id) REFERENCES workflow_runs (community_id, id) ON DELETE CASCADE,
    FOREIGN KEY (community_id, workflow_id) REFERENCES workflows (community_id, id) ON DELETE CASCADE
);
CREATE INDEX idx_routine_dispatches_open ON routine_dispatches (community_id, workflow_id) WHERE settled_at IS NULL;
CREATE INDEX idx_routine_dispatches_sweep ON routine_dispatches (created_at) WHERE settled_at IS NULL;

CREATE TABLE routine_state (
    community_id          UUID NOT NULL REFERENCES communities(id),
    workflow_id           UUID NOT NULL,
    consecutive_failures  INT NOT NULL DEFAULT 0,
    last_fired_at         TIMESTAMPTZ,
    last_outcome          TEXT,
    paused_reason         TEXT,
    paused_at             TIMESTAMPTZ,
    daily_notice_day      DATE,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, workflow_id),
    FOREIGN KEY (community_id, workflow_id) REFERENCES workflows (community_id, id) ON DELETE CASCADE
);