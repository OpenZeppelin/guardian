# Contract — `GET /dashboard/accounts/{account_id}/deltas/{nonce}`

**Status**: NEW endpoint.

## Path

```text
GET /dashboard/accounts/{account_id}/deltas/{nonce}
```

- `{account_id}` — URL-encoded account identifier. Same shape as elsewhere in the dashboard surface.
- `{nonce}` — canonical base-10 `u64`. `0` is allowed. No leading zeros except for the literal `"0"`. No negative numbers, no hex, no underscores.

## Query parameters

| Name | Values | Effect |
|------|--------|--------|
| `include` | `raw` | When set, the response includes `raw_transaction_summary`: a base64-encoded `TransactionSummary` blob for debugging. Omit the parameter to drop this field. |

`?include=scripts` (decoded note scripts) was considered and dropped — not implemented. Add via a coordinated wire-contract change if a use case appears.

## Auth

Reuses existing `dashboard::authz` middleware (cookie session, `guardian_operator_session`, see `crates/server/src/dashboard/config.rs:7`). Today there is no per-account ACL; any authenticated operator with dashboard read access can address any configured account. Per-account ACL is tracked separately. See SC-008 for what the v1 uniform-404 shape covers.

## Response — `200 OK`

```jsonc
{
  "account_id":       "0xacct...",
  "nonce":            42,
  "status":           "canonical",
  "status_timestamp": "2026-05-24T19:30:00Z",
  "prev_commitment":  "0xaaaa...",
  "new_commitment":   "0xbbbb...",
  // Typed metadata flattened to L1. Per-section arrays below carry
  // `asset` / `counterparty` / `note_counts` details, so those are
  // not duplicated as summary fields on the detail response.
  "category": "asset_transfer",
  "proposal": {
    "proposal_type":       "p2id",
    "recipient_id":        "0xrecipient...",
    "faucet_id":           "0xfaucet...",
    "amount":              "100",
    "required_signatures": 2
  },

  "input_notes": [],
  "output_notes": [
    {
      "note_id":   "0xnote1...",
      "tag":       "p2id",
      "assets":    [ { "asset_id": "0xfaucet...", "kind": "fungible", "amount": "100" } ],
      "recipient": "0xrecipient..."
    }
  ],

  "vault_changes": [
    { "asset_id": "0xfaucet...", "kind": "fungible", "change": "-100" }
  ],

  "storage_changes": [
    {
      "slot_name": "openzeppelin::multisig::threshold_config",
      "after": "0x0200..."
    }
  ]
}
```

**Storage changes (v1)**: each entry carries `slot_name` and post-change `after` only. The `before` field is **omitted** — a `TransactionSummary` delta does not include prior slot values. Populating `before` is a future enhancement tied to reading account storage at `prev_commitment`.


## Response — `400 Bad Request`

Returned when `{nonce}` fails to parse per the FR-009a / FR-018 constraints (negative, hex, leading-zero, non-decimal, etc.). Body uses `GuardianError::InvalidInput(_)` — the existing variant for unparseable inputs (`crates/server/src/error.rs:145`). No new error variant is added.

## Response — `404 Not Found`

Single uniform shape returned for both v1 not-found causes:
- `{nonce}` parses but no delta exists at `{account_id, nonce}` (`GuardianError::DeltaNotFound`),
- `{account_id}` is unknown to the server (`GuardianError::AccountNotFound`).

The two underlying variants today emit different `code` strings. To satisfy SC-008, the handler MUST normalize the response body so the two are field-level identical to callers (either by routing both to a single error or by post-processing the body). This is a contract requirement of the new detail endpoint; it does not change the behavior of the existing listing endpoints.

Per-account operator ACL is not in scope for v1; once added, the "unauthorized for this account" case shares the same shape (see FR-017).

## Behavioural invariants (test these explicitly)

1. The response's `nonce` equals the URL segment's `nonce` and the listing entry's `nonce` for the same delta. (FR-008)
2. The detail endpoint surfaces whatever `status` the delta currently has — no assumption that it is canonical. (Edge case: status transitions after listing.)
3. `input_notes`, `output_notes`, `vault_changes`, `storage_changes` are always present as arrays (possibly empty), never omitted, never null. (FR-011, US2-AS3)
4. Decoded notes never carry a `script` field. The raw `TransactionSummary` is **not** included by default; it appears as base64-encoded `raw_transaction_summary` only when the caller passes `?include=raw`.
6. If any section partially fails to decode, `decode_warnings[]` is present listing the failed sections, the request still returns `200`, and the other sections remain populated. (FR-016)
7. An unknown-account request returns a body that's field-level identical to the unknown-nonce case, even though the underlying `GuardianError` variants differ (SC-008). Verified by an integration test that diffs the two response bodies as `serde_json::Value`.
