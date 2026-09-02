//! Category inference. When proposal metadata is present, its
//! `proposal_type` drives `category` directly; otherwise the
//! transaction's note-count topology is used.

use miden_protocol::transaction::TransactionSummary;

use super::DashboardDeltaCategory;

/// Map an operator-declared `proposal_type` string to its dashboard
/// `category`. Unknown strings fall back to `Custom` — the original
/// `proposal_type` remains visible inside the `proposal` block.
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

/// Infer `category` from the `TransactionSummary` alone, used when no
/// proposal metadata is available. Note-count topology dominates;
/// account-state-only changes land in `account_storage_change`.
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
