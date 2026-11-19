CREATE TABLE account_recovery_backups (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id VARCHAR(128) NOT NULL,
    prior_state JSONB NOT NULL,
    prior_auth JSONB NOT NULL,
    prior_proposals JSONB NOT NULL DEFAULT '[]'::jsonb,
    recovered_commitment VARCHAR(128) NOT NULL,
    recovered_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
