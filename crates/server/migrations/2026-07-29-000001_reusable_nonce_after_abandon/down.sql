-- Restoring the total constraint requires that no nonce carries more than one
-- delta, so collapse any history the partial index allowed to accumulate:
-- discarded rows at a nonce that also has a live delta are removed.
DELETE FROM deltas d
    USING deltas live
    WHERE d.status_kind = 'discarded'
      AND live.account_id = d.account_id
      AND live.nonce = d.nonce
      AND live.status_kind <> 'discarded';

DELETE FROM deltas d
    USING deltas other
    WHERE d.status_kind = 'discarded'
      AND other.status_kind = 'discarded'
      AND other.account_id = d.account_id
      AND other.nonce = d.nonce
      AND other.id < d.id;

DROP INDEX IF EXISTS deltas_live_account_nonce;

ALTER TABLE deltas ADD CONSTRAINT deltas_account_id_nonce_key UNIQUE (account_id, nonce);
