-- Shared operator/EVM session store for horizontal scaling (issue #242).
-- Sessions move out of per-process memory so a session issued on one replica
-- is honored on every replica. Keyed by the SHA-256 digest of the session
-- token (the plaintext token is never stored). The primary key is composite on
-- (realm, token_digest) so operator and EVM sessions share one table with the
-- realm boundary enforced by the database, not merely by token randomness.

CREATE TABLE auth_sessions (
    realm        TEXT        NOT NULL,
    token_digest BYTEA       NOT NULL,
    subject      JSONB       NOT NULL,
    issued_at    TIMESTAMPTZ NOT NULL,
    expires_at   TIMESTAMPTZ NOT NULL,
    -- Set on logout; the row is kept until natural expiry so the revocation
    -- is honored fleet-wide for as long as the token would have been valid.
    revoked_at   TIMESTAMPTZ NULL,
    PRIMARY KEY (realm, token_digest)
);

CREATE INDEX auth_sessions_expires_idx ON auth_sessions (expires_at);
CREATE INDEX auth_sessions_realm_expires_idx ON auth_sessions (realm, expires_at);
