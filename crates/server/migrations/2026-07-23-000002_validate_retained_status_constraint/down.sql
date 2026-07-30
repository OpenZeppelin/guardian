-- No structural change to reverse: validation only flips the
-- constraint's convalidated flag, and the 000001 down migration
-- re-creates the narrowed constraint itself. SQL requires a statement,
-- so re-assert the (already guaranteed) validity harmlessly.
ALTER TABLE deltas VALIDATE CONSTRAINT deltas_status_kind_valid;
