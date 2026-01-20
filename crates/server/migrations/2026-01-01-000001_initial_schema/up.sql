-- Initial schema for Private State Manager Postgres backend

-- Account states table
CREATE TABLE states (
    account_id VARCHAR(64) PRIMARY KEY,
    state_json JSONB NOT NULL,
    commitment VARCHAR(128) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

-- Deltas table (account state changes)
CREATE TABLE deltas (
    id BIGSERIAL PRIMARY KEY,
    account_id VARCHAR(64) NOT NULL,
    nonce BIGINT NOT NULL,
    prev_commitment VARCHAR(128) NOT NULL,
    new_commitment VARCHAR(128),
    delta_payload JSONB NOT NULL,
    ack_sig TEXT,
    status JSONB NOT NULL,
    UNIQUE(account_id, nonce)
);

CREATE INDEX idx_deltas_account_id ON deltas(account_id);
CREATE INDEX idx_deltas_account_nonce ON deltas(account_id, nonce);

-- Delta proposals table (pending multi-party coordination)
CREATE TABLE delta_proposals (
    id BIGSERIAL PRIMARY KEY,
    account_id VARCHAR(64) NOT NULL,
    commitment VARCHAR(128) NOT NULL,
    nonce BIGINT NOT NULL,
    prev_commitment VARCHAR(128) NOT NULL,
    new_commitment VARCHAR(128),
    delta_payload JSONB NOT NULL,
    ack_sig TEXT,
    status JSONB NOT NULL,
    UNIQUE(account_id, commitment)
);

CREATE INDEX idx_proposals_account_id ON delta_proposals(account_id);
CREATE INDEX idx_proposals_account_commitment ON delta_proposals(account_id, commitment);

-- Account metadata table
CREATE TABLE account_metadata (
    account_id VARCHAR(64) PRIMARY KEY,
    auth JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    has_pending_candidate BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX idx_metadata_pending ON account_metadata(has_pending_candidate) WHERE has_pending_candidate = TRUE;
