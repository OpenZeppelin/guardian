-- A version per account_metadata row, raised by the database on every update,
-- so a replica can confirm a cached copy of the row is current with a one-column
-- read (issue #317). A trigger rather than application code: every write path,
-- including replicas still running the previous version during a rolling
-- deploy, bumps it. Versions come from one sequence, so an account whose row
-- is deleted and registered again gets a version above any copy still cached.
SET LOCAL lock_timeout = '5s';

CREATE SEQUENCE account_metadata_auth_version_seq AS BIGINT MINVALUE 1;

ALTER TABLE account_metadata ADD COLUMN auth_version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE account_metadata
    ALTER COLUMN auth_version SET DEFAULT nextval('account_metadata_auth_version_seq');
ALTER SEQUENCE account_metadata_auth_version_seq OWNED BY account_metadata.auth_version;

CREATE FUNCTION account_metadata_bump_auth_version() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.auth_version := nextval('account_metadata_auth_version_seq');
    RETURN NEW;
END;
$$;

CREATE TRIGGER account_metadata_bump_auth_version
BEFORE UPDATE ON account_metadata
FOR EACH ROW EXECUTE FUNCTION account_metadata_bump_auth_version();
