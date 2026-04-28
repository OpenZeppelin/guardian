ALTER TABLE states
    ALTER COLUMN account_id TYPE VARCHAR(128);

ALTER TABLE deltas
    ALTER COLUMN account_id TYPE VARCHAR(128);

ALTER TABLE delta_proposals
    ALTER COLUMN account_id TYPE VARCHAR(128);

ALTER TABLE account_metadata
    ALTER COLUMN account_id TYPE VARCHAR(128),
    ADD COLUMN IF NOT EXISTS network_config JSONB NOT NULL DEFAULT '{"kind":"miden","network_type":"devnet"}'::jsonb;
