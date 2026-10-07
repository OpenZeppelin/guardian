-- The migration scenario needs a second GUARDIAN to migrate to, and two
-- GUARDIANs sharing one database would share account metadata, which is exactly
-- the thing a migration moves.
CREATE DATABASE guardian_migration_target OWNER guardian;
