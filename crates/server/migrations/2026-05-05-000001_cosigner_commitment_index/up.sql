-- GIN index over the Miden cosigner commitment arrays embedded in the
-- account_metadata.auth JSONB column. Used by the /state/lookup endpoint
-- to resolve a key commitment to the account IDs whose authorization set
-- contains it.
--
-- COALESCE handles both Miden auth variants under one index. EVM rows store
-- their signers under auth.EvmEcdsa.signers (not cosigner_commitments) and so
-- coalesce to the empty JSON array — they contribute zero index entries and
-- never match a Miden-key lookup, which is the expected behavior for this
-- feature.
--
-- jsonb_path_ops is the smaller, faster GIN operator class for the @>
-- containment query the lookup uses; it does not support the broader operator
-- set, but lookup needs only @>.
--
-- CREATE INDEX (not CONCURRENTLY) is acceptable at current account_metadata
-- scale; revisit and switch to CONCURRENTLY in a follow-up migration if the
-- table grows to a point where the brief lock during deployment becomes
-- material.

CREATE INDEX idx_account_metadata_cosigner_commitments
ON account_metadata
USING GIN (
    COALESCE(
        auth -> 'MidenFalconRpo' -> 'cosigner_commitments',
        auth -> 'MidenEcdsa'     -> 'cosigner_commitments',
        '[]'::jsonb
    )
    jsonb_path_ops
);
