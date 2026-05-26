//! Typed metadata blob persisted on the `deltas` row at push time and
//! the pipeline that builds it.
//!
//! Each delta row (including candidate rows) carries an optional
//! [`DeltaMetadata`] derived once when `push_delta` runs and stored in
//! the `deltas.metadata` JSONB column. Canonicalization only flips the
//! status — it never re-runs derivation. Derived fields (`category`,
//! `assets`, `counterparty`, `note_counts`) come from the persisted
//! `TransactionSummary`. The optional `proposal` block is lifted
//! verbatim from the matching `delta_proposals` row for multisig
//! pushes. Dashboard listings are pure column reads and spread the
//! fields to L1 (no nested `metadata` envelope on the wire).

use serde::{Deserialize, Serialize};

pub mod build;
pub mod category;
pub mod decode;
pub mod projection;

pub use build::{build_metadata, lift_proposal_metadata, metadata_from_value, metadata_to_value};
pub use category::{category_from_proposal_type, infer_category_from_summary};
pub use decode::{decode_proposal_metadata, decode_transaction_summary};
pub use projection::{
    decode_full, project_assets_and_counterparty_from_input_notes,
    project_assets_and_counterparty_from_output_notes, project_note_counts,
};

/// Persisted activity metadata for a canonical delta. Stored as JSONB
/// in the `deltas.metadata` column. `None` for EVM deltas and any
/// historical row never reprocessed by [`build_metadata`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaMetadata {
    pub category: DashboardDeltaCategory,

    /// Assets surfaced in deterministic order. Empty when the
    /// transaction does not move an asset or extraction failed.
    /// Multi-asset transactions populate every extractable entry so
    /// clients do not show a misleading single-asset summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<AssetSummary>,

    /// Counterparty of the transaction. `None` for transactions
    /// without a clear sender/recipient (admin ops, swaps, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<CounterpartySummary>,

    #[serde(default)]
    pub note_counts: NoteCounts,

    /// Multisig proposal intent lifted from the matching
    /// `delta_proposals` row at push time. Absent for single-key
    /// `push_delta`, EVM deltas, and pushes where no proposal matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<ProposalMetadata>,
}

/// Closed, stable enumeration of action categories. Adding a value is
/// a wire-contract change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardDeltaCategory {
    AssetTransfer,
    NoteConsumption,
    NoteCreation,
    AccountStorageChange,
    GuardianSwitch,
    Custom,
}
// `AssetSwap` is intentionally absent: it requires per-note-tag
// inspection of output notes (Miden `pswap` use-case constant) which
// is not yet implemented. Adding it before detection lands would ship
// a wire value that's never emitted.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSummary {
    pub asset_id: String,
    pub kind: AssetKind,
    /// Signed decimal magnitude (e.g., `"+100"`, `"-50"`) for fungible
    /// holdings. Absent for non-fungible holdings where the detail
    /// view uses `added` / `removed` lists instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Fungible,
    NonFungible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterpartySummary {
    pub account_id: String,
    pub direction: CounterpartyDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterpartyDirection {
    Out,
    In,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NoteCounts {
    #[serde(default)]
    pub input: u32,
    #[serde(default)]
    pub output: u32,
}

/// Operator-stated intent lifted from a matching proposal. Mirrors
/// `ProposalMetadataPayload` in `crates/miden-multisig-client/src/payload.rs`
/// field-for-field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProposalMetadata {
    /// One of the validated multisig proposal types (`add_signer`,
    /// `remove_signer`, `change_threshold`, `update_procedure_threshold`,
    /// `p2id`, `consume_notes`, `switch_guardian`). Free string so new
    /// types from the multisig client don't force a wire-contract bump
    /// here — `category` is the closed enum.
    pub proposal_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_signatures: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faucet_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consume_notes_metadata_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consume_notes_notes: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_commitments: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_guardian_pubkey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_guardian_endpoint: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_procedure: Option<String>,
}

/// Detail-view types used by the per-delta endpoint; not built by the
/// listing path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodedNote {
    pub note_id: String,
    pub tag: NoteTag,
    pub assets: Vec<DecodedAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteTag {
    P2id,
    P2ide,
    Pswap,
    Mint,
    Burn,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodedAsset {
    pub asset_id: String,
    pub kind: AssetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VaultChange {
    Fungible {
        asset_id: String,
        change: String,
    },
    NonFungible {
        asset_id: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageChange {
    /// Human-readable slot name from
    /// `miden_protocol::account::StorageSlotName` (e.g. `"consumed_notes"`).
    /// Slots are identified by name in Miden, not by numeric index.
    pub slot_name: String,
    /// Hex-encoded `Word` map key (64 hex chars + `0x` prefix) for
    /// `StorageMap` slot entries. `None` for scalar value slots. For the
    /// multisig `proc_threshold_overrides` map (slot 4) this is the
    /// MASM procedure root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Hex-encoded `Word` (64 hex chars + `0x` prefix) before the
    /// change. Always omitted in v1 — `TransactionSummary` carries
    /// only post-change values; populating `before` requires reading
    /// storage at `prev_commitment`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// Hex-encoded `Word` after the change. `None` when the slot was cleared.
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodeWarning {
    pub section: DecodeSection,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeSection {
    TxSummary,
    Metadata,
    InputNotes,
    OutputNotes,
    Vault,
    Storage,
}
