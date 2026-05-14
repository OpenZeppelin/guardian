# Data Model: Operator Dashboard Metrics — Pagination, Info, and Activity

## Persistence Changes

**Phase A schema migration** (`crates/server/migrations/2026-05-10-000001_promote_delta_status/`)
promotes two derived fields from the existing `status` `Jsonb` blob
into typed, indexed columns on both `deltas` and `delta_proposals`:

- `status_kind text NOT NULL` — one of `pending` / `candidate` /
  `canonical` / `discarded`. `CHECK` constraint enforces the four
  values per table.
- `status_timestamp timestamptz NOT NULL` — wall-clock entry-into-status
  time, used as the global-feed sort key.

The `up.sql` adds both columns nullable, runs the backfill (`UPDATE
… SET status_kind = status->>'status', status_timestamp =
(status->>'timestamp')::timestamptz`), then `SET NOT NULL`, then
adds the `CHECK` constraint and the composite indexes:

- `(status_kind, status_timestamp DESC, account_id, id)` on each
  table — supports the global delta and proposal feeds' `WHERE
  status_kind = ANY($1) AND (status_timestamp, account_id, id) <
  ($cursor_ts, $cursor_account, $cursor_id) ORDER BY status_timestamp
  DESC, account_id ASC, id ASC LIMIT n` pattern.

`down.sql` drops the indexes, the check constraint, and the columns
with `IF EXISTS` clauses; safe to rerun.

Backfill is mandatory because existing Guardian deployments already
have populated rows in both tables. The Postgres backend dual-writes
both the `status` `Jsonb` blob and the typed columns from a single
`derive_status_columns(&DeltaStatus)` helper so the two
representations cannot drift.

Per-account history endpoints continue to sort by the immutable
primary key (`delta.id DESC` / `delta_proposal.id DESC`); the typed
columns are added specifically for the global feeds and the info
endpoint's `MAX(status_timestamp)` aggregation.

The only persistence-adjacent service-layer change is a server config
field for the filesystem-backend degradation threshold.

### Server Config Addition

`crates/server/src/config.rs` (or equivalent dashboard config struct):

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `filesystem_aggregate_threshold` | `usize` | `1000` | Above this account count, the filesystem backend returns a degraded marker on cross-account aggregates per FR-029. |

## Read Models (Wire Shapes)

All response bodies share the same envelope and field-name conventions
established by the spec's Field Glossary.

### `PagedResult<T>` envelope

```jsonc
{
  "items": [/* T entries */],
  "next_cursor": "string|null"  // null → end of list
}
```

`total_count` is intentionally absent from any paginated endpoint;
aggregate counts live on `/dashboard/info` only.

### `DashboardAccountSummary` (existing entry shape, preserved)

Per `crates/server/src/services/dashboard_accounts.rs`. The existing
fields (`account_id`, `auth_scheme`, `authorized_signer_count`,
`has_pending_candidate`, `current_commitment`, `state_status`,
`created_at`, `updated_at`) are unchanged. New fields MAY be added in
this feature without renaming or removing existing ones (FR-006).

### `DashboardInfoResponse` (new)

```jsonc
{
  "service_status": "healthy" | "degraded",
  "environment": "mainnet" | "testnet" | "<custom>",
  "total_account_count": 0,
  "latest_activity": "2026-05-08T12:34:56Z" | null,
  "delta_status_counts": {
    "candidate":  0,
    "canonical":  0,
    "discarded":  0
  },
  "in_flight_proposal_count": 0,
  "degraded_aggregates": []  // names of aggregates marked degraded per FR-029
}
```

The response intentionally omits any per-network account count or a
singular "the network" field. In practice an instance is gated to one
network family (Miden default; EVM behind a server feature flag), and
the dashboard knows its own deployment context. Per-account network
type can be derived from the account list if needed.

### `DashboardDeltaEntry` (new)

```jsonc
{
  "nonce": 47,
  "account_id": "<canonical account id>",   // present on global feed only
  "status": "candidate" | "canonical" | "discarded",
  "status_timestamp": "2026-05-08T12:34:56Z",
  "prev_commitment": "0x7e8f9a0b...",
  "new_commitment": "0xa3b4c5d6..." | null,
  "retry_count": 0   // present on candidate entries; default 0
}
```

- `nonce` is the per-account integer sequence number used as the
  human-readable identifier in dashboard tables.
- `account_id` is present only on the global feed.
- `prev_commitment` is the state commitment the delta was applied
  against; `new_commitment` is the resulting commitment, nullable for
  entries that did not produce one (e.g. a discarded delta).
- `retry_count` is omitted on canonical/discarded entries and always
  present (default `0`) on candidate entries.

### `DashboardProposalEntry` (new)

```jsonc
{
  "commitment": "0xab12cd34...",
  "nonce": 48,
  "account_id": "<canonical account id>",   // present on global feed only
  "proposer_id": "0x<hex>",
  "originating_timestamp": "2026-05-08T12:34:56Z",
  "signatures_collected": 0,
  "signatures_required": 0,
  "prev_commitment": "0xa3b4c5d6...",
  "new_commitment": "0xb4c5d6e7..." | null
}
```

- `commitment` is the proposal's cryptographic identifier (what
  cosigners are signing); it is the per-account stable identifier.
- `nonce` is the per-account integer sequence number for the proposed
  state change.
- `prev_commitment` is the state commitment the proposal applies
  against; `new_commitment` is the resulting commitment, nullable.

`signatures_required` is derived from the account's auth policy at
read time per FR-019:

- `Auth::MidenFalconRpo { cosigner_commitments }` →
  `cosigner_commitments.len()`
- `Auth::MidenEcdsa { cosigner_commitments }` →
  `cosigner_commitments.len()`
- `Auth::EvmEcdsa { signers }` → never reached; EVM accounts return an
  empty proposal queue per FR-017.

No raw signature bytes, no per-cosigner identity list (FR-020).

### Error Body (all new endpoints, applied via FR-028)

```jsonc
{
  "error": {
    "code": "Unauthorized" | "AccountNotFound" | "InvalidCursor"
          | "InvalidLimit" | "InvalidStatusFilter" | "DataUnavailable",
    "message": "<human-readable>",
    "details": { /* optional, code-specific */ }
  }
}
```

The `code` field is stable and machine-readable. Clients branch on
`code`, not on `message` or HTTP status alone.

## Cursor Codec

Cursors are opaque base64url-encoded payloads HMAC-signed with a
server secret from the dashboard config block. The signed payload is
internal — clients MUST treat the string as opaque per FR-005.

```text
cursor_bytes = base64url_decode(cursor_string)
payload      = cursor_bytes[..-32]
sig          = cursor_bytes[-32..]
require hmac_sha256(secret, payload) == sig

