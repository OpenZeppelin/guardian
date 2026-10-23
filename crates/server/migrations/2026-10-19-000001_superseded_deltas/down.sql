-- Reverse of 2026-10-19-000001_superseded_deltas. Archived rows are only a
-- recovery aid within the retention TTL; dropping them restores the
-- previous behavior, where a superseded row was deleted.
DROP TABLE IF EXISTS superseded_deltas;
