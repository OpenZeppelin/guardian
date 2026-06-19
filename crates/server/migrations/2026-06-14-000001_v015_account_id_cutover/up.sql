-- Miden 0.15 account-ID cutover purge.
--
-- Miden 0.15 invalidated account ID version 0 (0xMiden/protocol#2842): encoded
-- version `0` is now rejected, so every pre-0.15 (v0) account ID -- and every
-- serialized AccountDelta / TransactionSummary blob that embeds one -- fails to
-- deserialize under 0.15 with `\`0\` is not a known account ID version`. A v0 ID
-- is a proof-of-work-derived commitment with no v1 equivalent, so there is no
-- in-place migration. The Miden team confirmed pre-0.15 data can be dropped.
--
-- This one-time cutover clears Miden per-account operational data so 0.15 starts
-- from a clean, v1-only state. It runs once on the first 0.15 deploy, before any
-- v1 account can be created. The append-only `admin_actions` forensic audit
-- trail is deliberately preserved.
--
-- Scope: ONLY Miden rows are purged. EVM accounts share these tables, but their
-- IDs and serialized blobs are unaffected by the v0->v1 change, so they MUST
-- survive. The discriminator is `account_metadata.network_config->>'kind'`
-- (`NetworkConfig` serializes with `#[serde(tag = "kind")]`, e.g.
-- `{"kind":"miden",...}` vs `{"kind":"evm",...}`). EVM metadata is written
-- atomically with the account, so every live EVM row has a matching
-- `kind = 'evm'` metadata row; the child-table deletes therefore keep exactly
-- the confirmed-EVM rows and purge everything else -- including any Miden rows
-- orphaned from their metadata. This makes the migration safe to run on every
-- 0.15 deploy regardless of build features (it replaces the earlier
-- `--features evm` skip, which wrongly keyed data state off a compile flag).
--
-- Note: this covers the Postgres backend only. Filesystem-backend deployments
-- clear pre-0.15 data by removing the storage data directory.
--
-- IRREVERSIBLE: deleted rows cannot be restored (see down.sql). No foreign keys
-- reference these tables, so child rows are removed explicitly below.

DELETE FROM states
 WHERE account_id NOT IN (
   SELECT account_id FROM account_metadata WHERE network_config->>'kind' = 'evm'
 );

DELETE FROM deltas
 WHERE account_id NOT IN (
   SELECT account_id FROM account_metadata WHERE network_config->>'kind' = 'evm'
 );

DELETE FROM delta_proposals
 WHERE account_id NOT IN (
   SELECT account_id FROM account_metadata WHERE network_config->>'kind' = 'evm'
 );

DELETE FROM account_metadata
 WHERE network_config->>'kind' IS DISTINCT FROM 'evm';
