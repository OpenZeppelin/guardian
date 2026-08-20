-- Re-key replay-protection state from per-account to per-(account, signer)
-- (issue #367) so independent authorized cosigners never contend on one
-- timestamp. Each existing account-scoped row is expanded to one row per
-- currently authorized signer commitment, preserving the replay floor across
-- the upgrade instead of re-accepting requests seen just before it. A reserved
-- account-level row is also retained so a signer absent during migration still
-- inherits the old floor if it is authorized again later.
--
-- The exclusive lock blocks CAS writers still running the pre-#367 binary:
-- a timestamp committed between the expansion snapshot and the table swap
-- would otherwise be silently discarded, regressing replay state. The share
-- lock holds the authorized signer sets still, so the metadata validated
-- below is the metadata that is expanded; readers are unaffected.
-- Once this commits, queued pre-#367 writes fail against the replacement
-- schema because signer_commitment is required, which is the intended
-- fail-closed behavior.
-- Same bounded-wait rationale as 2026-07-31-000001_account_auth_state.
SET LOCAL lock_timeout = '5s';

LOCK TABLE account_metadata IN SHARE MODE;

LOCK TABLE account_auth_state IN ACCESS EXCLUSIVE MODE;

CREATE TABLE account_auth_state_per_signer (
    account_id VARCHAR(128) NOT NULL
        REFERENCES account_metadata(account_id) ON DELETE CASCADE,
    signer_commitment VARCHAR(128) NOT NULL,
    last_auth_timestamp BIGINT NOT NULL,
    PRIMARY KEY (account_id, signer_commitment)
) WITH (fillfactor = 50);

-- Expand every account-scoped replay row against its authorized signer set
-- exactly once, so the format check below and the copy that follows cannot
-- read different metadata or disagree on which JSON paths hold commitments.
-- Reading signer entries as text also coerces numbers and objects, which would
-- create rows under keys no real signer can ever present. Only canonical
-- commitments are expanded: Miden writes 0x + 64 lowercase hex, while EVM
-- signers are lowercased to 0x + 40 hex on ingest. The reserved account-level
-- row below retains the floor when metadata has no usable signer entries.
-- Keep these patterns aligned with Auth::is_canonical_signer_commitment in
-- crates/server/src/metadata/auth/mod.rs.
CREATE TEMP TABLE auth_state_signer_expansion AS
SELECT s.account_id,
       s.last_auth_timestamp,
       signer.value #>> '{}' AS signer_commitment,
       COALESCE(
           auth_state.variant_count = 1
               AND jsonb_typeof(signer.value) = 'string'
               AND (signer.value #>> '{}') ~ auth_state.commitment_pattern,
           FALSE
       ) AS is_canonical
  FROM account_auth_state s
  JOIN account_metadata m ON m.account_id = s.account_id
 CROSS JOIN LATERAL (
     SELECT COALESCE(
                m.auth -> 'MidenFalconRpo' -> 'cosigner_commitments',
                m.auth -> 'MidenEcdsa' -> 'cosigner_commitments',
                m.auth -> 'EvmEcdsa' -> 'signers'
            ) AS commitments,
            CASE
                WHEN m.auth ? 'EvmEcdsa' THEN '^0x[0-9a-f]{40}$'
                ELSE '^0x[0-9a-f]{64}$'
            END AS commitment_pattern,
            (m.auth ? 'MidenFalconRpo')::integer
              + (m.auth ? 'MidenEcdsa')::integer
              + (m.auth ? 'EvmEcdsa')::integer AS variant_count
 ) auth_state
  LEFT JOIN LATERAL jsonb_array_elements(
      CASE
          WHEN jsonb_typeof(auth_state.commitments) = 'array'
              THEN auth_state.commitments
          ELSE '[]'::jsonb
      END
  ) signer(value) ON TRUE;

-- Keep the old account-scoped floor under a value that no real signer can
-- present. Runtime CAS checks this row as a lower bound for every signer.
INSERT INTO account_auth_state_per_signer
    (account_id, signer_commitment, last_auth_timestamp)
SELECT s.account_id, '__legacy_account_floor__', s.last_auth_timestamp
  FROM account_auth_state s
  JOIN account_metadata m ON m.account_id = s.account_id;

INSERT INTO account_auth_state_per_signer
    (account_id, signer_commitment, last_auth_timestamp)
SELECT DISTINCT account_id, signer_commitment, last_auth_timestamp
  FROM auth_state_signer_expansion
 WHERE is_canonical;

DROP TABLE auth_state_signer_expansion;

-- Independent of the expansion above: an account_auth_state row whose
-- metadata row is missing produces no expansion at all, and would otherwise
-- lose its replay floor to the DROP TABLE below without any error.
DO $$
DECLARE
    unmatched_accounts text;
BEGIN
    SELECT string_agg(s.account_id, ', ') INTO unmatched_accounts
      FROM account_auth_state s
     WHERE NOT EXISTS (
         SELECT 1
           FROM account_auth_state_per_signer replacement
          WHERE replacement.account_id = s.account_id
     );

    IF unmatched_accounts IS NOT NULL THEN
        RAISE EXCEPTION
            'account_auth_state: replay rows for account(s) % were not copied to per-signer state',
            unmatched_accounts;
    END IF;
END $$;

DROP TABLE account_auth_state;

ALTER TABLE account_auth_state_per_signer RENAME TO account_auth_state;

ALTER TABLE account_auth_state
    RENAME CONSTRAINT account_auth_state_per_signer_pkey TO account_auth_state_pkey;

ALTER TABLE account_auth_state
    RENAME CONSTRAINT account_auth_state_per_signer_account_id_fkey
    TO account_auth_state_account_id_fkey;
