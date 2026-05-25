//! Typed metadata blob persisted on every canonical delta, plus the
//! push-time derivation pipeline that builds it.
//!
//! Spec reference: feature `007-dashboard-delta-details` —
//! `data-model.md`, `research.md` Decisions 2, 3, 4, 5, 10.
//!
//! ## Architecture
//!
//! Each delta carries an optional [`DeltaMetadata`] blob that is
//! derived once at push time and stored in the `deltas.metadata` JSONB
//! column. The blob carries:
//!
//!   - **Derived fields** (always populated when metadata is non-null):
//!     `category`, `asset`, `counterparty`, `note_counts`. Reconstructed
//!     from the persisted `TransactionSummary` and, when available,
//!     refined from the matching proposal's metadata.
//!
//!   - **Proposal block** (`proposal`, populated for multisig pushes
//!     only): lifted verbatim from the matching `delta_proposals` row
//!     so operator intent at proposal-creation time is preserved on the
//!     canonical record for audit, policy evaluation, and detail
//!     rendering.
//!
//! All decoding work happens once at push time inside
//! [`build_metadata`]. Dashboard listings are pure column reads — no
//! `TransactionSummary` decode on the hot path.

use serde::{Deserialize, Serialize};

pub mod build;
pub mod category;
pub mod decode;
pub mod projection;

pub use build::{build_metadata, lift_proposal_metadata, metadata_from_value, metadata_to_value};
pub use category::{category_from_proposal_type, infer_category_from_summary};
pub use decode::{decode_proposal_metadata, decode_transaction_summary};
pub use projection::{
    decode_full, project_asset_and_counterparty_from_input_notes,
    project_asset_and_counterparty_from_output_notes, project_note_counts,
};

#[cfg(test)]
pub(crate) mod tests {
    pub mod fixtures;
}

// ---------------------------------------------------------------------
// DeltaMetadata — the top-level blob persisted in `deltas.metadata`.
// ---------------------------------------------------------------------

/// Persisted activity metadata for a canonical delta.
///
/// Stored as JSONB in the `deltas.metadata` column. Built once at push
/// time by [`build_metadata`] from the decoded `TransactionSummary`
/// plus (for multisig) the matching `delta_proposals` row.
///
/// `None` (NULL column) for:
///   - EVM deltas (no derivation rules yet)
///   - Pre-feature-007 historical rows that were never reprocessed
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaMetadata {
    /// Closed enum: what the delta did at the coarsest useful level.
    /// Always present (FR-002, SC-002).
    pub category: DashboardDeltaCategory,

    /// First asset surfaced in deterministic order. `None` when the
    /// underlying transaction does not move an asset (e.g. account
    /// admin operations) or when extraction failed (FR-004).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetSummary>,

    /// Counterparty of the transaction. `None` for transactions
    /// without a clear sender/recipient (admin ops, swaps, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<CounterpartySummary>,

    /// Always present.
    #[serde(default)]
    pub note_counts: NoteCounts,

    /// Multisig proposal intent lifted from the matching
    /// `delta_proposals` row at push time. Absent for single-key
    /// `push_delta`, EVM deltas, and pushes where no matching proposal
    /// was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<ProposalMetadata>,
}

/// Closed, stable enumeration of action categories.
///
/// Adding a value is a wire-contract change (FR-002). Every persisted
/// metadata blob carries a non-null `category` (SC-002).
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
// `AssetSwap` is intentionally absent. Detecting it requires
// per-note-tag inspection of output notes (matching the Miden `pswap`
// note tag's use-case constant). Adding the variant before that
// detection lands would mean shipping a wire-contract value that is
// never emitted. When pswap detection is implemented in
// `projection.rs`, `AssetSwap` returns as a wire-contract addition
// (and the TS / smoke-web / spec must be updated in lockstep).

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

// ---------------------------------------------------------------------
// ProposalMetadata — operator-stated intent, lifted from a matching
// proposal. Mirrors `ProposalMetadataPayload` in
// `crates/miden-multisig-client/src/payload.rs` field-for-field.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProposalMetadata {
    /// One of the validated multisig proposal types (`add_signer`,
    /// `remove_signer`, `change_threshold`, `update_procedure_threshold`,
    /// `p2id`, `consume_notes`, `switch_guardian`). Future types added
    /// to the multisig client may appear here without an enum update —
    /// this field is intentionally a free string so the dashboard does
    /// not block on the wire-contract bump (`category` is the closed
    /// enum, not this).
    pub proposal_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_signatures: Option<u64>,

    // ---- p2id ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faucet_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,

    // ---- consume_notes ----
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consume_notes_metadata_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consume_notes_notes: Vec<String>,

    // ---- add_signer / remove_signer / change_threshold ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_commitments: Vec<String>,

    // ---- switch_guardian ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_guardian_pubkey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_guardian_endpoint: Option<String>,

    // ---- update_procedure_threshold ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_procedure: Option<String>,
}

// ---------------------------------------------------------------------
// Detail-view types (kept for future Phase 4 / US2 work — listing path
// does not build these).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodedNote {
    pub note_id: String,
    pub tag: NoteTag,
    pub assets: Vec<DecodedAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    // Note MAST scripts deliberately not exposed in v1
    // (US2 scope decision, 2026-05-25).
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
    /// `miden_protocol::account::StorageSlotName` (e.g.
    /// `"consumed_notes"`, `"executed_txs"`). Slots are identified by
    /// name in Miden, not by numeric index — earlier drafts of this
    /// field misnamed it `slot_index` and dropped the name string.
    pub slot_name: String,
    /// Hex-encoded `Word` (64 hex chars + `0x` prefix) of the value
    /// before the change. Omitted on the wire in v1 — a
    /// `TransactionSummary` carries only post-change slot values.
    /// Populating `before` requires reading account storage at
    /// `prev_commitment` (future enhancement).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// Hex-encoded `Word` (64 hex chars + `0x` prefix) of the value
    /// after the change. `None` when the slot was cleared.
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
