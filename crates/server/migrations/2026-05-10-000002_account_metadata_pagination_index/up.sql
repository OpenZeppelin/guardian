-- Composite index supporting the dashboard account list's paginated
-- read with the documented sort key `(updated_at DESC, account_id ASC)`.
-- Spec: feature `005-operator-dashboard-metrics` FR-001..FR-008
-- (`/dashboard/accounts`).
--
-- The index lets the Postgres backend serve `list_paged` as an index
-- range scan, so dashboard pagination scales beyond the in-memory
-- limit that bounded the v1 implementation.

CREATE INDEX idx_account_metadata_updated_at_account_id
    ON account_metadata (updated_at DESC, account_id ASC);
