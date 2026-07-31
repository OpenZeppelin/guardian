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
