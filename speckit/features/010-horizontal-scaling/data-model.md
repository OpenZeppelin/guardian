# Phase 1 Data Model: Horizontal Scaling Correctness

**Feature**: 010-horizontal-scaling | **Date**: 2026-06-20

Three new Postgres tables, added as Diesel migrations under
`crates/server/migrations/` (embedded via `embed_migrations!`, run at startup —
`crates/server/src/storage/postgres.rs:29-47`). All three exist **only** in the
Postgres backend; the filesystem/dev backend uses in-memory equivalents and
creates no tables.

All timestamps are `TIMESTAMPTZ` and all expiry comparisons use the database
clock (`now()`), giving a single authoritative clock across replicas.

### Migration concurrency (multi-replica startup) — REQUIRED

With 2-6 replicas booting simultaneously (ECS rolling deploy or cold start),
every replica runs `embed_migrations!` against the **one** shared Postgres at the
same time. Diesel's embedded runner does not serialize concurrent runners safely
by default, so first-deploy startup can race or deadlock applying the same
migration. This feature MUST guard migration with a **Postgres session-level
advisory lock**:

```text
run_migrations(conn):
    SELECT pg_advisory_lock(<fixed_migration_key>);   -- blocks until sole holder
    run_pending_migrations(conn);                      -- no-op for replicas that lose the race
    SELECT pg_advisory_unlock(<fixed_migration_key>);
```

The first replica to grab the lock migrates; the others block, then find no
pending migrations and proceed. `pg_advisory_lock` is appropriate here (unlike for
the canonicalization lease) because this is a short, bounded, single-connection
critical section held only for the migration call — not across request/pool
churn. This change lives in `storage/postgres.rs:32-47` (`run_migrations`). See
the matching edge case in spec.md and the db-schema.md contract.

---

## Entity: Auth Session  → table `auth_sessions`

Replaces the per-process `Arc<Mutex<HashMap<[u8;32], OperatorSessionRecord>>>`
(`dashboard/state.rs:30`) and the EVM equivalent (`evm/session.rs`).

| Column | Type | Notes |
|---|---|---|
| `token_digest` | `BYTEA` (32) PRIMARY KEY | SHA-256 of the session token (never store plaintext; matches current `[u8;32]` keying) |
| `realm` | `TEXT` NOT NULL | `operator` \| `evm` (discriminator) |
| `subject` | `JSONB` NOT NULL | Realm-specific identity: operator `AuthenticatedOperator` or EVM `address`. Permissions are re-resolved from the live allowlist at use time (preserves `authenticate_session` behavior, `dashboard/state.rs:290-332`) |
| `issued_at` | `TIMESTAMPTZ` NOT NULL | |
| `expires_at` | `TIMESTAMPTZ` NOT NULL | indexed for TTL sweep |
| `revoked_at` | `TIMESTAMPTZ` NULL | set on logout; a non-null value => session rejected on every replica (FR-003) |

**Indexes**: PK on `token_digest`; index on `expires_at` (sweep);
index on `(realm, expires_at)`.

**Lifecycle / validation**:
- Created on successful `verify`.
- Valid iff `revoked_at IS NULL AND now() < expires_at`.
- Logout sets `revoked_at = now()` (idempotent).
- A revoked row is **kept until its original `expires_at`** so the revocation is
  honored across every replica for as long as the token would otherwise have been
  valid; setting `revoked_at` (not deleting) is what makes logout effective fleet-
  wide. The sweep then deletes any row where `expires_at < now()` (covers both
  naturally expired and revoked-then-expired rows). There is no separate
  "revocation grace" — a revoked token is rejected immediately via `revoked_at`
  and the row is reclaimed at natural expiry.
- **Invariant**: stored subject identity is authoritative, but authorization
  (permissions) is always recomputed from the current allowlist — no stale
  permission capture.

---

## Entity: Auth Challenge  → table `auth_challenges`

Replaces per-process `Arc<Mutex<HashMap<String, Vec<PendingChallenge>>>>`
(`dashboard/state.rs:29`) and the EVM equivalent. Supports issue-on-A /
verify-on-B (FR-001).

| Column | Type | Notes |
|---|---|---|
| `signing_digest` | `BYTEA` (32) PRIMARY KEY | challenge identity = the exact value the client signs and returns (see decision below). No surrogate uuid. |
| `realm` | `TEXT` NOT NULL | `operator` \| `evm` |
| `principal` | `TEXT` NOT NULL | operator commitment (current key, `dashboard/state.rs:108-168`) or EVM address; indexed |
| `issued_at` | `TIMESTAMPTZ` NOT NULL | |
| `expires_at` | `TIMESTAMPTZ` NOT NULL | indexed |
| `consumed_at` | `TIMESTAMPTZ` NULL | set when `verify` succeeds; prevents replay across replicas |

