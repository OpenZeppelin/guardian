# Contract — `GET /dashboard/accounts/{account_id}/deltas`

**Status**: existing endpoint, response **extended** (additive only).

## Path

```text
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
      "assets": [
        {
          "asset_id": "0xfaucet123...",
          "kind":     "fungible",
          "amount":   "-100"
        }
      ],
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

`assets` is **an array** — every extractable asset from the transaction's notes is included so multi-asset transactions are not collapsed to a misleading single entry. The order is deterministic (output notes first in note order, then asset order within each note; falls back to input-note order when no outputs carry assets). Multi-asset transactions therefore look like `assets: [{...}, {...}, ...]` with no truncation. Clients pick their own display strategy (render all, truncate to N, etc.). When no asset can be extracted the field is omitted.

Examples per category:

- **Multisig `add_signer`**: `category = "account_storage_change"`, `proposal_type = "add_signer"`, `assets` absent, `counterparty` absent, `note_counts` absent (both input/output are zero).
- **Single-key push p2id** (no proposal): `category = "asset_transfer"`, `proposal_type` absent. `assets` populated from each output note's assets; `counterparty` stays absent for single-key push.
- **Multi-asset transfer**: `assets` carries one entry per `(note, asset)` pair. The detail endpoint's `vault_changes[]` is the canonical full breakdown if the client needs per-note attribution.
- **Pre-feature-007 row / EVM**: all enrichment fields (`category`, `proposal_type`, `assets`, `counterparty`, `note_counts`) absent. Listing entry still returned with `nonce`, `status`, commitments intact.

## Response — error cases

Identical to current endpoint. No new error shapes.

## Behavioural invariants (test these explicitly)

1. When `category` is present it is a value of the closed enum and is never `null`. (SC-002)
2. `proposal_type` is absent for any entry without a matching multisig proposal (single-key push, EVM, pre-feature-007 historical row).
3. `assets` is an array carrying every extractable asset entry from the transaction's output notes (or input notes when no outputs carry assets). The ordering is deterministic across calls. Single-asset transactions look like `assets: [{...}]`; multi-asset transactions carry one entry per `(note, asset)` pair.
4. All enrichment fields are **absent** (key omitted) for rows whose `delta_payload` is undecodable AND no matching proposal exists (EVM bridge, pre-feature-007 historical). Clients render this as "metadata unavailable" — they MUST NOT fabricate placeholder field values that would contradict actual on-chain activity.
5. `note_counts` is absent when both `input` and `output` are zero, and `assets` is absent when empty — same skip-when-empty rule applied uniformly to all enrichment fields.
6. Pagination (`cursor`, `limit`, `next_cursor`) behaviour is byte-identical to the pre-feature endpoint. (FR-005)
7. Ordering is `nonce DESC` — unchanged from current behavior (`crates/server/src/services/dashboard_account_deltas.rs`); since `nonce` is per-account monotonic, this is "newest-first" by construction.
