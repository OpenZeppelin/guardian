-- Fail fast rather than queue every delta_proposals query behind this ALTER.
SET LOCAL lock_timeout = '5s';

ALTER TABLE delta_proposals DROP COLUMN IF EXISTS request_bytes;
DROP TABLE IF EXISTS execution_outcomes;
DROP TABLE IF EXISTS execution_submissions;
DROP TABLE IF EXISTS execution_reservations;
