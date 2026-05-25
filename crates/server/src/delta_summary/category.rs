//! Classifier for canonical deltas.
//!
//! Derives the closed `category`, the optional `kind`, and the
//! listing-level `summary` from a [`NormalizedPayload`]. See
//! `data-model.md` and `research.md` Decision 2.

use super::{
    AssetKind, AssetSummary, CounterpartyDirection, CounterpartySummary, DashboardDeltaCategory,
    DeltaActivitySummary, MultisigMetadata, NormalizedPayload, NoteCounts,
};

/// Classify a normalized payload into `(category, kind, summary)`.
///
///   - `category` is never `Custom` by accident — for known
///     `proposal_type` values it follows FR-002a; for absent metadata
///     it follows FR-002b inference rules; only truly unrecognized
///     shapes fall through to `Custom`.
///   - `kind` echoes the underlying `metadata.proposal_type` when
///     present (multisig deltas), `None` otherwise.
///   - `summary` carries the listing-level derived fields per
///     `data-model.md`; sub-fields are `None` when not safely
///     extractable (FR-004).
pub fn classify(
    normalized: &NormalizedPayload,
) -> (DashboardDeltaCategory, Option<String>, DeltaActivitySummary) {
    match normalized {
        NormalizedPayload::WithSummary { summary, metadata } => {
            let counts = note_counts(summary);
            if let Some(meta) = metadata {
                let category = category_from_proposal_type(&meta.proposal_type);
                let kind = Some(meta.proposal_type.clone());
                let activity = build_summary_from_metadata(meta, counts);
                (category, kind, activity)
            } else {
                let category = infer_category_no_metadata(summary, &counts);
                let activity = DeltaActivitySummary {
                    asset: None,
                    counterparty: None,
                    note_counts: counts,
                };
                (category, None, activity)
            }
        }
        NormalizedPayload::Opaque { .. } => (
            DashboardDeltaCategory::Custom,
            None,
            DeltaActivitySummary::default(),
        ),
    }
}

