# Phase 1 Data Model — Dashboard delta activity feed and detail view

Source-of-truth for the wire shapes returned by the three endpoints. The server's Rust types mirror these names verbatim; the TS operator client's types mirror them via `server-types.ts`. Read-only — no persistence changes.

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
"asset_swap"
"note_consumption"
"note_creation"
"account_storage_change"
"guardian_switch"
"custom"
```

Mapping rules:

| Source signal | Resulting `category` |
|---|---|
| `metadata.proposal_type == "p2id"` | `asset_transfer` |
| `metadata.proposal_type == "consume_notes"` | `note_consumption` |
| `metadata.proposal_type == "switch_guardian"` | `guardian_switch` |
| `metadata.proposal_type ∈ {add_signer, remove_signer, change_threshold, update_procedure_threshold}` | `account_storage_change` |
| No `metadata.proposal_type`, output notes include `pswap` | `asset_swap` |
| No `metadata.proposal_type`, output notes include `p2id`/`p2ide` | `asset_transfer` |
| No `metadata.proposal_type`, only input notes (no outputs) | `note_consumption` |
| No `metadata.proposal_type`, only output notes (no inputs) | `note_creation` |
| No `metadata.proposal_type`, only `AccountDelta` (no notes) | `account_storage_change` |
| Any other shape, or partial-decode failure | `custom` |

## Entities

### `DashboardDeltaEntry` (listing — extended)

Returned by `GET /dashboard/accounts/{account_id}/deltas` and (with `account_id` added) `GET /dashboard/deltas`. **All fields prior to this feature remain present and unchanged**; new fields are additive (FR-021).

```text
{
  // — Pre-existing fields (unchanged) —
  nonce:              u64                        // Per-account monotonically increasing.
  status:             DashboardDeltaStatus
  status_timestamp:   string                     // RFC 3339
  prev_commitment:    string                     // hex Word
  new_commitment:     string | null              // hex Word; null for non-canonical
  retry_count?:       u32                        // present on candidate; omitted otherwise
  proposal_type:      string | null              // Already exposed; kept for backwards compat. Equals `kind` below.

  // — New fields (this feature) —
  category:           DashboardDeltaCategory     // Always present, never null
  kind:               string | null              // Echoes metadata.proposal_type when present
  summary:            DeltaActivitySummary       // Always present; fields inside may be null
}
```

`proposal_type` is preserved as-is so existing TS callers don't regress; `kind` is the new canonical name and is always equal to `proposal_type` for multisig deltas. A future contract change may deprecate `proposal_type` in favor of `kind`; that is out of scope here.

The global feed wraps this with `account_id`:

```text
{
  account_id: string,
  ...DashboardDeltaEntry
}
```

This mirrors the existing `DashboardGlobalDeltaEntry` shape from `crates/server/src/services/dashboard_global_deltas.rs`.

### `DeltaActivitySummary` (new)

Per-entry derived fields. Each sub-field is null when not extractable (FR-004).

```text
{
  asset?: {
    asset_id:  string                            // hex faucet id (fungible) or hex asset id (non-fungible)
    kind:      "fungible" | "non_fungible"
    amount?:   string                            // signed decimal (e.g., "+100", "-50"); fungible only
  } | null

  counterparty?: {
    account_id: string                            // recipient or sender, depending on direction
    direction:  "out" | "in"
  } | null

  note_counts: {
    input:  u32
    output: u32
  }
}
```

When multiple assets are involved, the listing surfaces the **first** asset (deterministic ordering: fungible-by-faucet-id, then non-fungible-by-asset-id) and the detail endpoint exposes the full list. The listing is not the place to enumerate every asset.

### `DashboardDeltaDetail` (new — detail endpoint response)

Returned by `GET /dashboard/accounts/{account_id}/deltas/{nonce}`.

```text
{
  account_id:        string
  nonce:             u64
  status:            DashboardDeltaStatus
  status_timestamp:  string
  prev_commitment:   string
  new_commitment:    string | null
  category:          DashboardDeltaCategory
  kind:              string | null
  summary:           DeltaActivitySummary

  input_notes:       DecodedNote[]              // Possibly empty; never null
  output_notes:      DecodedNote[]              // Possibly empty; never null
  vault_changes:     VaultChange[]              // Possibly empty
  storage_changes:   StorageChange[]            // Possibly empty

  decode_warnings?:  DecodeWarning[]            // Present iff any partial-decode occurred (FR-016)
  raw_transaction_summary?: string              // base64 of the persisted TransactionSummary; debug only (FR-015)
}
```

### `DecodedNote` (new)

```text
{
  note_id:       string                          // hex NoteId
  tag:           "p2id" | "p2ide" | "pswap" | "mint" | "burn" | "custom"
  assets:        DecodedAsset[]                  // possibly empty
  sender?:       string                          // counterparty account id
  recipient?:    string                          // counterparty account id
  script?:       string                          // hex MAST; only present when ?include=scripts is set
}
```

### `DecodedAsset` (new)

```text
{
  asset_id: string
  kind:     "fungible" | "non_fungible"
  amount?:  string                               // u64 decimal (unsigned in note context — direction is implied)
}
```

### `VaultChange` (new)

Fungible:

```text
{
  asset_id: string,
  kind:     "fungible",
  change:   string                               // signed decimal: "+100" / "-50"
}
```

Non-fungible:

```text
{
  asset_id: string,                              // faucet id of the non-fungible collection
  kind:     "non_fungible",
  added:    string[],                            // asset ids gained
  removed:  string[]                             // asset ids burned/transferred
}
```

### `StorageChange` (new)

```text
{
  slot_index: u32,
  before:     string | null,                     // hex Word; null for previously-unset
  after:      string | null                      // hex Word; null when the slot was cleared
}
```

### `DecodeWarning` (new)

```text
{
  section: "tx_summary" | "metadata" | "input_notes" | "output_notes" | "vault" | "storage"
  reason:  string                                // short machine-readable token, e.g., "malformed_base64", "unknown_proposal_type"
}
```

## Error shapes

Reused; no new error variants are introduced.

| Condition | Status | Variant (verified in `crates/server/src/error.rs:131–152`) |
|---|---|---|
| Malformed `nonce` segment (FR-018) | `400 Bad Request` | `GuardianError::InvalidInput(_)` (matches the existing dashboard pattern for unparseable path/query inputs) |
| Unknown nonce on a known account | `404 Not Found` | `GuardianError::DeltaNotFound { .. }` |
| Unknown account id (v1; unifies with unauthorized once per-account ACL ships) | `404 Not Found` | `GuardianError::AccountNotFound(_)` — body is shape-identical to the `DeltaNotFound` shape so SC-008 holds (no field-level difference distinguishes the two) |
| Listing decode failure on a single entry | — | entry still returned with missing fields null; `category` falls back to `custom`; warning surfaces only in detail responses, not in the listing |
| Listing pagination / cursor errors | — | unchanged from current behavior (`GuardianError::InvalidCursor` already exists) |

The 404 shape parity (between `DeltaNotFound` and `AccountNotFound` for SC-008) must be enforced by an integration test that diffs the response bodies, since the two variants do today emit different `code` strings — see plan §Validation Matrix for how this is exercised.

## Notes on Rust type surface

```text
crates/server/src/services/dashboard_account_deltas.rs
    pub struct DashboardDeltaEntry { ... }                  // extended

