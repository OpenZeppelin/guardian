-- Issue #305: release-on-guardian-switch. `released_at` marks the UTC
-- instant this server detected (via a canonicalized SwitchGuardian
-- delta) that it is no longer the account's guardian. NULL while this
-- server is the guardian. Terminal until the account re-onboards via
-- /configure; owned by set_released / clear_released at the trait
-- level — generic metadata writes never change it. Orthogonal to
-- paused_at so an operator unpause cannot resurrect a released account.
ALTER TABLE account_metadata ADD COLUMN released_at TIMESTAMPTZ NULL;

-- Partial index supports "list all released accounts" cheaply; size
-- scales with the count of released accounts, not total accounts.
CREATE INDEX IF NOT EXISTS idx_account_metadata_released
    ON account_metadata(released_at)
    WHERE released_at IS NOT NULL;
