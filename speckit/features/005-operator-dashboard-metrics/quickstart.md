# Quickstart: Operator Dashboard Metrics

This walks the happy path for each new endpoint after logging in via the
operator session flow established by `002-operator-auth`. All requests
carry the `guardian_operator_session` cookie established by the login
flow.

## 0. Prerequisites

- Running `guardian-server` (Postgres or filesystem backend).
- A signed-in operator session — see `002-operator-auth` quickstart.
- Run the Phase A migration
  (`2026-05-10-000001_promote_delta_status`) before starting the
  server. It promotes `status_kind` and `status_timestamp` to typed
  indexed columns on both `deltas` and `delta_proposals` and
  backfills them from the existing `status` `Jsonb` blob. Backfill is
  mandatory for existing deployments.

## 1. List accounts (paginated)

```text
GET /dashboard/accounts?limit=50
```

Expected response:

```jsonc
{
  "items": [ /* DashboardAccountSummary entries, up to 50 */ ],
  "next_cursor": "AaBbCc..."  // null at end of inventory
}
```

Notes:

- `limit` may be omitted; default is 50, max 500.
- Resume with `GET /dashboard/accounts?cursor=AaBbCc...` (re-supplying
  `limit` is optional — the default 50 applies).
- `total_count` is **not** returned. Aggregate inventory counts come
  from `/dashboard/info`.
- Tampered or foreign cursors are rejected with `400 InvalidCursor`.

## 2. Inventory and health summary

```text
GET /dashboard/info
```

Expected response:

```jsonc
{
  "service_status": "healthy",
  "environment": "testnet",
  "total_account_count": 1234,
  "latest_activity": "2026-05-08T12:34:56Z",
  "delta_status_counts": {
    "candidate":  7,
    "canonical": 8902,
    "discarded": 21
  },
  "in_flight_proposal_count": 12,
  "degraded_aggregates": []
}
```

Notes:

- On the filesystem backend with more than `filesystem_aggregate_threshold`
  accounts (default 1,000), some aggregates may appear in
  `degraded_aggregates`. Postgres serves all aggregates fully.
- `latest_activity` is `null` when the inventory has produced no
  deltas and has no in-flight proposals.

## 3. One account's delta history

```text
GET /dashboard/accounts/0x.../deltas?limit=50
```

Expected response (account exists, has history):

```jsonc
{
  "items": [
    {
      "nonce": 42,
      "status": "candidate",
      "status_timestamp": "2026-05-08T14:22:03Z",
      "prev_commitment": "0x7e8f9a0b1c2d...",
      "new_commitment":  "0xa3b4c5d6e7f8...",
      "retry_count": 3
    },
    {
      "nonce": 41,
      "status": "canonical",
      "status_timestamp": "2026-05-08T13:15:20Z",
      "prev_commitment": "0x6d7e8f9a0b1c...",
      "new_commitment":  "0x7e8f9a0b1c2d..."
    },
    {
      "nonce": 40,
      "status": "discarded",
      "status_timestamp": "2026-05-08T12:01:55Z",
      "prev_commitment": "0x6d7e8f9a0b1c...",
      "new_commitment":  null
    }
  ],
  "next_cursor": null
}
```

Notes:

- `retry_count` is always present on candidate entries (default 0).
- Pending entries do **not** appear here — they are exposed via
  `/dashboard/accounts/{id}/proposals`.
- Unknown `account_id` → `404 AccountNotFound`.
- Known account, no deltas yet → `200` with empty `items`.
- Underlying records unreadable → `503 DataUnavailable`.

## 4. One account's in-flight proposals

```text
GET /dashboard/accounts/0x.../proposals?limit=50
```

Expected response (multisig account with one in-flight proposal):

```jsonc
{
  "items": [
    {
      "commitment": "0xab12cd34ef567890...",
      "nonce": 48,
      "proposer_id": "0xfeed1234...",
      "originating_timestamp": "2026-05-08T14:18:50Z",
      "signatures_collected": 2,
      "signatures_required": 3,
      "prev_commitment": "0xa3b4c5d6e7f8...",
      "new_commitment":  "0xb4c5d6e7f809..."
    }
  ],
  "next_cursor": null
}
```

Notes:

- `commitment` is the proposal's cryptographic identifier; `nonce` is
  the per-account sequence number for the proposed state change.
- Single-key Miden accounts and EVM accounts always return an empty
  page — neither has rows in `delta_proposals` per FR-017.
- No raw signature bytes, no per-cosigner identity list (deferred per
  FR-020).

## 5. Global delta feed (smallest priority)

```text
GET /dashboard/deltas?limit=100&status=candidate,canonical
```

Expected response:

```jsonc
{
  "items": [
    {
      "nonce": 47,
      "account_id": "0x1234abcd...",
      "status": "candidate",
      "status_timestamp": "2026-05-08T14:22:03Z",
      "prev_commitment": "0x7e8f9a0b...",
      "new_commitment":  "0xa3b4c5d6...",
      "retry_count": 0
    },
    {
      "nonce": 12,
      "account_id": "0xabcd5678...",
      "status": "canonical",
      "status_timestamp": "2026-05-08T14:21:48Z",
      "prev_commitment": "0x5d6e7f80...",
      "new_commitment":  "0x6e7f8091..."
    }
  ],
  "next_cursor": "OTAwMA"
}
```

Notes:

- `status` accepts a comma-separated list. Default (no filter) returns
  all surfaced statuses. Unknown values → `400 InvalidStatusFilter`.
- Each entry carries `account_id` for grouping/linking client-side.
- On the filesystem backend above the threshold, the endpoint may
  return `503 DataUnavailable` with a clear reason (FR-029).

## 6. Global proposal feed (smallest priority)

```text
GET /dashboard/proposals?limit=100
```

Expected response:

```jsonc
{
  "items": [
    {
      "commitment": "0xab12cd34...",
      "nonce": 48,
      "account_id": "0x1234abcd...",
      "proposer_id": "0xfeed1234...",
      "originating_timestamp": "2026-05-08T14:18:50Z",
      "signatures_collected": 2,
      "signatures_required": 3,
      "prev_commitment": "0xa3b4c5d6...",
      "new_commitment":  "0xb4c5d6e7..."
    }
  ],
  "next_cursor": null
}
```

Notes:

- No `status` filter — all entries are in-flight by definition.
- EVM accounts do not appear here in v1 (FR-017).
- Empty inventory → `200` with empty `items`, never `404`.

## 7. Error matrix smoke test

| Action | Expected status / code |
|--------|------------------------|
| Any new endpoint without session cookie | `401 Unauthorized` |
| `GET /dashboard/accounts/{id}/deltas` for unknown id | `404 AccountNotFound` |
| `GET /dashboard/accounts?cursor=garbage` | `400 InvalidCursor` |
| `GET /dashboard/accounts?limit=9999` | `400 InvalidLimit` |
| `GET /dashboard/deltas?status=foo` | `400 InvalidStatusFilter` |
| Underlying delta store unreadable | `503 DataUnavailable` |

The `400` subtypes carry their stable `code` in the response body per
FR-028 — clients branch on `code`, not on `message`.
