-- Account metadata table
CREATE TABLE IF NOT EXISTS account_metadata (
    account_id VARCHAR(64) PRIMARY KEY,
    auth JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    has_pending_candidate BOOLEAN NOT NULL DEFAULT FALSE,
    last_auth_timestamp BIGINT
);

CREATE INDEX IF NOT EXISTS idx_metadata_pending
    ON account_metadata(has_pending_candidate)
    WHERE has_pending_candidate = TRUE;
