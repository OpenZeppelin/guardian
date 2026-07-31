ALTER TABLE account_metadata ADD COLUMN last_auth_timestamp BIGINT;

UPDATE account_metadata
   SET last_auth_timestamp = account_auth_state.last_auth_timestamp
  FROM account_auth_state
 WHERE account_metadata.account_id = account_auth_state.account_id;

DROP TABLE account_auth_state;
