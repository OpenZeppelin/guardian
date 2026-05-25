# Contract — `GET /dashboard/accounts/{account_id}/deltas`

**Status**: existing endpoint, response **extended** (additive only).

## Path

```
GET /dashboard/accounts/{account_id}/deltas
```

`{account_id}` — URL-encoded account identifier. Unchanged semantics.

## Query parameters

Unchanged from current behavior — `limit`, `cursor` only (per the `FeedQuery` struct at `crates/server/src/api/dashboard_feeds.rs:31`). There is intentionally **no** `status` filter on the per-account endpoint; the global endpoint has one (`GlobalDeltasQuery` at `:42`). Adding a per-account `status` filter is not in scope for this feature.

## Auth

Reuses existing `dashboard::authz` middleware (cookie session, `guardian_operator_session`, see `crates/server/src/dashboard/config.rs:7`). Today there is no per-account ACL: any authenticated operator with dashboard read access can list any configured account's deltas. Per-account ACL scoping is tracked separately (see spec §Edge Cases, "Operator authorization scope (v1)").

## Response — `200 OK`

```jsonc
{
  "items": [
    {
      // Pre-existing fields — UNCHANGED
      "nonce": 42,
      "status": "canonical",
      "status_timestamp": "2026-05-24T19:30:00Z",
      "prev_commitment": "0xaaaa...",
      "new_commitment":  "0xbbbb...",
      "proposal_type":   "p2id",

      // NEW (this feature)
      "category": "asset_transfer",
      "kind":     "p2id",
      "summary": {
        "asset": {
          "asset_id": "0xfaucet123...",
          "kind":     "fungible",
          "amount":   "-100"
        },
        "counterparty": {
          "account_id": "0xrecipient...",
          "direction":  "out"
        },
        "note_counts": { "input": 0, "output": 1 }
      }
    }
  ],
  "next_cursor": "..."
}
```

Examples per category (all share the same outer envelope):

- **Multisig `add_signer` (category = `account_storage_change`, kind = `add_signer`)**: `summary.asset = null`, `summary.counterparty = null`, `summary.note_counts = {input: 0, output: 0}`.
- **Single-key push p2id** (no metadata): `category = "asset_transfer"`, `kind = null`, `summary` populated by on-chain inference.
- **Unknown shape**: `category = "custom"`, `kind = null`, `summary.asset = null`, `summary.counterparty = null`, `note_counts` populated from the decoded `TransactionSummary`.

## Response — error cases

Identical to current endpoint. No new error shapes.

## Behavioural invariants (test these explicitly)

1. Every entry's `category` is a value of the closed enum and is never `null`. (SC-002)
2. `kind` is `null` for any entry whose underlying `DeltaObject.proposal_type()` returns `None`. (FR-002)
3. The first asset surfaced in `summary.asset` is deterministic across calls. (Decision 4 of research.md)
4. No previously-returned field is removed or renamed. (FR-021, SC-007) — existing TS tests at `packages/guardian-operator-client/src/http.test.ts:580+` continue to pass without modification.
5. Pagination (`cursor`, `limit`, `next_cursor`) behaviour is byte-identical to the pre-feature endpoint. (FR-005)
6. Ordering is `nonce DESC` — unchanged from current behavior (`crates/server/src/services/dashboard_account_deltas.rs`); since `nonce` is per-account monotonic, this is "newest-first" by construction.
