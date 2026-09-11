-- Validate the constraint 2026-07-23-000001 added NOT VALID. Runs in
-- its own migration (and therefore its own transaction) so the
-- validating scan only takes SHARE UPDATE EXCLUSIVE — concurrent reads
-- and writes proceed — instead of extending the ACCESS EXCLUSIVE
-- window of the constraint switch itself.
ALTER TABLE deltas VALIDATE CONSTRAINT deltas_status_kind_valid;
