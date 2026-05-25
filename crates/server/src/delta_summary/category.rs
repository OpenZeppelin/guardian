//! Category inference rules.
//!
//! When metadata is present, the proposal's `proposal_type` drives
//! `category` directly via [`category_from_proposal_type`] (FR-002a).
//! When metadata is absent (single-key push, EVM, malformed proposal),
//! [`infer_category_from_summary`] uses the on-chain transaction
//! topology — input/output note counts plus any decoded note tags —
//! per FR-002b.

use miden_protocol::transaction::TransactionSummary;

use super::DashboardDeltaCategory;

/// Map an operator-declared `proposal_type` string to its dashboard
/// `category`. Unknown strings fall back to `Custom` so adding a new
/// proposal type in the multisig client doesn't break the listing —
/// it surfaces as `category: "custom"` with the original
/// `proposal_type` still visible inside the `proposal` block.
pub fn category_from_proposal_type(proposal_type: &str) -> DashboardDeltaCategory {
    match proposal_type {
        "p2id" => DashboardDeltaCategory::AssetTransfer,
        "consume_notes" => DashboardDeltaCategory::NoteConsumption,
        "switch_guardian" => DashboardDeltaCategory::GuardianSwitch,
        "add_signer" | "remove_signer" | "change_threshold" | "update_procedure_threshold" => {
            DashboardDeltaCategory::AccountStorageChange
        }
        _ => DashboardDeltaCategory::Custom,
    }
}

/// Infer `category` from the on-chain `TransactionSummary` alone — used
/// for single-key `push_delta` and EVM-bridge deltas that carry no
/// metadata (FR-002b).
///
/// Heuristic: note-count topology dominates; account-state-only
/// changes (no notes) land in `account_storage_change`. Deeper
/// inference (per-note-tag detection of `pswap` for swaps, `p2id` for
/// transfers) would require walking the output notes — that work lives
/// in `projection::project_asset_and_counterparty_from_output_notes`
/// and the category is *upgraded* there if it found a recognized tag.
pub fn infer_category_from_summary(summary: &TransactionSummary) -> DashboardDeltaCategory {
    let has_input = summary.input_notes().num_notes() > 0;
    let has_output = summary.output_notes().num_notes() > 0;
    match (has_input, has_output) {
        (true, true) => DashboardDeltaCategory::AssetTransfer,
        (true, false) => DashboardDeltaCategory::NoteConsumption,
        (false, true) => DashboardDeltaCategory::NoteCreation,
        (false, false) => DashboardDeltaCategory::AccountStorageChange,
    }
}
