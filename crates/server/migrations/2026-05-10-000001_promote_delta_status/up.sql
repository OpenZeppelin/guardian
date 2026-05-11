-- Promote delta status to typed indexed columns for fast dashboard reads.
-- Spec: feature `005-operator-dashboard-metrics`, Decision 1 (revised).
--
-- This migration:
--   1. Adds `status_kind text not null` and `status_timestamp timestamptz
--      not null` to `deltas` and `delta_proposals`.
--   2. Backfills the new columns from the existing `status` Jsonb column,
--      with safe fallbacks for malformed historical data. Malformed
--      timestamps (non-empty but unparseable) are also caught by a regex
--      guard before the cast so the migration does not abort on bad data.
--   3. Adds composite indexes optimized for the global-feed sort key
--      `(status_kind, status_timestamp DESC, account_id, nonce[, commitment])`
--      and the per-account sort key `(account_id, nonce DESC[, commitment DESC])`
--      so cursor traversal lands on covering indexes.
--
-- The `status` Jsonb column is retained. Writes dual-populate both during
-- the transition; a future migration can drop the Jsonb column once
-- nothing reads from it.

-- ---------------------------------------------------------------------------
-- deltas
-- ---------------------------------------------------------------------------

-- Add columns nullable first to support backfill, then tighten to NOT NULL.
ALTER TABLE deltas
    ADD COLUMN status_kind text,
    ADD COLUMN status_timestamp timestamptz;

-- The status_timestamp cast is guarded by a regex that matches the
-- ISO-8601 prefix every well-formed write emits (`YYYY-MM-DDTHH:MM:SS`).
-- Strings that fail the guard fall through to `now()` instead of
-- aborting the migration, matching the "safe fallback for malformed
-- historical data" intent in the header comment.
UPDATE deltas
SET
    status_kind = COALESCE(NULLIF(status->>'status', ''), 'candidate'),
    status_timestamp = COALESCE(
        CASE
            WHEN status->>'timestamp' ~ '^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}'
                THEN (status->>'timestamp')::timestamptz
            ELSE NULL
        END,
        now()
    );

-- After backfill, every row has a value.
ALTER TABLE deltas
    ALTER COLUMN status_kind SET NOT NULL,
    ALTER COLUMN status_timestamp SET NOT NULL;

-- Defensive constraint: status_kind must be one of the four lifecycle
-- variants. Anything else is a data bug and we want it caught at write
-- time rather than silently surfaced on the dashboard.
ALTER TABLE deltas
    ADD CONSTRAINT deltas_status_kind_valid CHECK (
        status_kind IN ('pending', 'candidate', 'canonical', 'discarded')
    );

CREATE INDEX idx_deltas_status_kind_status_timestamp
    ON deltas (status_kind, status_timestamp DESC, account_id, nonce);

-- ---------------------------------------------------------------------------
-- delta_proposals
-- ---------------------------------------------------------------------------

ALTER TABLE delta_proposals
    ADD COLUMN status_kind text,
    ADD COLUMN status_timestamp timestamptz;

-- Same regex guard as on `deltas` above — see the comment there.
UPDATE delta_proposals
SET
    status_kind = COALESCE(NULLIF(status->>'status', ''), 'pending'),
    status_timestamp = COALESCE(
        CASE
            WHEN status->>'timestamp' ~ '^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}'
                THEN (status->>'timestamp')::timestamptz
            ELSE NULL
        END,
        now()
    );

ALTER TABLE delta_proposals
    ALTER COLUMN status_kind SET NOT NULL,
    ALTER COLUMN status_timestamp SET NOT NULL;

ALTER TABLE delta_proposals
    ADD CONSTRAINT delta_proposals_status_kind_valid CHECK (
        status_kind IN ('pending', 'candidate', 'canonical', 'discarded')
    );

CREATE INDEX idx_delta_proposals_status_kind_status_timestamp
    ON delta_proposals (status_kind, status_timestamp DESC, account_id, nonce, commitment);

CREATE INDEX idx_delta_proposals_account_nonce_commitment
    ON delta_proposals (account_id, nonce DESC, commitment DESC);
