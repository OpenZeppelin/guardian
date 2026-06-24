-- Shared operator/EVM session store for horizontal scaling (issue #242).
-- Sessions move out of per-process memory so a session issued on one replica
-- is honored on every replica. Keyed by the SHA-256 digest of the session
-- token (the plaintext token is never stored). Realm-scoped so operator and
-- EVM sessions share one table without collision.

CREATE TABLE auth_sessions (
    token_digest BYTEA       PRIMARY KEY,
    realm        TEXT        NOT NULL,
    subject      JSONB       NOT NULL,
    issued_at    TIMESTAMPTZ NOT NULL,
    expires_at   TIMESTAMPTZ NOT NULL,
    -- Set on logout; the row is kept until natural expiry so the revocation
    -- is honored fleet-wide for as long as the token would have been valid.
    revoked_at   TIMESTAMPTZ NULL
);

CREATE INDEX auth_sessions_expires_idx ON auth_sessions (expires_at);
CREATE INDEX auth_sessions_realm_expires_idx ON auth_sessions (realm, expires_at);
