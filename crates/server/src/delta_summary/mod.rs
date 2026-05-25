//! Shared decoder, classifier, and projector for canonical deltas
//! surfaced on the operator dashboard.
//!
//! Spec reference: feature `007-dashboard-delta-details`,
//! `data-model.md`, `research.md` Decisions 2, 3, 4, 5, 10.
//!
//! Public entry points:
//!   - [`resolve_payload`] turns a raw persisted `delta_payload`
//!     ([`serde_json::Value`]) into a [`NormalizedPayload`], handling
//!     both on-disk shapes (multisig wrapper and raw `push_delta`
//!     `TransactionSummary`).
//!   - [`classify`] derives the closed `category`, optional `kind`,
//!     and the listing-level summary from a normalized payload.
//!   - [`decode_full`] (Phase 4 / US2) projects the full detail-view
//!     shape; currently a stub that returns empty sections.

use serde::Serialize;

pub mod category;
pub mod decode;
pub mod projection;

pub use category::classify;
pub use decode::resolve_payload;
pub use projection::decode_full;

/// One-call helper that resolves and classifies a raw `delta_payload`
/// in a single step. Used by both listing services
/// (`dashboard_account_deltas` and `dashboard_global_deltas`) so they
/// stay in lockstep — every wire-shape change should land here, not in
/// the two projections.
///
/// Listing entries do not surface `DecodeWarning`s (per
/// `data-model.md`), so this helper drops them. The detail endpoint
/// uses [`resolve_payload`] + [`classify`] + [`decode_full`] directly
/// to retain warnings.
pub fn classify_delta_payload(
    payload: &serde_json::Value,
) -> (DashboardDeltaCategory, Option<String>, DeltaActivitySummary) {
    let (normalized, _warnings) = resolve_payload(payload);
    classify(&normalized)
}

#[cfg(test)]
pub(crate) mod tests {
    pub mod fixtures;
}

/// Closed, stable enumeration of action categories.
///
/// Adding a value is a wire-contract change (FR-002). Every listing
/// and detail entry carries a non-null `category` (SC-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardDeltaCategory {
    AssetTransfer,
    AssetSwap,
    NoteConsumption,
    NoteCreation,
    AccountStorageChange,
    GuardianSwitch,
    Custom,
}

/// Per-entry derived summary fields surfaced on the listing endpoints.
///
/// Each sub-field is `None` when not safely extractable; the listing
/// entry is still returned (FR-004). `note_counts` is always present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct DeltaActivitySummary {
    pub asset: Option<AssetSummary>,
    pub counterparty: Option<CounterpartySummary>,
    pub note_counts: NoteCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetSummary {
    pub asset_id: String,
    pub kind: AssetKind,
    /// Signed decimal magnitude (e.g., `"+100"`, `"-50"`) for fungible;
    /// `None` for non-fungible holdings where the wire shape uses
    /// `added` / `removed` lists in the detail view instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Fungible,
    NonFungible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CounterpartySummary {
    pub account_id: String,
    pub direction: CounterpartyDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterpartyDirection {
    Out,
    In,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct NoteCounts {
    pub input: u32,
    pub output: u32,
}

/// Result of normalizing a raw `delta_payload` blob.
///
/// Two on-disk shapes are recognized (Decision 10):
///
///   1. Multisig wrapper: `{ tx_summary: {data: base64}, metadata: {..}, signatures?: [..] }`
///   2. Raw `push_delta`: `{ data: base64 }` — the TransactionSummary
///      JSON shape, persisted directly without a wrapper.
///
/// Anything else (EVM deltas, schema drift, malformed base64) becomes
/// [`NormalizedPayload::Opaque`] and is classified as `custom`.
pub enum NormalizedPayload {
    WithSummary {
        summary: miden_protocol::transaction::TransactionSummary,
        metadata: Option<MultisigMetadata>,
    },
    Opaque {
        reason: &'static str,
    },
}

/// Lightweight projection of the multisig metadata block carried under
/// `delta_payload.metadata`. Mirrors the producer in
/// `crates/miden-multisig-client/src/payload.rs` but only retains the
/// fields the dashboard needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MultisigMetadata {
    pub proposal_type: String,
    pub recipient_id: Option<String>,
    pub faucet_id: Option<String>,
    pub amount: Option<String>,
    pub note_ids: Vec<String>,
}

/// Opt-in flags for the detail endpoint. US2 / Phase 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetailIncludeFlags {
    pub scripts: bool,
    pub raw: bool,
}

// --- Detail-view types ------------------------------------------------
//
// Committed in Phase 2 so the wire contract is fixed, but the projector
// in `projection.rs` is a stub until US2 (Phase 4). The listing path
// (US1) never builds these.

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodedNote {
    pub note_id: String,
    pub tag: NoteTag,
    pub assets: Vec<DecodedAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
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
    pub slot_index: u32,
    pub before: Option<String>,
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
