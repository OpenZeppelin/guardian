DROP INDEX IF EXISTS idx_account_metadata_paused;
ALTER TABLE account_metadata DROP COLUMN IF EXISTS paused_reason;
ALTER TABLE account_metadata DROP COLUMN IF EXISTS paused_at;
