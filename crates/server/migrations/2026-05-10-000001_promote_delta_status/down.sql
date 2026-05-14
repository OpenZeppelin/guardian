-- Reverse of 2026-05-10-000001_promote_delta_status.
-- Drops the typed status columns + indexes. The `status` Jsonb column
-- and existing data remain intact; rolling back is safe because nothing
-- depended on the typed columns at the contract level — they were a
-- read-side optimization with dual-write maintaining the source of
-- truth in the Jsonb column.

DROP INDEX IF EXISTS idx_delta_proposals_account_nonce_commitment;
DROP INDEX IF EXISTS idx_delta_proposals_status_kind_status_timestamp;
ALTER TABLE delta_proposals
    DROP CONSTRAINT IF EXISTS delta_proposals_status_kind_valid;
ALTER TABLE delta_proposals
    DROP COLUMN IF EXISTS status_timestamp,
    DROP COLUMN IF EXISTS status_kind;

DROP INDEX IF EXISTS idx_deltas_status_kind_status_timestamp;
ALTER TABLE deltas
    DROP CONSTRAINT IF EXISTS deltas_status_kind_valid;
ALTER TABLE deltas
    DROP COLUMN IF EXISTS status_timestamp,
    DROP COLUMN IF EXISTS status_kind;
