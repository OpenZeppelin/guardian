# Data Model: Block-Driven Switch Detection

**Feature**: 012-block-streaming | **Date**: 2026-11-02

Two new Postgres tables (one additive, reversible Diesel migration under
`crates/server/migrations/`, run under the existing advisory lock), their
filesystem-backend equivalents, and the in-memory types the feed passes
around. No change to `states`, `deltas`, `delta_proposals`,
`account_metadata`, or `admin_actions`.

## `block_watch_cursor`

The last block fully processed by block watch, one row per network (today:
the one configured Miden network).

| Column | Type | Notes |
|---|---|---|
| `network` | `TEXT` PK | `NetworkType` name (`MidenDevnet`, …). |
| `last_block` | `BIGINT NOT NULL` | u32 block number; `BIGINT` avoids sign issues. |
| `genesis_commitment` | `TEXT` | Genesis commitment of the chain `last_block` counts (node `Status`); another one means the network was reset. |
| `updated_at` | `TIMESTAMPTZ NOT NULL` | Database clock. |
| `holder_id` | `TEXT NOT NULL` | Lease holder that wrote it (diagnostics). |
| `fence_token` | `BIGINT NOT NULL` | Fence token of the lease the write was made under. |

Write rule (Postgres): one transaction that locks the `worker_leases` row
for `release_sweep` (`FOR SHARE`), checks `holder_id` / `fence_token` match
the caller's lease (same predicate `storage::LeaseFence` uses for
canonicalization writes), and upserts the cursor **forward-only**
(`WHERE block_watch_cursor.last_block < EXCLUDED.last_block`). A mismatch is
`BlockWatchStoreError::StaleLease` and stops the consumer (the lease is
gone). A "jump" (D5) is a forward write too.

Filesystem backend: `<metadata dir>/block_watch/cursor.json`, one entry
per network keyed by its name,
`{ "MidenDevnet": { "last_block": N, "updated_at": "..." } }`, written
atomically (temp + rename). Single process, no fence.

## `block_watch_observations`

The worklist handed from the consumer to the visitor: a guarded account
moved on chain to a commitment this server does not hold.

| Column | Type | Notes |
|---|---|---|
| `account_id` | `TEXT NOT NULL` | Hex account id (as in `account_metadata`). |
| `block_num` | `BIGINT NOT NULL` | Block the move was observed in. |
| `on_chain_commitment` | `TEXT NOT NULL` | `final_state_commitment` of the update, `word_to_hex` format. |
| `observed_at` | `TIMESTAMPTZ NOT NULL` | Database clock at insert. |
| `attempts` | `INTEGER NOT NULL DEFAULT 0` | Visits that ended `Deferred`/`Failed`. |
| `next_attempt_at` | `TIMESTAMPTZ NOT NULL` | Due time; `now()` on insert. |

PK `(account_id, block_num)`; index `(next_attempt_at, block_num,
account_id COLLATE "C")` for `next_due_observation`, which serves the one
due longest first in exactly that order (account ids bytewise, as the
in-memory store compares them).

The same migration adds `idx_deltas_account_new_commitment` on `deltas
(account_id, new_commitment)`: block watch matches moved accounts against
their own canonical and candidate post-states pair by pair, one probe each,
instead of reading each account's whole delta history.

Lifecycle:

- **record** (consumer): one `INSERT … SELECT FROM unnest(…) ON CONFLICT
  (account_id, block_num) DO NOTHING` per block — a replayed block is
  idempotent. `observed_at` / `next_attempt_at` come from the server
  clock.
- **next_due** (visitor): `SELECT … WHERE next_attempt_at <= now() ORDER BY
  block_num, account_id LIMIT 1`.
- **delete** (visit `Checked`): by PK.
- **defer** (visit `Deferred`/`Failed`): `attempts = attempts + 1,
  next_attempt_at = now() + backoff(attempts)`; the visitor deletes instead
  when `attempts >= max_attempts`.
- **count_pending** (gauge / dashboard): `SELECT count(*)`.
- **next_due_at** (visit loop wake-up): `SELECT min(next_attempt_at)`.
- **purge_account** (on `/configure` re-onboarding? — not needed: a visit
  re-reads the stored commitment and resolves stale observations as
  `Checked`).

Filesystem backend: `BTreeMap<(u32 /*block*/, String /*account*/),
Observation>` behind a `Mutex` in the process; lost on restart (documented;
restart requests a rotation).

## In-memory types

```rust
/// One committed block, reduced to what block watch needs.
pub struct ObservedBlock {
    pub block_num: u32,
    pub committed_chain_tip: u32,
    pub updates: Vec<AccountUpdate>,
}

pub struct AccountUpdate {
    pub account_id: String,   // hex, validated through NetworkClient::validate_account_id
    pub commitment: String,   // word_to_hex(final_state_commitment)
    pub is_public: bool,      // AccountUpdateDetails::Public(_) present (phase 3 hook)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport { Stream, Poll }

pub enum BlockSourceError {
    Status { code: tonic::Code, message: String },  // terminal gRPC status
    Transport(String),                              // connect / decode failure
    OutOfSequence { expected: u32, got: u32 },
    Malformed(&'static str),                        // missing header/body/ids/words
}

/// Cursor policy outcome at (re)start.
pub enum StartPosition {
    Resume { from: u32 },                 // cursor + 1
    JumpToTip { tip: u32, gap: u64 },     // no cursor, gap too large, or blocks pruned
}

/// Recorded on the release write for the audit payload.
pub enum ReleaseOrigin {
    PushPath,
    Rotation,
    BlockWatch { observed_block_num: u32 },
}
```

## Dashboard shape (`GET /dashboard/info`, `backend.block_watch`)

```json
{
  "transport_mode": "auto",       // configured: "auto" | "stream" | "poll"
  "transport": "stream",          // "stream" | "poll" | null (not attached yet)
  "last_block": 574041,
  "chain_tip": 574041,
  "lag_blocks": 0,
  "pending_observations": 0,
  "poll_seconds": 3,
  "stall_seconds": 60,
  "max_catchup_blocks": 10000
}
```

`null` when `GUARDIAN_BLOCK_WATCH_ENABLED=false`. Values are the lease
holder's; a non-holder replica reports `enabled`, config, and the shared
`last_block` / `pending_observations` from the store, with `transport`,
`chain_tip`, `lag_blocks` `null` (it has no feed). Keep it simple: the
dashboard already reads leased-task state this way for stats (issue #371).
