ALTER TABLE account_metadata
    DROP COLUMN IF EXISTS network_config,
    ALTER COLUMN account_id TYPE VARCHAR(64);

ALTER TABLE delta_proposals
    ALTER COLUMN account_id TYPE VARCHAR(64);

ALTER TABLE deltas
    ALTER COLUMN account_id TYPE VARCHAR(64);

ALTER TABLE states
    ALTER COLUMN account_id TYPE VARCHAR(64);
