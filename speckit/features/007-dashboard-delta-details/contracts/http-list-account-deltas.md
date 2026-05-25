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

      // NEW (this feature) — typed metadata blob derived at push time
      "metadata": {
        "category": "asset_transfer",
        "asset": {
          "asset_id": "0xfaucet123...",
          "kind":     "fungible",
          "amount":   "-100"
        },
        "counterparty": {
          "account_id": "0xrecipient...",
          "direction":  "out"
        },
        "note_counts": { "input": 0, "output": 1 },
        "proposal": {
          "proposal_type":       "p2id",
          "recipient_id":        "0xrecipient...",
          "faucet_id":           "0xfaucet123...",
          "amount":              "100",
          "required_signatures": 2
        }
      }
    }
  ],
  "next_cursor": "..."
}
```

Examples per category (all share the same outer envelope):

- **Multisig `add_signer`**: `metadata.category = "account_storage_change"`, `metadata.proposal.proposal_type = "add_signer"`, `metadata.asset = absent`, `metadata.counterparty = absent`, `metadata.note_counts = {input: 0, output: 0}`. The `metadata.proposal` block carries `target_threshold` + `signer_commitments`.
- **Single-key push p2id** (no proposal): `metadata.category = "asset_transfer"`, `metadata.proposal = absent`. `metadata.asset` populated from the first output note's first asset; `metadata.counterparty` stays absent for single-key push.
- **Pre-feature-007 row / EVM**: `metadata = absent`. Listing entry still returned with `nonce`, `status`, commitments intact.

## Response — error cases

Identical to current endpoint. No new error shapes.

## Behavioural invariants (test these explicitly)

1. When `metadata` is present, `metadata.category` is a value of the closed enum and is never `null`. (SC-002)
2. `metadata.proposal` is absent for any entry without a matching multisig proposal (single-key push, EVM, pre-feature-007 historical row). Verified by the proposal-lookup-miss path in `services/push_delta.rs` integration tests.
3. The first asset surfaced in `metadata.asset` is deterministic across calls — derived from the proposal's typed metadata for `p2id` multisig, or from the first output note for single-key push.
4. `metadata` is **absent** (key omitted) for rows whose `delta_payload` is undecodable AND no matching proposal exists (EVM bridge, pre-feature-007 historical). Clients render this as "metadata unavailable" — they MUST NOT fabricate placeholder field values that would contradict actual on-chain activity.
5. Pagination (`cursor`, `limit`, `next_cursor`) behaviour is byte-identical to the pre-feature endpoint. (FR-005)
6. Ordering is `nonce DESC` — unchanged from current behavior (`crates/server/src/services/dashboard_account_deltas.rs`); since `nonce` is per-account monotonic, this is "newest-first" by construction.
