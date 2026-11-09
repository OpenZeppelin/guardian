DROP TRIGGER account_metadata_bump_auth_version ON account_metadata;
DROP FUNCTION account_metadata_bump_auth_version();
ALTER TABLE account_metadata DROP COLUMN auth_version;
