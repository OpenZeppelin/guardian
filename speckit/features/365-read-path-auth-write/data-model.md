# Data Model: Account Replay State Split

**Feature**: `365-read-path-auth-write` | **Date**: 2026-07-31
**Source**: spec.md Key Entities; research.md Option A sub-decisions A1–A8

## Entities

### `account_auth_state` (new, Postgres)

The per-account replay-protection record — the single hot row this feature exists to isolate.

```sql
CREATE TABLE account_auth_state (
    account_id VARCHAR(128) PRIMARY KEY
        REFERENCES account_metadata(account_id) ON DELETE CASCADE,
    last_auth_timestamp BIGINT NOT NULL
) WITH (fillfactor = 50);
```

| Field | Type | Semantics |
|---|---|---|
| `account_id` | VARCHAR(128) PK, FK → `account_metadata` | Same identifier space as today; cascade delete prevents orphan auth state |
| `last_auth_timestamp` | BIGINT NOT NULL | Milliseconds-since-epoch of the most recently accepted authenticated request. `NOT NULL` because a row exists **only after** the account's first successful authentication (the upsert creates it); "never authenticated" is row absence, replacing today's `NULL` |

Design constraints (from research.md):

- **No other columns.** No `updated_at` mirror (FR-008; A2). The table has one job.
- **`fillfactor = 50`** (A3): rows are ~40 bytes, so pages hold plenty even half-filled; the free space keeps updates HOT (heap-only tuples — no index maintenance, page-local dead-tuple pruning). This is the mechanism by which the split converts the MVCC full-tuple rewrite into a cheap in-page update.
- **No secondary indexes.** The PK is the only access path; anything else would re-block HOT.

### `account_metadata` (changed, Postgres)

Loses `last_auth_timestamp`. Everything else is untouched.

```sql
ALTER TABLE account_metadata DROP COLUMN last_auth_timestamp;
```

Consequences by design:

- `updated_at` regains its meaning: it now advances **only** on configuration changes (FR-008), which also stabilises dashboard pagination ordering (`postgres.rs` `list_paginated` orders by `updated_at DESC`).
- `PostgresMetadataStore::set()` no longer reads or writes the field, structurally eliminating the stale-clobber race (research.md, latent defect 1).

### `AccountMetadata` (changed, Rust struct — `crates/server/src/metadata/mod.rs:16`)

`pub last_auth_timestamp: Option<i64>` is **removed**. All construction sites update mechanically (registration paths in `api/grpc.rs`/`api/http.rs`, dashboard code, canonicalization processor, EVM service, tests/mocks/fixtures). `evm/service.rs:133` drops its preserve-the-field workaround entirely.

The field never appears on any wire response or dashboard payload, so its removal is invisible outside `crates/server` (verified in research.md, Resolved Unknowns).

### Filesystem backend auth-state file (new — `crates/server/src/metadata/filesystem.rs`)

A dedicated persisted map, separate from the metadata cache file:

```text
<metadata_dir>/auth_state.json    # { "<account_id>": <last_auth_timestamp_i64>, ... }
```

