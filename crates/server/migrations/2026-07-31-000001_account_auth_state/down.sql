-- Mirror image of the up.sql hazard: replicas already running the post-split
-- binary CAS-write into account_auth_state, and the backfill UPDATE below
-- reads that table without locking it (the ALTER only locks
-- account_metadata). A timestamp committed between the backfill snapshot and
-- DROP TABLE would be silently discarded, regressing replay state (FR-006).
-- Locking account_auth_state up front blocks those writers; once the drop
-- commits, queued post-split writes fail on the missing table, which is the
-- intended fail-closed behavior. Same bounded wait rationale as up.sql.
SET LOCAL lock_timeout = '5s';

LOCK TABLE account_auth_state IN ACCESS EXCLUSIVE MODE;

ALTER TABLE account_metadata ADD COLUMN last_auth_timestamp BIGINT;

UPDATE account_metadata
   SET last_auth_timestamp = account_auth_state.last_auth_timestamp
  FROM account_auth_state
 WHERE account_metadata.account_id = account_auth_state.account_id;

DROP TABLE account_auth_state;