payload = bincode_encode({
  kind: AccountList | AccountDeltas | AccountProposals
      | GlobalDeltas | GlobalProposals,
  sort_keys: <kind-specific tuple of (timestamp, ...)>,
  last_id: i64    // delta.id or delta_proposal.id from the last page entry
                  // (also covers AccountList tiebreaker as last_account_id: String)
})
```

For each kind:

| Kind | Sort key (newest first) | Tiebreaker | Mutable? |
|------|-------------------------|------------|----------|
| AccountList | `updated_at DESC` | `account_id ASC` | yes (sort key) — FR-005 caveat applies |
| AccountDeltas | `delta.id DESC` | none needed (PK is unique) | no — fully stable |
| AccountProposals | `delta_proposal.id DESC` | none needed | no — fully stable |
| GlobalDeltas | `status_timestamp DESC` (with optional `status_kind = ANY($1)` filter) | `account_id ASC`, then `id ASC` | yes (sort key on status transitions) — FR-005 caveat applies |
| GlobalProposals | `status_timestamp DESC` | `account_id ASC`, then `id ASC` | originating timestamp is immutable while the proposal is in-flight, so traversal is stable for the lifetime of the queue entry |

Per-account `delta.id` and `delta_proposal.id` are Postgres-assigned
`bigserial` PKs that grow monotonically on insert, so `id DESC`
gives newest-first ordering and the position of any returned entry
never changes after insert. This is what makes cursor traversal
fully stable for the per-account kinds.

The two global feeds sort by the typed `status_timestamp` column
(promoted by the Phase A migration); they use `(status_timestamp,
account_id, id)` as the cursor predicate to walk the
`(status_kind, status_timestamp DESC, account_id, id)` composite
index in a single seek.

A cursor is rejected with `400 InvalidCursor` when:

- HMAC verification fails (tampered/foreign secret).
- The decoded `kind` does not match the endpoint receiving it.
- The referenced row no longer exists at the resumed position.

There is no TTL field; cursor staleness is detected via the row-existence
check above.

## Lifecycle and Read-Path Rules

- **Read-only**. No endpoint mutates `account_metadata`, `states`,
  `deltas`, or `delta_proposals` (FR-026).
- **Status surfaces are direct**. The response `status` field is the
  canonical lifecycle status read from the typed `status_kind` column
  promoted by the Phase A migration (the same value that is also
  preserved in the `status` `Jsonb` blob). The feature does not
  collapse, rename, or merge statuses.
- **Per-account endpoints are scoped**. The path account ID is the
  authoritative scope; queries MUST filter by it (FR-023). One
  account's read MUST NOT leak entries belonging to another account.
- **Filesystem degradation is explicit**. When the configured threshold
  is exceeded, the affected aggregate is marked in
  `DashboardInfoResponse.degraded_aggregates` (or the global feed
  endpoint returns `503 DataUnavailable` with a clear reason). No
  endpoint silently substitutes a zero count.
- **Cross-network and EVM behavior**:
  - The info response does not enumerate networks (no per-network
    counts and no singular network field per FR-009).
  - EVM accounts always return an empty proposal queue and do not
    appear in the global proposal feed (FR-017, Decision 8 in
    `research.md`).
- **Backend parity** holds for per-account endpoints; cross-account
  aggregates have the FR-029 exception. Both backends use the same
  service-layer code paths and the same response shapes; only the
  filesystem backend's aggregate path may short-circuit to a degraded
  marker.