- Guarded by the backend's existing write lock; persisted (tiny file) on each successful CAS instead of rewriting the whole metadata cache (today's `persist(&cache)` at `filesystem.rs:205`). Writes are atomic (write-temp-then-rename, matching the backend's existing persist discipline) so a crash mid-write never truncates or loses the file.
- **Legacy seed rule (one-time, satisfies FR-006 on filesystem)**: at startup, if `auth_state.json` is absent **and** persisted metadata carries legacy `last_auth_timestamp` values, those values are read via the file-format (not public-struct) deserializer, written into a fresh `auth_state.json`, and the file is persisted **immediately — even when the seeded map is empty** — so after the first post-upgrade boot the file always exists. If the file exists, legacy values in metadata files are ignored. The public `AccountMetadata` struct never regains the field.
- **Fail-open guard**: because the seed rule only fires when `auth_state.json` is absent, a *later* deletion of the file must not silently re-seed from the frozen (stale) legacy values — that would reopen a replay window for every timestamp between the legacy value and the deletion. The immediate-creation rule above makes post-first-boot absence an anomaly; when detected (metadata exists, auth-state file missing, legacy values present), the server logs a prominent warning that replay state was lost, and starts with empty state rather than stale state. Residual: deleting `auth_state.json` discards replay history within the skew window — equivalent to deleting any other filesystem state file, and documented as a limitation of the dev/test backend.
- **Deployment model**: the filesystem backend is single-process by design across *all* its operations (process-local `RwLock`, whole-file persistence — `filesystem.rs:18`); multi-replica deployments require Postgres. This is a pre-existing, documented limitation the feature inherits unchanged — cross-process CAS atomicity is provided by the Postgres backend only (FR-001, SC-003).

## Internal contract changes

### `MetadataStore` trait (`crates/server/src/metadata/mod.rs:179`)

```rust
// before
async fn update_last_auth_timestamp_cas(&self, account_id: &str, new_timestamp: i64, now: &str) -> Result<bool, String>;
// after
async fn update_last_auth_timestamp_cas(&self, account_id: &str, new_timestamp: i64) -> Result<bool, String>;
```

The `now` parameter existed solely to bump `updated_at`; FR-008 abolishes that side effect (A6). Return semantics unchanged: `Ok(true)` accepted, `Ok(false)` replay, `Err` storage failure. The call site (`services/mod.rs:162-186`) keeps its exact error mapping.

### Postgres CAS becomes a single-statement upsert (A1)

```sql
INSERT INTO account_auth_state (account_id, last_auth_timestamp)
VALUES ($1, $2)
ON CONFLICT (account_id) DO UPDATE
    SET last_auth_timestamp = EXCLUDED.last_auth_timestamp
    WHERE account_auth_state.last_auth_timestamp < EXCLUDED.last_auth_timestamp;
```

- Affected rows `1` ⇒ accepted (first auth inserts; later auths conditionally update).
- Affected rows `0` ⇒ replay (conflict row exists and the guard failed) → `Ok(false)`.
- Atomicity: the conditional `DO UPDATE` takes the row lock, so two concurrent same-timestamp requests admit exactly one winner — same property, same mechanism class as today's conditional `UPDATE`.
- Account existence is pre-checked by the metadata `get()` earlier in `resolve_account`; the FK backstops the race with a concurrent account deletion.

## State transitions (account replay state)

```text
(no row)              --first accepted auth-->            row(ts = T1)
row(ts = T1)          --auth with T2 > T1-->              row(ts = T2)          [accepted]
row(ts = T1)          --auth with T2 <= T1-->             row(ts = T1)          [rejected: replay]
row(ts = *)           --account deleted-->                (no row, via cascade)
```

The skew window check (±5 min, `MAX_TIMESTAMP_SKEW_MS`) remains a separate, earlier gate in `resolve_account` and is untouched.

## Migration (`2026-07-31-000001_account_auth_state`)

`up.sql` — one transaction (Diesel embedded migrations run transactionally at server startup):

```sql
CREATE TABLE account_auth_state ( ... ) WITH (fillfactor = 50);

INSERT INTO account_auth_state (account_id, last_auth_timestamp)
SELECT account_id, last_auth_timestamp
  FROM account_metadata
 WHERE last_auth_timestamp IS NOT NULL;

ALTER TABLE account_metadata DROP COLUMN last_auth_timestamp;
```

`down.sql` restores the column, backfills it from `account_auth_state`, and drops the table.

**FR-006 analysis**: within the transaction, replay state is never observable as absent or duplicated. Post-migration, every previously recorded timestamp is enforced from the new table. A captured pre-upgrade request replayed post-upgrade hits the same `<` guard against the same value.

**Mixed-version fleet**: after the column drop, an old binary's metadata `SELECT` (explicit column list, `schema.rs:86`) errors on every call → the old replica fails **closed** (storage errors, no authentication against stale state). Operator note in quickstart.md: during a rolling deploy, old replicas return errors until replaced; no replay exposure exists in the window.

## Validation rules (→ tests, FR-009)

1. **CAS return-mapping contract (store-level unit test, both backends)** — the explicit contract for the affected-rows → `Ok(false)` mapping: no existing row + valid timestamp → `Ok(true)` and row created; stored `T`, new `> T` → `Ok(true)`; stored `T`, new `== T` → `Ok(false)` (0 rows affected on Postgres); stored `T`, new `< T` → `Ok(false)`; stored value unchanged after every `Ok(false)`. `Err` is reserved for storage failure — a replay must never surface as `Err`.
2. Same-timestamp replay rejected end-to-end through `resolve_account` (both backends), with the frozen `AuthenticationFailed` error.
3. Older-timestamp request rejected (both backends).
4. First request for an account with no prior state accepted; immediate identical retry rejected.
5. Concurrent same-timestamp race: exactly one winner — Postgres via parallel connections (cross-process atomicity), filesystem via concurrent in-process tasks (its supported deployment model).
6. `updated_at` does not advance on authenticated reads; does advance on non-auth metadata mutations (configuration change, pause/release, pending-candidate transitions) — FR-008 as respecified.
7. Postgres migration backfill: pre-migration timestamps enforced post-migration (Postgres-gated test).
8. Filesystem legacy seed: metadata file with a legacy `last_auth_timestamp` value yields a populated `auth_state.json` on first run and enforcement from it; the file is created immediately even when empty; absence after first boot triggers the fail-open guard warning, not a silent re-seed from stale values.
9. Two-replica replay rejection on the Postgres backend (diagnostic stack, quickstart runbook — SC-003).
