-- Miden 0.16 irreversible reset.
--
-- Miden 0.16 moved the custody account's on-chain surface in several ways at
-- once, and no stored Miden row survives any of them:
--
--   * the guarded-multisig procedure roots changed, so stored proposals no
--     longer address the procedures they were signed against;
--   * `miden-crypto` 0.28 changed ECDSA-k256 public-key commitments to hash
--     native affine-coordinate limbs, so stored approver commitments no longer
--     match their keys;
--   * `miden-vm` 0.29 changed the signature advice ABI, so stored signatures
--     cannot be replayed into a transaction;
--   * component storage slots were renamed from `openzeppelin::*` to
--     `miden::standards::*`, so stored state cannot be read back by name;
--   * the transaction summary layout changed and now binds a chain anchor, so
--     stored summaries cannot be recomputed or re-verified.
--
-- Stored Miden states, deltas, proposals, and metadata therefore can neither be
-- deserialized nor recomputed. There is no in-place migration and no
-- partial-salvage path.
--
-- Scope: ONLY Miden rows are purged, and the discriminator is the DATA
-- (`account_metadata.network_config->>'kind'`), not a build feature and not an
-- assumption that a given deployment happens to be Miden-only. This mirrors
-- 2026-06-14-000001_v015_account_id_cutover, whose comment records that an
-- earlier `--features evm` skip "wrongly keyed data state off a compile flag".
-- EVM account IDs and serialized blobs are untouched by the 0.16 changes, so
-- they MUST survive. The child deletes keep exactly the confirmed-EVM rows and
-- purge everything else, including Miden rows orphaned from their metadata.
--
-- `account_auth_state` is absent from the statements below on purpose: it is
-- declared `REFERENCES account_metadata(account_id) ON DELETE CASCADE`, so the
-- final delete clears it. (The 0.15 cutover's "no foreign keys reference these
-- tables" note predates that table and no longer holds.)
--
-- Preserved deliberately: `admin_actions` (append-only, trigger-protected
-- forensic audit trail), `auth_sessions` and `auth_challenges` (operator
-- session state, no FK to `account_metadata`, no account state),
-- `storage_encryption_marker` (encryption configuration), and `worker_leases`
-- (keyed by `lease_name`, not by account).
--
-- Note: Postgres backend only. Filesystem-backend deployments reset by starting
-- from empty storage and metadata directories while preserving the keystore
-- directory.
--
-- IRREVERSIBLE: deleted rows cannot be restored (see down.sql).

-- Block a replica still running the pre-0.16 binary from inserting Miden rows
-- between these deletes and the commit, which would leave behind exactly the
-- incompatible residue this reset exists to remove. Operators are told to stop
-- the old server first; the lock enforces that instead of trusting it.
--
-- Every purged table is locked, not just `account_metadata`. Locking metadata
-- alone would not be enough: `push_delta` and canonicalization do take
-- `lock_account_metadata` and would queue, but `submit_delta_proposal` inserts
-- straight into `delta_proposals` without reading or locking
-- `account_metadata`, and no foreign key forces an implicit lock, so a proposal
-- could still land after `DELETE FROM delta_proposals` and before this commit.
--
-- Cost: `ACCESS EXCLUSIVE` also blocks EVM reads and writes on these tables for
-- the duration of the deletes. That is acceptable because the old server is
-- supposed to be stopped, which makes the lock uncontended and the window
-- short.
--
-- Bounded wait for the same reason as 2026-07-31-000001_account_auth_state:
-- migrations run at server startup, so an unbounded wait behind a long-running
-- transaction would stall the fleet. Failing fast lets the orchestrator restart
-- and retry.
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
