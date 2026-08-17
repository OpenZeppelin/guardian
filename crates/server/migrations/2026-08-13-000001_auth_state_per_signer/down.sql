-- Collapse per-(account, signer) replay state back to one per-account row.
-- MAX over the account's signers is the conservative account-scoped floor:
-- no request accepted under per-signer scope wins the account-scoped CAS
-- after the rollback. Same locking rationale as up.sql: post-#367 CAS
-- writers are blocked so their timestamps land in the aggregate; once the
-- rebuild commits, queued per-signer writes fail on the missing column,
-- which is the intended fail-closed behavior.
SET LOCAL lock_timeout = '5s';

LOCK TABLE account_auth_state IN ACCESS EXCLUSIVE MODE;

CREATE TABLE account_auth_state_collapsed AS
SELECT account_id, MAX(last_auth_timestamp) AS last_auth_timestamp
  FROM account_auth_state
 GROUP BY account_id;

DROP TABLE account_auth_state;

CREATE TABLE account_auth_state (
    account_id VARCHAR(128) PRIMARY KEY
        REFERENCES account_metadata(account_id) ON DELETE CASCADE,
    last_auth_timestamp BIGINT NOT NULL
) WITH (fillfactor = 50);

INSERT INTO account_auth_state (account_id, last_auth_timestamp)
SELECT account_id, last_auth_timestamp FROM account_auth_state_collapsed;

DROP TABLE account_auth_state_collapsed;
