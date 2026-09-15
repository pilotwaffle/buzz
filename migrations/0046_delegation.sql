SET LOCAL lock_timeout = '5s';

CREATE TABLE delegation_records (
    community_id               UUID NOT NULL REFERENCES communities(id),
    delegation_id              UUID NOT NULL,
    run_id                     UUID NOT NULL,
    origin_event_id            BYTEA NOT NULL,
    parent_approval_event_id   BYTEA,
    source_agent               BYTEA NOT NULL,
    target_agent               BYTEA NOT NULL,
    agent_path                 BYTEA[] NOT NULL,
    hop_budget                 SMALLINT NOT NULL,
    max_turns                  INT NOT NULL,
    cost_cap_microusd          BIGINT,
    token_budget               BIGINT NOT NULL,
    idempotency_key            TEXT NOT NULL,
    expires_at                 TIMESTAMPTZ NOT NULL,
    operator_pubkey            BYTEA NOT NULL,
    operator_approval_event_id BYTEA NOT NULL,
    approval_event_json        JSONB NOT NULL,          -- the signed 43007 envelope; identifiers only
    immutable_request_hash     BYTEA NOT NULL,
    state                      TEXT NOT NULL,           -- offered|approved|refused|delivered|failed|expired
    failure_detail             TEXT,                    -- turns|budget|cost_unknown|timeout|cancelled|refused|store_unavailable|expired
    remaining_turns            INT NOT NULL,
    token_budget_remaining     BIGINT NOT NULL,
    committed_cost_microusd    BIGINT NOT NULL DEFAULT 0,
    answer_event_id            BYTEA,
    summary_event_id           BYTEA,
    failure_notice_event_id    BYTEA,
    origin_channel_id          UUID NOT NULL,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, delegation_id),
    CONSTRAINT chk_delegation_state CHECK (state IN ('offered','approved','refused','delivered','failed','expired')),
    CONSTRAINT chk_delegation_turns CHECK (remaining_turns >= 0 AND remaining_turns <= max_turns),
    CONSTRAINT chk_delegation_budget CHECK (token_budget_remaining >= 0 AND token_budget_remaining <= token_budget)
);
CREATE INDEX idx_delegation_records_open_target ON delegation_records (community_id, target_agent) WHERE state = 'approved';
CREATE INDEX idx_delegation_records_expiry ON delegation_records (expires_at) WHERE state = 'approved';

CREATE TABLE delegation_claims (
    community_id      UUID NOT NULL REFERENCES communities(id),
    delegation_id     UUID NOT NULL,
    approval_event_id BYTEA NOT NULL,
    operator_pubkey   BYTEA NOT NULL,
    source_agent      BYTEA NOT NULL,
    idempotency_key   TEXT NOT NULL,
    immutable_request_hash BYTEA NOT NULL,
    expires_at        TIMESTAMPTZ NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, delegation_id),
    UNIQUE (community_id, approval_event_id),
    UNIQUE (community_id, operator_pubkey, source_agent, idempotency_key),
    FOREIGN KEY (community_id, delegation_id) REFERENCES delegation_records (community_id, delegation_id)
);

CREATE TABLE delegation_actions (
    community_id            UUID NOT NULL REFERENCES communities(id),
    delegation_id           UUID NOT NULL,
    action_seq              INT NOT NULL,                -- 1..max_turns
    approval_event_id       BYTEA NOT NULL,
    immutable_request_hash  BYTEA NOT NULL,
    remaining_turns_before  INT NOT NULL,
    committed_cost_before   BIGINT NOT NULL,
    cost_reservation        BIGINT,
    token_budget_at_dispatch BIGINT NOT NULL,
    owner_snapshot          JSONB NOT NULL,              -- [{agent_pubkey, owner_pubkey, ownership_revision}]
    child_answer_event_id   BYTEA,
    wake_event_id           BYTEA,
    dispatched_at           TIMESTAMPTZ,
    settled_at              TIMESTAMPTZ,
    outcome                 TEXT,                        -- delivered|delegated|failed|budget_exceeded|timeout|cancelled
    tokens_used             BIGINT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, delegation_id, action_seq),
    FOREIGN KEY (community_id, delegation_id) REFERENCES delegation_records (community_id, delegation_id)
);
CREATE INDEX idx_delegation_actions_open ON delegation_actions (community_id, delegation_id) WHERE settled_at IS NULL;
CREATE INDEX idx_delegation_actions_sweep ON delegation_actions (created_at) WHERE settled_at IS NULL;
