-- Block concurrent CAS writes from replicas still running the pre-split
-- binary: timestamps committed between the backfill snapshot and the column
-- drop would otherwise be silently discarded, regressing replay state
-- (FR-006). The lock is held until the migration transaction commits;
-- queued legacy writes then fail on the missing column, which is the
-- intended fail-closed behavior.
--
-- Bounded wait: migrations run at server startup, and an unbounded LOCK
-- TABLE queued behind a long-running transaction would also block every
-- new reader queued after it. Failing fast lets the orchestrator restart
-- the replica and retry instead of stalling the fleet.
SET LOCAL lock_timeout = '5s';

LOCK TABLE account_metadata IN ACCESS EXCLUSIVE MODE;

CREATE TABLE account_auth_state (
    account_id VARCHAR(128) PRIMARY KEY
        REFERENCES account_metadata(account_id) ON DELETE CASCADE,
    last_auth_timestamp BIGINT NOT NULL
) WITH (fillfactor = 50);

INSERT INTO account_auth_state (account_id, last_auth_timestamp)
SELECT account_id, last_auth_timestamp
  FROM account_metadata
 WHERE last_auth_timestamp IS NOT NULL;

ALTER TABLE account_metadata DROP COLUMN last_auth_timestamp;
