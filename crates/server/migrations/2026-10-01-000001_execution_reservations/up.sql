-- Durable per-account execution reservations for Guardian-executed proposals
-- (issue #254). A reservation spans acceptance through a terminal state and is
-- fenced by the account-scoped `execution:{account_id}` lease in
-- `worker_leases`. The submission evidence row is the no-retry boundary: once
-- it exists the transaction is never proved or sent again, only reconciled.

CREATE TABLE execution_reservations (
    id                  BIGSERIAL   PRIMARY KEY,
    account_id          TEXT        NOT NULL,
    proposal_id         TEXT        NOT NULL,
    attempt             INTEGER     NOT NULL CHECK (attempt >= 1),
    holder_id           TEXT        NOT NULL,
    lease_name          TEXT        NOT NULL,
    fence_token         BIGINT      NOT NULL,
    lease_expires_at    TIMESTAMPTZ NOT NULL,
    phase               TEXT        NOT NULL CHECK (phase IN (
                            'accepted', 'verified', 'acknowledged', 'executed', 'proving',
                            'proved', 'submission_committed', 'sent', 'reconciling')),
    ignored_signatures  INTEGER     NOT NULL DEFAULT 0 CHECK (ignored_signatures >= 0),
    released_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt)
);

-- At most one active reservation per account, enforced by the schema so the
-- single-owner rule survives a bypassed or refactored service-layer check.
CREATE UNIQUE INDEX execution_reservations_one_active_per_account
    ON execution_reservations (account_id)
    WHERE released_at IS NULL;

CREATE TABLE execution_submissions (
    id                   BIGSERIAL   PRIMARY KEY,
    account_id           TEXT        NOT NULL,
    proposal_id          TEXT        NOT NULL,
    attempt              INTEGER     NOT NULL,
    candidate_nonce      BIGINT      NOT NULL CHECK (candidate_nonce >= 0),
    transaction_id       TEXT        NOT NULL,
    expected_commitment  TEXT        NOT NULL,
    reference_block      BIGINT      NOT NULL,
    expiration_block     BIGINT      NOT NULL,
    base_commitment      TEXT        NOT NULL,
    committed_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt),
    FOREIGN KEY (account_id, proposal_id, attempt)
        REFERENCES execution_reservations (account_id, proposal_id, attempt)
);

-- Canonicalization looks up whether a candidate belongs to a live execution
-- before discarding it: the execution may sit between its boundary commit and
-- its send.
CREATE INDEX execution_submissions_account_nonce
    ON execution_submissions (account_id, candidate_nonce);

CREATE TABLE execution_outcomes (
    id             BIGSERIAL   PRIMARY KEY,
    account_id     TEXT        NOT NULL,
    proposal_id    TEXT        NOT NULL,
    attempt        INTEGER     NOT NULL,
    state          TEXT        NOT NULL CHECK (state IN ('committed', 'failed')),
    error_code     TEXT,
    error_message  TEXT,
    error_meta     JSONB,
    resolved_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt),
    FOREIGN KEY (account_id, proposal_id, attempt)
        REFERENCES execution_reservations (account_id, proposal_id, attempt),
    CHECK (
        (state = 'committed'
            AND error_code IS NULL AND error_message IS NULL AND error_meta IS NULL)
        OR (state = 'failed' AND error_code IS NOT NULL AND error_message IS NOT NULL)
    )
);

-- The retention sweep walks finished attempts oldest first from here; the
-- attempt-key unique indexes answer its per-attempt lookups.
CREATE INDEX execution_outcomes_resolved_at
    ON execution_outcomes (resolved_at);

-- Fail fast rather than queue every delta_proposals query behind this ALTER while a long
-- transaction on a replica still running the previous version holds the table.
SET LOCAL lock_timeout = '5s';

ALTER TABLE delta_proposals ADD COLUMN request_bytes BIGINT NOT NULL DEFAULT 0;
