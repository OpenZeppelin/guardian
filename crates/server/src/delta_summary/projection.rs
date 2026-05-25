//! Projectors that derive structured wire fields from a decoded
//! [`TransactionSummary`]. Used at push time by `build_metadata` to
//! populate the `derived` half of [`DeltaMetadata`].
//!
//! Two surfaces:
//!   - [`project_note_counts`] — cheap, always callable.
//!   - [`project_asset_and_counterparty_from_output_notes`] — walks the
//!     first output note and pulls `(asset, counterparty)` from it
//!     when the note carries assets. Best-effort: returns `(None, None)`
//!     when no output note exists or the first one is empty.
//!
//! [`decode_full`] is the Phase 4 / US2 stub that will project the
//! whole detail-view shape; kept here so the module's public surface
//! is committed.

use miden_protocol::asset::Asset;
use miden_protocol::transaction::TransactionSummary;

use super::{
    AssetKind, AssetSummary, CounterpartySummary, DecodeWarning, DecodedNote, NoteCounts,
    StorageChange, VaultChange,
};

/// Note input/output counts. Always cheap, always callable.
pub fn project_note_counts(summary: &TransactionSummary) -> NoteCounts {
    NoteCounts {
        input: summary.input_notes().num_notes() as u32,
        output: summary.output_notes().num_notes() as u32,
    }
}

/// Walk the first output note and extract `(asset, counterparty)` for
/// the listing summary. Best-effort:
///
///   - First output note's first asset, when present, is surfaced as
///     `AssetSummary` (signed `amount` for fungible — negative because
///     the account *sent* the asset out).
///   - Counterparty is the output note's `sender` (per `NoteMetadata`).
///     For an account creating an output note, the sender is the
///     creating account itself; that's not a useful "counterparty"
///     from the dashboard perspective, so for single-key push we
///     leave it `None`. The multisig path overrides counterparty
///     from `proposal.recipient_id` upstream in `build_metadata`,
///     which is the right source.
///
/// Returns `(None, None)` if there are no output notes, the first one
/// is empty, or asset extraction fails.
pub fn project_asset_and_counterparty_from_output_notes(
    summary: &TransactionSummary,
) -> (Option<AssetSummary>, Option<CounterpartySummary>) {
    let outputs = summary.output_notes();
    if outputs.num_notes() == 0 {
        return (None, None);
    }
    let Some(first) = outputs.iter().next() else {
        return (None, None);
    };

    let assets = first.assets();
    let asset_summary = assets.iter().next().map(|asset| match asset {
        Asset::Fungible(a) => AssetSummary {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::Fungible,
            // Sent out → negative magnitude.
            amount: Some(format!("-{}", a.amount())),
        },
        Asset::NonFungible(a) => AssetSummary {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::NonFungible,
            amount: None,
        },
    });

    // Counterparty intentionally left None here for single-key push —
    // the output note's `metadata().sender()` is the creating account
    // (i.e. us), which is not a useful "counterparty" on the
    // dashboard. When metadata is available (multisig), build.rs
    // populates this from `proposal.recipient_id`.
    let counterparty = None;

    (asset_summary, counterparty)
}

/// Decode the full detail-view projection.
///
/// Returns `(input_notes, output_notes, vault_changes, storage_changes, warnings)`.
/// `include_scripts` controls whether the optional `script` field on
/// each decoded note is populated (per Decision 5, opt-in via
/// `?include=scripts`).
///
/// **Phase 4 stub**: returns empty sections. US2 replaces the body
/// (which will take a `TransactionSummary` + the persisted
/// `DeltaMetadata` and project the full structured shape).
pub fn decode_full(
    _summary: &TransactionSummary,
    _include_scripts: bool,
) -> (
    Vec<DecodedNote>,
    Vec<DecodedNote>,
    Vec<VaultChange>,
    Vec<StorageChange>,
    Vec<DecodeWarning>,
) {
    (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
}