**Challenge identity decision** (resolves the previously-open PK type): the PK is
the `signing_digest`, not a uuid. Today `verify` matches a signed response against
a `Vec<PendingChallenge>` per principal (`dashboard/state.rs:170-288`); the signed
digest the client returns uniquely identifies which challenge it answers, so it is
the natural single-use key and preserves current matching semantics. `consume` is
therefore keyed by `(realm, signing_digest)` (see coordination-traits.md).

**Indexes**: PK on `signing_digest`; index on `(realm, principal)`; index on
`expires_at`.

**Lifecycle / validation**:
- Created by `issue_challenge`.
- Consumable iff `consumed_at IS NULL AND now() < expires_at`; consumption sets
  `consumed_at = now()` atomically (single-use; a replay on any replica fails —
  FR-003, US1 scenario 3).
- Multiple pending challenges per principal allowed (current code stores a
  `Vec`); the `(realm, principal)` index supports lookup.

---

## Entity: Worker Lease  → table `worker_leases`

Backs single-owner canonicalization (FR-004/005/006, US2). Generic enough for
future background workers.

| Column | Type | Notes |
|---|---|---|
| `lease_name` | `TEXT` PRIMARY KEY | e.g. `canonicalization` |
| `holder_id` | `TEXT` NOT NULL | replica identity (e.g. hostname/task-id + random suffix, generated at boot) |
| `acquired_at` | `TIMESTAMPTZ` NOT NULL | when current holder first took the lease |
| `renewed_at` | `TIMESTAMPTZ` NOT NULL | last heartbeat |
| `expires_at` | `TIMESTAMPTZ` NOT NULL | `renewed_at + ttl`; another replica may claim only when `now() >= expires_at` |
| `fence_token` | `BIGINT` NOT NULL | monotonically incremented on each (re)acquisition; guards against a stale holder acting after losing the lease |

**Acquire / renew (single atomic statement)**:
- Acquire/steal: `INSERT ... ON CONFLICT (lease_name) DO UPDATE SET holder_id =
  excluded.holder_id, acquired_at = now(), renewed_at = now(), expires_at = now()
  + ttl, fence_token = worker_leases.fence_token + 1 WHERE worker_leases.expires_at
  < now() OR worker_leases.holder_id = excluded.holder_id` (claim only if expired
  or already mine).
- Renew: `UPDATE ... SET renewed_at = now(), expires_at = now() + ttl WHERE
  lease_name = $1 AND holder_id = $2 AND now() < expires_at` — runs on its **own
  timer concurrent with the pass** (not at tick boundaries); a failed renew (0
  rows) means the lease was lost; the renewal task trips the pass's cancellation
  signal (see coordination-traits.md "Renewal concurrency").
- Verify-held (fence check at submission boundary): `SELECT 1 FROM worker_leases
  WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 AND now() <
  expires_at` — the processor MUST run this immediately before any on-chain
  submission / canonical promotion and skip the write if it returns no row.

**Timing constraints**:
- `renew_interval << ttl` (e.g. renew every 5s, ttl 30s).
- The TTL is sized **solely** for renew/failover: it must comfortably exceed one
  renew interval (a healthy holder never loses its lease) and it sets the failover
  bound — another replica may claim only after `ttl` elapses without a renew, so
  failover (SC-003) happens within `ttl` of the holder dying.
- The TTL is **independent of** the canonicalization `submission_grace_period`
  (600s default) and the check interval (10s); those govern delta promotion
  timing, not lease ownership. Do not couple them.

**Invariant (no split-brain)**: at most one `holder_id` can satisfy the renew
predicate at a time because acquisition is a single atomic conditional write
against the DB clock. The cooperative-cancellation abort path has a small window
between lease loss and the pass stopping; the **mandatory** fence check
(`verify_held`) at every submission boundary closes that window — even if two
replicas momentarily both believe they lead, only the current `fence_token` holder
can commit a state-mutating write. TTL + voluntary abort alone is NOT relied on.

---

## In-memory equivalents (filesystem/dev backend)

No tables. The `coordination` module provides:
- `InMemorySessionStore` / `InMemoryChallengeStore` — the current
  `Arc<Mutex<HashMap>>` behavior, byte-for-byte.
- `AlwaysLeader` — `try_acquire`/`renew` always succeed (single replica is always
  the leader).

This keeps single-replica/dev behavior identical to today and requires no
database (FR-014; constitution dev-default invariant).

---

## Relationships & boundaries

- `auth_sessions` / `auth_challenges` are independent of the custody record
  tables (`states`, `deltas`, `delta_proposals`, `account_metadata`) — no FKs,
  no impact on append-only delta lineage (Constitution III).
- `worker_leases` is pure coordination metadata; it never participates in or
  alters the pending->candidate->canonical/discarded transitions — it only gates
  **which replica** executes them.
- None of these tables are exposed on any client wire contract.
