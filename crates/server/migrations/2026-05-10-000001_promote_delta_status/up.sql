-- Promote delta status to typed indexed columns for fast dashboard reads.
-- Spec: feature `005-operator-dashboard-metrics`, Decision 1 (revised).
--
-- This migration:
--   1. Adds `status_kind text not null` and `status_timestamp timestamptz
--      not null` to `deltas` and `delta_proposals`.
--   2. Backfills the new columns from the existing `status` Jsonb column,
--      with safe fallbacks for malformed historical data. Malformed
--      timestamps (non-empty but unparseable) are caught by a regex
--      guard before the cast so the migration does not abort on bad
--      data, and fall back to the **epoch sentinel** (`'epoch'`,
--      `1970-01-01T00:00:00Z`) rather than `now()`. Using the epoch
--      means corrupt rows land at the *back* of the global
--      `status_timestamp DESC` feed and never masquerade as the most
--      recent activity, which would skew `latest_activity` on
--      `/dashboard/info` and reorder the global delta/proposal feeds.
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
-- Strings that fail the guard fall through to the **epoch sentinel**
-- (`'epoch'`, `1970-01-01T00:00:00Z`) instead of aborting the
-- migration. The sentinel keeps corrupt rows at the back of the
-- global `status_timestamp DESC` feed and out of `latest_activity`
-- on `/dashboard/info`. Using `now()` here would silently float
-- malformed data to the top of the feed — undesirable.
UPDATE deltas
SET
    status_kind = COALESCE(NULLIF(status->>'status', ''), 'candidate'),
    status_timestamp = COALESCE(
        CASE
            WHEN status->>'timestamp' ~ '^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}'
                THEN (status->>'timestamp')::timestamptz
            ELSE NULL
        END,
        'epoch'::timestamptz
    );

-- After backfill, every row has a value.
ALTER TABLE deltas
    ALTER COLUMN status_kind SET NOT NULL,
    ALTER COLUMN status_timestamp SET NOT NULL;

-- Defensive constraint: status_kind on `deltas` is one of
-- `candidate`/`canonical`/`discarded` only. `pending` is the lifecycle
-- state of an in-flight multisig proposal and lives on
-- `delta_proposals`; a `pending` row in `deltas` would mean a delta
-- was committed without going through the candidate/canonical path,
-- which is a data bug we want caught at write time. The
-- `delta_proposals` CHECK below covers all four variants because
-- proposals can transition through candidate/canonical/discarded
-- before being moved out of the proposal queue.
ALTER TABLE deltas
    ADD CONSTRAINT deltas_status_kind_valid CHECK (
        status_kind IN ('candidate', 'canonical', 'discarded')
    );

CREATE INDEX idx_deltas_status_kind_status_timestamp
    ON deltas (status_kind, status_timestamp DESC, account_id, nonce);

-- ---------------------------------------------------------------------------
-- delta_proposals
-- ---------------------------------------------------------------------------

ALTER TABLE delta_proposals
    ADD COLUMN status_kind text,
    ADD COLUMN status_timestamp timestamptz;

-- Same regex guard + epoch sentinel as on `deltas` above — see the
-- comment there for the rationale.
UPDATE delta_proposals
SET
    status_kind = COALESCE(NULLIF(status->>'status', ''), 'pending'),
    status_timestamp = COALESCE(
        CASE
            WHEN status->>'timestamp' ~ '^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}'
                THEN (status->>'timestamp')::timestamptz
            ELSE NULL
        END,
        'epoch'::timestamptz
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