crates/server/src/services/dashboard_global_deltas.rs
    pub struct DashboardGlobalDeltaEntry { ... }            // extended in lockstep (flat struct, hand-mapped — see plan Phase B)

crates/server/src/services/dashboard_account_delta_detail.rs    // NEW
    pub struct DashboardDeltaDetail { ... }
    pub struct DetailIncludeFlags { pub scripts: bool, pub raw: bool }
    pub async fn get_account_delta_detail(
        state: &AppState,
        account_id: &str,
        nonce: u64,
        include: DetailIncludeFlags,
    ) -> Result<DashboardDeltaDetail>;

crates/server/src/delta_summary/                                // NEW
    pub enum DashboardDeltaCategory { ... }
    pub struct DeltaActivitySummary { ... }
    pub struct DecodedNote { ... }
    pub struct DecodedAsset { ... }
    pub struct VaultChange { ... }
    pub struct StorageChange { ... }
    pub struct DecodeWarning { ... }

    /// Result of normalizing a raw `delta_payload` blob into a uniform
    /// shape callers can classify or project from. See research.md
    /// Decision 10 for the two on-disk shapes this resolves.
    pub enum NormalizedPayload {
        WithSummary { summary: TransactionSummary, metadata: Option<MultisigMetadata> },
        Opaque { reason: &'static str },                    // EVM, schema drift, malformed base64, etc.
    }
    impl NormalizedPayload {
        pub fn resolve(payload: &serde_json::Value) -> (Self, Vec<DecodeWarning>);
    }

    pub fn classify(normalized: &NormalizedPayload)
                    -> (DashboardDeltaCategory, Option<String>, DeltaActivitySummary);

    pub fn decode_full(normalized: &NormalizedPayload, include_scripts: bool)
                      -> (Vec<DecodedNote>, Vec<DecodedNote>, Vec<VaultChange>, Vec<StorageChange>, Vec<DecodeWarning>);
```

The `NormalizedPayload::resolve` intermediate is the canonical entry point — callers (the listing service, the detail service, and tests) MUST go through it. `classify` and `decode_full` never accept a raw `serde_json::Value` directly so that the wrapper-vs-raw branching lives in exactly one place (per Decision 10).

All structs derive `serde::Serialize` only (these are response shapes; nothing deserialized from a wire request other than the URL segments). All `Option<T>` fields use the project's existing serde conventions — `#[serde(skip_serializing_if = "Option::is_none")]` for truly optional fields, explicit `null` serialization for the fields that callers rely on as a stable key set (`category` is `T`, `kind` is serialized as `null` not skipped).
