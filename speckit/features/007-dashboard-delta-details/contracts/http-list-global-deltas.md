# Contract — `GET /dashboard/deltas`

**Status**: existing endpoint, response **extended** (additive only). Cross-account variant of `/dashboard/accounts/{account_id}/deltas`.

## Path

```text
GET /dashboard/deltas
```

## Query parameters

Unchanged: `limit`, `cursor`, `status` (comma-separated filter). See pre-existing tests at `crates/server/src/api/dashboard_feeds.rs:561+`.

## Auth

Reuses existing `dashboard::authz` middleware (cookie session, `guardian_operator_session`). Today the endpoint returns deltas across **all configured accounts** — there is no per-account ACL filter (see `list_global_deltas` signature at `crates/server/src/services/dashboard_global_deltas.rs:147`). Adding per-account ACL is tracked separately and is not in scope for this feature.

## Response — `200 OK`

Each entry is a `DashboardGlobalDeltaEntry` — a `DashboardDeltaEntry` (see [http-list-account-deltas.md](./http-list-account-deltas.md)) with an additional `account_id` field at the top of the object.

```jsonc
{
  "items": [
    {
      "account_id":       "0xacct...",
      // ...all DashboardDeltaEntry fields, flattened to L1...
      "category": "asset_transfer",
      "assets": [{ "asset_id": "0xfaucet...", "kind": "fungible", "amount": "-25" }],
      "note_counts": { "input": 1, "output": 1 }
      // no proposal_type — single-key push or non-multisig source
    }
  ],
  "next_cursor": "..."
}
```

## Behavioural invariants (test these explicitly)

1. Every entry includes a non-null `account_id`. The enrichment fields (`category`, `proposal_type`, `assets`, `counterparty`, `note_counts`) are all optional and MAY be absent — EVM deltas and pre-feature-007 historical rows whose source proposal was already deleted carry none of them. When `category` is present it MUST be non-null and a member of the closed enumeration (SC-002). The server MUST NOT fabricate enrichment fields at read time.
2. The shape inside each entry (excluding the leading `account_id`) is byte-identical to the per-account listing's entry shape. The TS operator client parses both via the same `parseDeltaEntry` function (`packages/guardian-operator-client/src/http.ts:1257`).
3. Cross-account ordering / pagination behaviour is unchanged from current `dashboard_global_deltas` semantics.
4. Status filter (`?status=candidate,canonical`) keeps current semantics; no new filterable fields are added in this feature.
5. Operator authorization scope is unchanged from current behavior — any authenticated operator with dashboard read access sees deltas for all configured accounts. Per-account ACL filtering is not in scope for v1; see the spec edge case "Operator authorization scope (v1)".
