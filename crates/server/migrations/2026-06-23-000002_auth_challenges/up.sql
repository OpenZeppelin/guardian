-- Shared operator/EVM login-challenge store for horizontal scaling (issue #242).
-- A challenge issued on one replica must be verifiable on another. Realm-aware
-- so the two verification models coexist: `challenge_key` is the operator
-- signing-digest hex or the EVM nonce, and `payload` carries the realm-specific
-- fields needed to match/recover at verify time. Matching runs in Rust (Falcon
-- verify / ECDSA recover); the store provides the candidates and the single-use
-- claim.

CREATE TABLE auth_challenges (
    realm         TEXT        NOT NULL,
    challenge_key TEXT        NOT NULL,
    principal     TEXT        NOT NULL,
    payload       JSONB       NOT NULL,
    issued_at     TIMESTAMPTZ NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    consumed_at   TIMESTAMPTZ NULL,
    PRIMARY KEY (realm, challenge_key)
);

CREATE INDEX auth_challenges_realm_principal_idx ON auth_challenges (realm, principal);
CREATE INDEX auth_challenges_expires_idx ON auth_challenges (expires_at);
