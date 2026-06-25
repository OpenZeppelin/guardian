CREATE TABLE storage_encryption_marker (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE,
    scheme_version SMALLINT NOT NULL,
    init_kid TEXT NOT NULL,
    CONSTRAINT storage_encryption_marker_singleton CHECK (id),
    CONSTRAINT storage_encryption_marker_scheme_version_range CHECK (scheme_version BETWEEN 0 AND 255)
);
