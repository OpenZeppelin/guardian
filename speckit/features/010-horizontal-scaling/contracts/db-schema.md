# Contract: Database Schema (new migrations)

**Feature**: 010-horizontal-scaling

Three new Diesel migrations under `crates/server/migrations/`, embedded and run at
startup. Postgres backend only. Column details and lifecycle rules are in
[data-model.md](../data-model.md); this file fixes the migration contract.

## Migration: `<date>_auth_sessions`

`up.sql` creates `auth_sessions` (composite PK `(realm TEXT, token_digest
BYTEA)` so operator and EVM sessions are namespaced rather than relying on token
randomness, `subject JSONB`, `issued_at`, `expires_at`, `revoked_at` nullable) +
index on `expires_at` and `(realm, expires_at)`. `down.sql` drops it.

## Migration: `<date>_auth_challenges`

`up.sql` creates `auth_challenges` with composite PK `(realm TEXT, challenge_key
TEXT)` — `challenge_key` is the operator signing-digest hex or the EVM nonce —
plus `principal TEXT`, `payload JSONB` (realm-specific match/recover fields, see
data-model.md), `issued_at`, `expires_at`, `consumed_at` nullable + index on
`(realm, principal)` and `expires_at`. `down.sql` drops it.

## Migration: `<date>_worker_leases`

`up.sql` creates `worker_leases` (PK `lease_name TEXT`, `holder_id TEXT`,
`acquired_at`, `renewed_at`, `expires_at`, `fence_token BIGINT NOT NULL DEFAULT
0`). `down.sql` drops it.

## Migration execution under concurrent replica startup — REQUIRED

All replicas run the embedded migrations against one Postgres at boot. The runner
(`storage/postgres.rs`) MUST wrap `run_pending_migrations` in a Postgres
**session-level advisory lock** on a fixed key, acquired with a **bounded wait**:
poll `SELECT pg_try_advisory_lock($key)` until it succeeds or a timeout elapses ->
migrate -> `SELECT pg_advisory_unlock($key)`. One replica migrates; the rest poll,
then find nothing pending. The bounded wait (vs. an unbounded `pg_advisory_lock`)
means a replica stuck mid-migration fails the others fast rather than wedging the
fleet on boot. Without the lock, simultaneous first-deploy boots can race/deadlock
on identical migrations. (Acceptable here — short, single-connection — unlike the
canonicalization lease, which spans pool churn and uses a lease row instead.)

## Constraints on the migration set

- **Additive only**: no changes to existing tables (`states`, `deltas`,
  `delta_proposals`, `account_metadata`, `admin_actions`). No FKs to custody
  tables (keeps append-only delta lineage isolated — Constitution III).
- **Reversible**: every `up.sql` has a matching `down.sql`.
- **No data migration / backfill**: these tables start empty; sessions and
  challenges are ephemeral and challenges/sessions in flight at deploy time
  simply require a re-login (acceptable, documented in the runbook).
- **Filesystem backend creates none of these** — it uses in-memory stores.

## Atomic operations the impls rely on

- Challenge single-use consume: conditional `UPDATE ... SET consumed_at = now()
  WHERE realm = $1 AND challenge_key = $2 AND consumed_at IS NULL AND now() <
  expires_at` — affected-row-count `1` => this caller won the claim, `0` =>
  already consumed/expired.
- Lease acquire/steal: `INSERT ... ON CONFLICT (lease_name) DO UPDATE ... WHERE
  worker_leases.expires_at < now() OR worker_leases.holder_id = excluded.holder_id`.
- Lease renew: `UPDATE ... WHERE lease_name = $1 AND holder_id = $2 AND now() <
  expires_at`.
- Lease fence verify (submission boundary, mandatory): `SELECT 1 FROM
  worker_leases WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 AND
  now() < expires_at`.

These must be single round-trip statements (no read-modify-write races across
replicas).
