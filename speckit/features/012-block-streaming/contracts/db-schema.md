# Contract: Database Schema (new migration)

**Feature**: 012-block-streaming

One additive, reversible Diesel migration under `crates/server/migrations/`,
embedded via `embed_migrations!` and run at startup under the existing
advisory lock (feature 010). Postgres backend only; the filesystem backend
uses a cursor file and in-memory observations (see `data-model.md`).

## Migration: `2026-11-XX-000001_block_watch`

`up.sql`:

```sql
CREATE TABLE block_watch_cursor (
    network            TEXT PRIMARY KEY,
    last_block         BIGINT NOT NULL,
    genesis_commitment TEXT,          -- the chain the cursor counts; a change = a reset network
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    holder_id          TEXT NOT NULL,
    fence_token        BIGINT NOT NULL
);

CREATE TABLE block_watch_observations (
    account_id          TEXT NOT NULL,
    block_num           BIGINT NOT NULL,
    on_chain_commitment TEXT NOT NULL,
    observed_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts            INTEGER NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, block_num)
);

CREATE INDEX block_watch_observations_due_idx
    ON block_watch_observations (next_attempt_at, block_num);
```

`down.sql` drops the index and both tables.

## Queries (all in `coordination/postgres/block_watch_store.rs`)

- **cursor read**: `SELECT last_block FROM block_watch_cursor WHERE network = $1`.
- **cursor write (fenced, forward-only)**, one transaction:

  ```sql
  SELECT holder_id, fence_token FROM worker_leases
   WHERE lease_name = 'release_sweep' FOR SHARE;        -- must equal the caller's lease, else StaleLease
  INSERT INTO block_watch_cursor (network, last_block, updated_at, holder_id, fence_token)
  VALUES ($1, $2, now(), $3, $4)
  ON CONFLICT (network) DO UPDATE
     SET last_block = EXCLUDED.last_block, updated_at = now(),
         holder_id = EXCLUDED.holder_id, fence_token = EXCLUDED.fence_token
   WHERE block_watch_cursor.last_block < EXCLUDED.last_block;
  ```

  Zero rows updated with a matching lease means a backwards write: treated
  as a bug (`warn` + metric), never applied.
- **record observation**: `INSERT … ON CONFLICT DO NOTHING`.
- **next due**: `SELECT … WHERE next_attempt_at <= now() ORDER BY block_num, account_id LIMIT 1`.
- **delete / defer / count**: as in `data-model.md`.

## Metadata and storage additions (existing tables, new queries)

- `MetadataStore::filter_release_sweep_ids(ids: &[String]) -> Vec<String>`:
  `release_sweepable_rows().filter(account_metadata::account_id.eq_any(chunk))
  .select(account_id)`, chunked at 1,000 ids per statement; PK index serves
  it. Filesystem: filter the in-memory/disk metadata by the same predicate.
- `StorageBackend::pull_state_commitments_batch(ids: &[&str]) ->
  HashMap<String, String>`: `SELECT account_id, commitment FROM states WHERE
  account_id = ANY($1)` (chunked); missing rows omitted (same policy as
  `pull_states_batch`). Filesystem: default implementation over
  `pull_state_commitment`.

## Constraints

- **Additive only**; no FKs into custody tables (Constitution III).
- **Reversible**; `down.sql` complete.
- **No backfill**: both tables start empty; the first holder initializes the
  cursor at the tip (plan D5).
- Postgres tests for the new store live under a module path containing
  `postgres` and are `#[ignore]`d for `scripts/test-postgres.sh`
  (AGENTS.md).
- Release process: per the repo convention a migration ships in its own PR
  ahead of a release PR (RC = two PRs).
