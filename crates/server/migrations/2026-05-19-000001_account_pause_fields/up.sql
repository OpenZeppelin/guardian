-- Feature 001-account-pausing: add operator-initiated pause fields to
-- account_metadata. Pause state is a per-account operational flag and
-- not a state/delta lifecycle change; the invariant
--   (paused_at IS NULL) <-> (paused_reason IS NULL)
-- is enforced at the trait/handler level via set_pause / clear_pause.
ALTER TABLE account_metadata ADD COLUMN paused_at TIMESTAMPTZ NULL;
ALTER TABLE account_metadata ADD COLUMN paused_reason TEXT NULL;

-- Partial index supports "list all currently-paused accounts" cheaply
-- even on a wide table. Index size scales with the count of currently-
-- paused accounts, not total accounts.
CREATE INDEX IF NOT EXISTS idx_account_metadata_paused
    ON account_metadata(paused_at)
    WHERE paused_at IS NOT NULL;
