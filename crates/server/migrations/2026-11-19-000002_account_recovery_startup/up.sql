CREATE TABLE account_recovery_startup (
    network_config JSONB PRIMARY KEY,
    resume_after VARCHAR(128) NOT NULL DEFAULT '',
    completed_at TIMESTAMPTZ,
    unresolved_accounts BIGINT,
    pass_unresolved BIGINT NOT NULL DEFAULT 0,
    pass_deferred BOOLEAN NOT NULL DEFAULT FALSE
);
