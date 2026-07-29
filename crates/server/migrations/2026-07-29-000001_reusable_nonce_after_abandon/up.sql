-- A client-abandoned delta must not consume its nonce.
--
-- `UNIQUE(account_id, nonce)` treats every row at a nonce as settled, which is
-- right for a canonical delta -- that nonce really was consumed on-chain -- but
-- wrong for a discarded one. An abandoned transaction never landed, so the
-- account's on-chain nonce is unchanged and its next delta must reuse the same
-- nonce. Under the old constraint that insert collided with the discarded row
-- and was rejected as a pending-delta conflict, leaving the account unable to
-- ever submit again: every subsequent attempt targets the same blocked nonce.
--
-- The real invariant is narrower: at most one *live* delta per (account, nonce).
-- A partial unique index states exactly that, so discarded attempts accumulate
-- as history without consuming the nonce.
ALTER TABLE deltas DROP CONSTRAINT IF EXISTS deltas_account_id_nonce_key;

CREATE UNIQUE INDEX deltas_live_account_nonce
    ON deltas (account_id, nonce)
    WHERE status_kind <> 'discarded';
