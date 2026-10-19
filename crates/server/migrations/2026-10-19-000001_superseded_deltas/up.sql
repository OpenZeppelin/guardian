-- Archive for deltas superseded by a resubmission at their nonce.
--
-- `deltas` holds one row per (account_id, nonce). A retained or
-- client-abandoned row is kept so the reconcile pass can recover its
-- transaction if it lands late, but a new candidate at the same nonce (the
-- resubmission the abandon endpoint exists to enable) used to delete that
-- row to make room. If the superseded transaction landed afterwards, no row
-- linked the stored state to the chain any more and the account stayed
-- behind the chain for good.
--
-- `submit_candidate` now moves the superseded row here, in the same
-- transaction, instead of deleting it. The reconcile pass treats archived
-- rows as recoverable while `superseded_at` is within the retention TTL:
-- if the chain shows the archived transaction landed, it is promoted back
-- into `deltas` as canonical, replacing the non-canonical row that took
-- its nonce. Older rows are pruned by the reconcile pass.
--
-- The columns mirror `deltas` (payloads stay as stored, encrypted when
-- storage encryption is on). There is no (account_id, nonce) uniqueness:
-- one nonce can be superseded more than once.
CREATE TABLE superseded_deltas (
    id BIGSERIAL PRIMARY KEY,
    account_id VARCHAR(128) NOT NULL
        REFERENCES account_metadata(account_id) ON DELETE CASCADE,
    nonce BIGINT NOT NULL,
    prev_commitment VARCHAR(128) NOT NULL,
    new_commitment VARCHAR(128),
    delta_payload JSONB NOT NULL,
    ack_sig TEXT,
    status JSONB NOT NULL,
    status_kind TEXT NOT NULL,
    status_timestamp TIMESTAMPTZ NOT NULL,
    metadata JSONB,
    superseded_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_superseded_deltas_account_superseded_at
    ON superseded_deltas (account_id, superseded_at);
CREATE INDEX idx_superseded_deltas_superseded_at
    ON superseded_deltas (superseded_at);
