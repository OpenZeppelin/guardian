-- Miden 0.17 irreversible reset.
--
-- Miden 0.17 changed the encoding of every object Guardian stores for a Miden
-- account, and no stored Miden row survives any of it:
--
--   * `Account`, `AccountHeader` and `AssetId` carry a serialized version
--     field, and `Account::from_json` rejects the 0.16 encoding outright
--     ("account version is 241 but only version 1 is supported");
--   * account code procedures are sorted after the authentication procedure,
--     so every stored account code commitment differs from what the same
--     components produce now;
--   * the account delta and storage patch commitments are versioned and their
--     domain separators moved into the hasher capacity word, so stored deltas
--     no longer recompute to the commitments they were signed under;
--   * the transaction summary is versioned, binds a caller-chosen block, and
--     carries six user params instead of seven, so stored summaries cannot be
--     deserialized, recomputed, or re-verified;
--   * the multisig auth argument is now the commitment to a three-word preimage
--     (bound block and approval expiration, salt, fee conversion info), so a
--     stored proposal's request cannot be rebuilt against the new auth
--     procedure, whose root also changed.
--
-- Stored Miden states, deltas, proposals, and metadata therefore can neither be
-- deserialized nor recomputed. There is no in-place migration and no
-- partial-salvage path.
--
-- Scope, locking, and what is preserved are exactly as in
-- 2026-08-24-000001_miden_016_irreversible_reset: ONLY Miden rows are purged,
-- keyed off the DATA (`account_metadata.network_config->>'kind'`), so EVM rows
-- survive; `account_auth_state` clears by cascade; `admin_actions`,
-- `auth_sessions`, `auth_challenges`, `storage_encryption_marker`,
-- `worker_leases`, and the dashboard stats snapshot are untouched.
--
-- Note: Postgres backend only. Filesystem-backend deployments reset by starting
-- from empty storage and metadata directories while preserving the keystore
-- directory.
--
-- IRREVERSIBLE: deleted rows cannot be restored (see down.sql).

SET LOCAL lock_timeout = '5s';

LOCK TABLE delta_proposals, deltas, states, account_metadata
  IN ACCESS EXCLUSIVE MODE;

DELETE FROM delta_proposals
 WHERE account_id NOT IN (
   SELECT account_id FROM account_metadata WHERE network_config->>'kind' = 'evm'
 );

DELETE FROM deltas
 WHERE account_id NOT IN (
   SELECT account_id FROM account_metadata WHERE network_config->>'kind' = 'evm'
 );

DELETE FROM states
 WHERE account_id NOT IN (
   SELECT account_id FROM account_metadata WHERE network_config->>'kind' = 'evm'
 );

DELETE FROM account_metadata
 WHERE network_config->>'kind' IS DISTINCT FROM 'evm';