/// Map a `metadata.proposal_type` value to its dashboard `category`
/// per FR-002a. Unknown strings fall back to `Custom` so a future
/// proposal type added in the multisig client doesn't break the
/// listing here — it surfaces as `kind: "<new_type>", category:
/// "custom"` until this map is extended.
fn category_from_proposal_type(proposal_type: &str) -> DashboardDeltaCategory {
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

/// Infer `category` from the on-chain `TransactionSummary` alone, used
/// for single-key `push_delta` and EVM-bridge deltas that carry no
/// metadata (FR-002b). Coarse heuristic: note-count topology dominates;
/// account-state-only changes land in `account_storage_change`.
fn infer_category_no_metadata(
    summary: &miden_protocol::transaction::TransactionSummary,
    counts: &NoteCounts,
) -> DashboardDeltaCategory {
    let has_input = counts.input > 0;
    let has_output = counts.output > 0;
    let _ = summary; // Deeper inference (per-note tag inspection,
                     // pswap detection) belongs to projection.rs in
                     // US2; here we only use topology.
    match (has_input, has_output) {
        (true, true) => DashboardDeltaCategory::AssetTransfer,
        (true, false) => DashboardDeltaCategory::NoteConsumption,
        (false, true) => DashboardDeltaCategory::NoteCreation,
        (false, false) => DashboardDeltaCategory::AccountStorageChange,
    }
}

fn note_counts(summary: &miden_protocol::transaction::TransactionSummary) -> NoteCounts {
    NoteCounts {
        input: summary.input_notes().num_notes() as u32,
        output: summary.output_notes().num_notes() as u32,
    }
}

/// Build a [`DeltaActivitySummary`] for the multisig (metadata-present)
/// case. Asset / counterparty are extracted from `metadata` when the
/// `proposal_type` carries them (`p2id`); other proposal types do not
/// surface an asset on the listing.
fn build_summary_from_metadata(meta: &MultisigMetadata, counts: NoteCounts) -> DeltaActivitySummary {
    let mut summary = DeltaActivitySummary {
        note_counts: counts,
        ..DeltaActivitySummary::default()
    };

    if meta.proposal_type == "p2id" {
        if let (Some(faucet), Some(amount)) = (&meta.faucet_id, &meta.amount) {
            summary.asset = Some(AssetSummary {
                asset_id: faucet.clone(),
                kind: AssetKind::Fungible,
                amount: Some(format!("-{amount}")),
            });
        }
        if let Some(recipient) = &meta.recipient_id {
            summary.counterparty = Some(CounterpartySummary {
                account_id: recipient.clone(),
                direction: CounterpartyDirection::Out,
            });
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_summary::tests::fixtures;

    #[test]
    fn p2id_multisig_classifies_as_asset_transfer() {
        let (normalized, _) = crate::delta_summary::resolve_payload(&fixtures::multisig_p2id_wrapper());
        let (category, kind, summary) = classify(&normalized);
        assert_eq!(category, DashboardDeltaCategory::AssetTransfer);
        assert_eq!(kind.as_deref(), Some("p2id"));
        let asset = summary.asset.expect("asset surfaced from metadata");
        assert_eq!(asset.kind, AssetKind::Fungible);
        assert_eq!(asset.amount.as_deref(), Some("-100"));
        let counterparty = summary.counterparty.expect("recipient surfaced");
        assert_eq!(counterparty.direction, CounterpartyDirection::Out);
    }

    #[test]
    fn add_signer_classifies_as_account_storage_change() {
        let (normalized, _) = crate::delta_summary::resolve_payload(&fixtures::multisig_add_signer());
        let (category, kind, summary) = classify(&normalized);
        assert_eq!(category, DashboardDeltaCategory::AccountStorageChange);
        assert_eq!(kind.as_deref(), Some("add_signer"));
        assert!(summary.asset.is_none());
        assert!(summary.counterparty.is_none());
    }

    #[test]
    fn switch_guardian_classifies_as_guardian_switch() {
        let (normalized, _) =
            crate::delta_summary::resolve_payload(&fixtures::multisig_switch_guardian());
        let (category, kind, _) = classify(&normalized);
        assert_eq!(category, DashboardDeltaCategory::GuardianSwitch);
        assert_eq!(kind.as_deref(), Some("switch_guardian"));
    }

    #[test]
    fn push_delta_with_empty_summary_classifies_as_account_storage_change() {
        let (normalized, _) =
            crate::delta_summary::resolve_payload(&fixtures::push_delta_raw_tx_summary());
        let (category, kind, summary) = classify(&normalized);
        // Empty summary fixture: no notes, only account_delta — falls
        // into the "topology" bucket for account_storage_change.
        assert_eq!(category, DashboardDeltaCategory::AccountStorageChange);
        assert!(kind.is_none());
        assert!(summary.asset.is_none());
    }

    #[test]
    fn evm_placeholder_classifies_as_custom() {
        let (normalized, _) = crate::delta_summary::resolve_payload(&fixtures::evm_placeholder());
        let (category, kind, summary) = classify(&normalized);
        assert_eq!(category, DashboardDeltaCategory::Custom);
        assert!(kind.is_none());
        assert!(summary.asset.is_none());
        assert_eq!(summary.note_counts, NoteCounts::default());
    }

    #[test]
    fn malformed_base64_classifies_as_custom() {
        let (normalized, _) = crate::delta_summary::resolve_payload(&fixtures::malformed_base64());
        let (category, kind, _) = classify(&normalized);
        assert_eq!(category, DashboardDeltaCategory::Custom);
        assert!(kind.is_none());
    }

    #[test]
    fn classifier_never_returns_null_category() {
        // SC-002: every payload, no matter how malformed, produces
        // a non-null category.
        for payload in fixtures::all_fixtures() {
            let (normalized, _) = crate::delta_summary::resolve_payload(&payload);
            let (category, _, _) = classify(&normalized);
            // `category` is `DashboardDeltaCategory` — non-nullable
            // by type. This loop also exercises that every fixture
            // can be classified without panicking.
            let _ = category;
        }
    }
}
