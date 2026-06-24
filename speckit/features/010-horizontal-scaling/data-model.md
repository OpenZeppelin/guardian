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
| `realm` | `TEXT` NOT NULL | `operator` \| `evm` (part of PK) |
| `challenge_key` | `TEXT` NOT NULL | per-challenge unique key **within a realm** (part of PK). Operator: the signing digest hex. EVM: the challenge nonce. This is what `consume` targets. |
| `principal` | `TEXT` NOT NULL | operator commitment (`dashboard/state.rs:108-168`) or EVM address; indexed for `active_for` lookup |
| `payload` | `JSONB` NOT NULL | realm-specific fields needed to match/recover at verify time (see below) |
| `issued_at` | `TIMESTAMPTZ` NOT NULL | |
| `expires_at` | `TIMESTAMPTZ` NOT NULL | indexed |
| `consumed_at` | `TIMESTAMPTZ` NULL | set when `verify` succeeds; single-use across replicas |

**Realm-aware payload (resolves the EVM modeling gap)**: the two realms verify
differently, so a single `signing_digest` column does not model both:
- **Operator** matches by Falcon-verifying the stored signing digest (a `Word`)
  against the submitted signature (`dashboard/state.rs:228-230`). `payload` =
  `{ "signing_digest": "<hex>" }`; `challenge_key` = that hex.
- **EVM** matches by **nonce**, then recovers the signer from the **full original
  challenge** (`address`, `nonce`, `issued_at`, `expires_at`) via
  `recover_session_address` (`evm/session.rs:112-127`). `payload` =
  `{ "address", "nonce", "issued_at", "expires_at" }`; `challenge_key` = the nonce.

Verification matching (Falcon verify / nonce compare + ECDSA recover) runs in
Rust, not SQL: `active_for(principal)` returns the unexpired, unconsumed payloads;
the caller matches one; then `consume(challenge_key)` atomically claims it.

**Primary key**: `(realm, challenge_key)`. **Indexes**: PK; index on
`(realm, principal)` for `active_for`; index on `expires_at` for the sweep.

**Lifecycle / validation**:
- Created by `issue_challenge` (per-principal cap via `max_outstanding`, oldest
  pruned, matching today's `Vec` cap).
- Consumable iff `consumed_at IS NULL AND now() < expires_at`; `consume`
  conditionally sets `consumed_at = now()` and reports whether it won the race
  (single-use; a replay on any replica fails — FR-003, US1 scenario 3).
- Multiple pending challenges per principal allowed; the `(realm, principal)`
  index supports `active_for`.

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
(`verify_held`) immediately before every state-mutating write strongly mitigates
that window. Note the fence is **advisory** (a separate round-trip, TOCTOU): a
lease could in principle be stolen between the check and the write. That residual
window is benign here because the canonical writes are **idempotent deterministic
upserts** — the same delta produces identical state/delta bytes regardless of
which replica writes — and retry/discard writes are likewise idempotent for a
given candidate. So a brief overlap cannot corrupt state; it can at most
re-apply the same transition. TTL + voluntary abort alone is NOT relied on.

---

## In-memory equivalents (filesystem/dev backend)

No tables. The `coordination` module provides:
- `InMemorySessionStore` / `InMemoryChallengeStore` — the current
  `Arc<Mutex<HashMap>>` behavior, byte-for-byte.
- `AlwaysLeader` — `try_acquire`/`renew`/`verify_held` always succeed (single
  replica is always the leader).

Selected when the filesystem backend is active (backend-derived selection, R9 —
**not** gated on `GUARDIAN_MAX_REPLICAS`). A Postgres deployment always uses the
shared (table-backed) impls. This keeps single-replica/dev behavior identical to
today and requires no database (FR-014; constitution dev-default invariant).

---

## Relationships & boundaries

- `auth_sessions` / `auth_challenges` are independent of the custody record
  tables (`states`, `deltas`, `delta_proposals`, `account_metadata`) — no FKs,
  no impact on append-only delta lineage (Constitution III).
- `worker_leases` is pure coordination metadata; it never participates in or
  alters the pending->candidate->canonical/discarded transitions — it only gates
  **which replica** executes them.
- None of these tables are exposed on any client wire contract.
