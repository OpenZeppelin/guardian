# Phase 1 Data Model — Dashboard delta activity feed and detail view

Source-of-truth for the wire shapes returned by the listing endpoints and persisted in the `deltas.metadata` JSONB column. The server's Rust types in `crates/server/src/delta_summary/mod.rs` mirror these names verbatim; the TS operator client's types in `packages/guardian-operator-client/src/types.ts` mirror them via camelCase. **Schema change**: one new nullable JSONB column on `deltas` (migration `2026-05-25-000001_delta_metadata`).

## Enums

### `DashboardDeltaStatus` (existing — unchanged)

```text
"candidate" | "canonical" | "discarded"
```

Already defined at `crates/server/src/services/dashboard_account_deltas.rs:29-33`. `pending` is intentionally excluded — those live on the proposal feed (FR-006).

### `DashboardDeltaCategory` (new — FR-002)

Closed enumeration. Adding a value is a wire-contract change.

```text
"asset_transfer"
"note_consumption"
"note_creation"
"account_storage_change"
"guardian_switch"
"custom"
```

`asset_swap` was originally listed but is **not** in the v1 enum: detecting an atomic swap requires per-output-note tag inspection (matching the Miden `pswap` note tag's use-case constant) which is deferred. Shipping the variant before the detection lands would mean emitting a wire-contract value that's never used. When the detection lands, `asset_swap` returns as a coordinated wire-contract update across the server enum, TS types, and operator UI.

Mapping rules at push time (see `crates/server/src/delta_summary/category.rs`):

| Source signal | Resulting `category` |
|---|---|
| `proposal.proposal_type == "p2id"` | `asset_transfer` |
| `proposal.proposal_type == "consume_notes"` | `note_consumption` |
| `proposal.proposal_type == "switch_guardian"` | `guardian_switch` |
| `proposal.proposal_type ∈ {add_signer, remove_signer, change_threshold, update_procedure_threshold}` | `account_storage_change` |
| No matching proposal, both input and output notes | `asset_transfer` |
| No matching proposal, only input notes | `note_consumption` |
| No matching proposal, only output notes | `note_creation` |
| No matching proposal, no notes (account-state-only change) | `account_storage_change` |
| Any unknown `proposal_type`, or no decodable `TransactionSummary` and no proposal | `custom` (or `metadata: NULL` when even the summary is undecodable) |

## Entities

### `DeltaMetadata` (new — persisted in `deltas.metadata` JSONB)

Built once at push time by `delta_summary::build_metadata`. The wire shape exposed by listings is the same shape as the persisted column — serde does the round-trip.

```jsonc
{
  // Always present when metadata is non-null:
  "category": "asset_transfer" | "note_consumption" | "note_creation" | "account_storage_change" | "guardian_switch" | "custom",
  "asset":         { "asset_id": "0x...", "kind": "fungible" | "non_fungible", "amount": "-100" } | absent,
  "counterparty":  { "account_id": "0x...", "direction": "in" | "out" } | absent,
  "note_counts":   { "input": 0, "output": 1 },

  // Multisig pushes only:
  "proposal": {
    "proposal_type":              "p2id" | "add_signer" | ... (open-ended string),
    "description":                "..." | absent,
    "salt":                       "0x..." | absent,
    "required_signatures":        2 | absent,
    "recipient_id":               "0x..." | absent,      // p2id
    "faucet_id":                  "0x..." | absent,      // p2id
    "amount":                     "100"  | absent,       // p2id
    "note_ids":                   ["0x..."] | absent,    // consume_notes
    "consume_notes_metadata_version": 2 | absent,        // consume_notes
    "consume_notes_notes":        ["<base64>"] | absent, // consume_notes
    "target_threshold":           2 | absent,            // add_signer / remove_signer / change_threshold
    "signer_commitments":         ["0x..."] | absent,    // (same)
    "new_guardian_pubkey":        "0x..." | absent,      // switch_guardian
    "new_guardian_endpoint":      "https://..." | absent, // switch_guardian
    "target_procedure":           "..." | absent          // update_procedure_threshold
  } | absent
}
```

The column is **NULL** for:
- EVM deltas whose `delta_payload` is not a `TransactionSummary`
- Pre-feature-007 historical rows (never reprocessed)

### `DashboardDeltaEntry` (listing — extended)

Returned by `GET /dashboard/accounts/{account_id}/deltas` and (with `account_id` added) `GET /dashboard/deltas`. **All fields prior to this feature remain present and unchanged**. The enrichment fields are spread directly at L1 (no nested `metadata` envelope on the wire); the persisted `deltas.metadata` JSONB column is an internal implementation detail. See spec §Clarifications 2026-05-25.

```text
{
  // — Pre-existing fields (unchanged) —
  nonce:              u64                        // Per-account monotonically increasing.
  status:             DashboardDeltaStatus
  status_timestamp:   string                     // RFC 3339
  prev_commitment:    string                     // hex Word
  new_commitment:     string | null              // hex Word; null for non-canonical
  retry_count?:       u32                        // present on candidate; omitted otherwise

  // — Enrichment fields (this feature, spread at L1, all optional) —
  category?:          DashboardDeltaCategory     // omitted for EVM and pre-feature-007 historical rows
  proposal_type?:     string                     // multisig intent tag (e.g. "p2id", "consume_notes")
  assets?:            DashboardDeltaAssetSummary[]
  counterparty?:      DashboardDeltaCounterparty
  note_counts?:       NoteCounts                 // omitted when both input and output are zero
}
```

The global feed entry adds `account_id: string` at the top.

### `DashboardDeltaDetail` (detail endpoint — Phase 4 / US2 scaffolding)

Detail endpoint construction stays in scaffold form for Phase 4; once implemented it will read `metadata` plus the persisted `TransactionSummary` to project the full detail view (decoded input/output notes, vault changes, storage changes) per the contract in `contracts/http-get-account-delta-detail.md`.

## Error shapes

No new error variants. The push pipeline never fails the push because of metadata derivation — `metadata: NULL` is the safe fallback for any payload it can't decode.

## Notes on Rust type surface

```text
crates/server/src/delta_summary/
    pub struct DeltaMetadata { ... }                      // top-level JSONB shape
    pub struct ProposalMetadata { ... }                   // mirrors ProposalMetadataPayload
    pub enum DashboardDeltaCategory { ... }
    pub struct AssetSummary { ... }
    pub struct CounterpartySummary { ... }
    pub struct NoteCounts { ... }

    pub fn build_metadata(
        delta_payload: &serde_json::Value,
        matching_proposal_payload: Option<&serde_json::Value>,
    ) -> Option<DeltaMetadata>;

    pub fn metadata_from_value(value: serde_json::Value) -> Option<DeltaMetadata>;
    pub fn metadata_to_value(metadata: &DeltaMetadata) -> serde_json::Value;

crates/server/src/services/push_delta.rs
    // Calls build_metadata at push time, persists the result on
    // result_delta.metadata before submit_delta.

crates/server/src/services/dashboard_account_deltas.rs
crates/server/src/services/dashboard_global_deltas.rs
    // Both projections read delta.metadata.clone() directly. No
    // classifier call on the read path.
```

All structs derive `Serialize` + `Deserialize` so serde handles the JSONB round-trip transparently.
