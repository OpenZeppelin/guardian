# Contract: Database Schema (new migrations)

**Feature**: 010-horizontal-scaling

Three new Diesel migrations under `crates/server/migrations/`, embedded and run at
startup. Postgres backend only. Column details and lifecycle rules are in
[data-model.md](../data-model.md); this file fixes the migration contract.

## Migration: `<date>_auth_sessions`

`up.sql` creates `auth_sessions` (PK `token_digest BYTEA`, `realm TEXT`,
`subject JSONB`, `issued_at`, `expires_at`, `revoked_at` nullable) + index on
`expires_at` and `(realm, expires_at)`. `down.sql` drops it.

## Migration: `<date>_auth_challenges`

`up.sql` creates `auth_challenges` (PK `signing_digest BYTEA` — the value the
client signs and returns; no surrogate id, see data-model.md), `realm TEXT`,
`principal TEXT`, `issued_at`, `expires_at`, `consumed_at` nullable + index on
`(realm, principal)` and `expires_at`. `down.sql` drops it.

## Migration: `<date>_worker_leases`

`up.sql` creates `worker_leases` (PK `lease_name TEXT`, `holder_id TEXT`,
`acquired_at`, `renewed_at`, `expires_at`, `fence_token BIGINT NOT NULL DEFAULT
0`). `down.sql` drops it.

## Migration execution under concurrent replica startup — REQUIRED

All replicas run the embedded migrations against one Postgres at boot. The runner
(`storage/postgres.rs:32-47`) MUST wrap `run_pending_migrations` in a Postgres
**session-level advisory lock** on a fixed key:
`SELECT pg_advisory_lock($key)` -> migrate -> `SELECT pg_advisory_unlock($key)`.
One replica migrates; the rest block, then find nothing pending. Without this,
simultaneous first-deploy boots can race/deadlock on identical migrations. (This
advisory lock is acceptable here — short, single-connection, bounded — unlike for
the canonicalization lease, which spans pool churn and uses a lease row instead.)

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
  WHERE signing_digest = $1 AND realm = $2 AND consumed_at IS NULL AND now() <
  expires_at RETURNING ...`.
- Lease acquire/steal: `INSERT ... ON CONFLICT (lease_name) DO UPDATE ... WHERE
  worker_leases.expires_at < now() OR worker_leases.holder_id = excluded.holder_id`.
- Lease renew: `UPDATE ... WHERE lease_name = $1 AND holder_id = $2 AND now() <
  expires_at`.
- Lease fence verify (submission boundary, mandatory): `SELECT 1 FROM
  worker_leases WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 AND
  now() < expires_at`.

These must be single round-trip statements (no read-modify-write races across
replicas).
