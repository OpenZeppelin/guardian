DROP INDEX IF EXISTS idx_account_metadata_released;
ALTER TABLE account_metadata DROP COLUMN IF EXISTS released_at;
