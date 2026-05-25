# Contract — `GET /dashboard/accounts/{account_id}/deltas/{nonce}`

**Status**: NEW endpoint.

## Path

```
GET /dashboard/accounts/{account_id}/deltas/{nonce}
```

- `{account_id}` — URL-encoded account identifier. Same shape as elsewhere in the dashboard surface.
- `{nonce}` — canonical base-10 `u64`. `0` is allowed. No leading zeros except for the literal `"0"`. No negative numbers, no hex, no underscores.

## Query parameters

A single `include` query parameter accepts a comma-separated list of opt-in features:

| Value in `include=` | Effect |
|---|---|
| `scripts` | Each decoded note carries an additional `script` field (hex MAST). (Decision 5 in `research.md`, FR-012.) |
| `raw` | Response carries the top-level `raw_transaction_summary` field (base64 of the persisted `TransactionSummary`). Debug only. (FR-015.) |
| anything else | Ignored, for forwards compatibility. |

Multiple values may be combined: `?include=scripts,raw`. Default (parameter unset): both fields are absent.

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
  "metadata": {
    "category":     "asset_transfer",
    "asset":        { "asset_id": "0xfaucet...", "kind": "fungible", "amount": "-100" },
    "counterparty": { "account_id": "0xrecipient...", "direction": "out" },
    "note_counts":  { "input": 0, "output": 1 },
    "proposal": {
      "proposal_type":       "p2id",
      "recipient_id":        "0xrecipient...",
      "faucet_id":           "0xfaucet...",
      "amount":              "100",
      "required_signatures": 2
    }
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

  "storage_changes": []
}
```

When `?include=scripts` is set, each decoded note carries an additional `script` field (hex MAST).

When `?include=raw` is set (debug only), the response carries `raw_transaction_summary` (base64).

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
4. `script` is absent unless `include=scripts` was requested. (Decision 5 of research.md)
5. `raw_transaction_summary` is absent unless `include=raw` was requested. (FR-015)
6. If any section partially fails to decode, `decode_warnings[]` is present listing the failed sections, the request still returns `200`, and the other sections remain populated. (FR-016)
7. An unknown-account request returns a body that's field-level identical to the unknown-nonce case, even though the underlying `GuardianError` variants differ (SC-008). Verified by an integration test that diffs the two response bodies as `serde_json::Value`.
