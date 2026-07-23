-- Reverse of 2026-07-23-000001_retained_delta_status.
-- Retained rows are best-effort recovery artifacts, never settled
-- history, so dropping them (the pre-#345 delete behavior) is the
-- correct rollback — the tightened CHECK could not be restored while
-- any remain.
DELETE FROM deltas WHERE status_kind = 'retained';
ALTER TABLE deltas
    DROP CONSTRAINT deltas_status_kind_valid;
ALTER TABLE deltas
    ADD CONSTRAINT deltas_status_kind_valid CHECK (
        status_kind IN ('candidate', 'canonical', 'discarded')
    );
