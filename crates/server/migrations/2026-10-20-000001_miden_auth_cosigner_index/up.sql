-- GIN index over the cosigners of the per-signer-scheme Miden auth variant
-- (`auth.Miden.cosigners`, an array of `{ "commitment", "scheme" }` objects),
-- for the /state/lookup and session-grant cosigner lookups (issue #563).
--
-- A separate index rather than a third arm in the COALESCE of
-- idx_account_metadata_cosigner_commitments: that index's expression stays
-- exactly what replicas still running the previous version query, so their
-- lookups keep using it during a rolling deploy. Rows of the single-scheme
-- variants have no `auth.Miden` key and contribute no entries here.
--
-- jsonb_path_ops serves the `@> '[{"commitment": ...}]'` containment the lookup
-- uses. CREATE INDEX (not CONCURRENTLY) is acceptable at current
-- account_metadata scale, as for the existing lookup index.
SET LOCAL lock_timeout = '5s';

CREATE INDEX idx_account_metadata_miden_cosigners
ON account_metadata
USING GIN ((auth -> 'Miden' -> 'cosigners') jsonb_path_ops);
