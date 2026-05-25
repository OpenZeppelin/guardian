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

      // NEW (this feature) — typed metadata fields flattened to L1.
      // Each field is omitted when absent (no `null` placeholders).
      "category": "asset_transfer",
      "proposal_type": "p2id",
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
  ],
  "next_cursor": "..."
}
```

The full `proposal` block (recipient_id, faucet_id, amount, required_signatures, target_threshold, signer_commitments, …) lives on the **detail endpoint**, not on listing rows. Listing carries only the lightweight `proposal_type` tag.

Examples per category:

- **Multisig `add_signer`**: `category = "account_storage_change"`, `proposal_type = "add_signer"`, `asset` absent, `counterparty` absent, `note_counts` absent (both input/output are zero).
- **Single-key push p2id** (no proposal): `category = "asset_transfer"`, `proposal_type` absent. `asset` populated from the first output note's first asset; `counterparty` stays absent for single-key push.
- **Pre-feature-007 row / EVM**: all enrichment fields (`category`, `proposal_type`, `asset`, `counterparty`, `note_counts`) absent. Listing entry still returned with `nonce`, `status`, commitments intact.

## Response — error cases

Identical to current endpoint. No new error shapes.

## Behavioural invariants (test these explicitly)

1. When `category` is present it is a value of the closed enum and is never `null`. (SC-002)
2. `proposal_type` is absent for any entry without a matching multisig proposal (single-key push, EVM, pre-feature-007 historical row).
3. The first asset surfaced in `asset` is deterministic across calls — derived from the proposal's typed metadata for `p2id` multisig, or from the first output note for single-key push.
4. All enrichment fields are **absent** (key omitted) for rows whose `delta_payload` is undecodable AND no matching proposal exists (EVM bridge, pre-feature-007 historical). Clients render this as "metadata unavailable" — they MUST NOT fabricate placeholder field values that would contradict actual on-chain activity.
5. `note_counts` is absent when both `input` and `output` are zero, matching the skip-when-empty rule applied to `asset` and `counterparty`.
6. Pagination (`cursor`, `limit`, `next_cursor`) behaviour is byte-identical to the pre-feature endpoint. (FR-005)
7. Ordering is `nonce DESC` — unchanged from current behavior (`crates/server/src/services/dashboard_account_deltas.rs`); since `nonce` is per-account monotonic, this is "newest-first" by construction.
