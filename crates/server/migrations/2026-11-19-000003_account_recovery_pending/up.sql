CREATE TABLE account_recovery_pending (
    account_id VARCHAR(128) PRIMARY KEY,
    outcome TEXT NOT NULL CHECK (outcome IN ('deferred', 'unresolved')),
    reason TEXT NOT NULL,
    stored_commitment VARCHAR(128) NOT NULL,
    canonical_nonce BIGINT,
    canonical_commitment VARCHAR(128),
    history_starts_at_nonce BIGINT,
    history_base_commitment VARCHAR(128),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
