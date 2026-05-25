//! Push-time orchestrator that builds the [`DeltaMetadata`] blob.
//!
//! The pipeline:
//!
//!   1. Decode the `TransactionSummary` from the persisted
//!      `delta_payload` (the candidate row's payload — either the raw
//!      `{data: base64}` shape from `push_delta` or the wrapper shape
//!      from the multisig pre-execute proposal storage path).
//!   2. If a matching proposal exists in `delta_proposals`, lift its
//!      `metadata` block into a typed [`ProposalMetadata`]. This is
//!      the *intent* layer.
//!   3. Build the *derived* layer: `category` (refined by proposal
//!      type when present, else inferred from topology), `note_counts`
//!      (from the decoded summary), and `asset` / `counterparty`
//!      (from proposal metadata when present for `p2id`, falling back
//!      to a shallow walk of the first output note for single-key
//!      push).
//!   4. Merge into a [`DeltaMetadata`] and stash on the
//!      [`DeltaObject`] before persistence.
//!
//! Returns `None` when nothing meaningful can be derived (e.g. EVM
//! deltas whose `delta_payload` is not a `TransactionSummary`). The
//! caller persists `metadata: None` (column NULL) in that case;
//! listings fall back to `category: "custom"` per FR-002b.

use miden_protocol::transaction::TransactionSummary;
use serde_json::Value;

use super::category::{category_from_proposal_type, infer_category_from_summary};
use super::decode::{decode_proposal_metadata, decode_transaction_summary};
use super::projection::{project_asset_and_counterparty_from_output_notes, project_note_counts};
use super::{
    AssetKind, AssetSummary, CounterpartyDirection, CounterpartySummary, DeltaMetadata,
    ProposalMetadata,
};

/// Build the typed [`DeltaMetadata`] blob for a delta being persisted.
///
/// Inputs:
///   - `delta_payload` — the value being persisted on the new row.
///     Used to decode the `TransactionSummary`.
///   - `matching_proposal_payload` — optional payload of a matching
///     proposal looked up by the caller (push_delta). When `Some`,
///     drives `proposal_type` → `category` mapping and seeds
///     `asset` / `counterparty` from the proposal's typed metadata
///     fields.
///
/// Returns `None` when no `TransactionSummary` is decodable from
/// `delta_payload` AND no proposal metadata exists. In that case
/// the column should be NULL.
pub fn build_metadata(
    delta_payload: &Value,
    matching_proposal_payload: Option<&Value>,
) -> Option<DeltaMetadata> {
    let proposal_metadata = matching_proposal_payload.and_then(decode_proposal_metadata);

    let tx_summary = decode_transaction_summary(delta_payload).ok();

    match (tx_summary, proposal_metadata) {
        (None, None) => None,
        (Some(summary), proposal) => Some(assemble(&summary, proposal)),
        (None, Some(proposal)) => {
            // We have intent but no decodable summary — unusual, but
            // still surface the proposal block with an inferred
            // category from the proposal_type alone. note_counts
            // defaults to (0, 0) because we cannot enumerate notes.
            Some(DeltaMetadata {
                category: category_from_proposal_type(&proposal.proposal_type),
                asset: asset_from_proposal(&proposal),
                counterparty: counterparty_from_proposal(&proposal),
                note_counts: Default::default(),
                proposal: Some(proposal),
            })
        }
    }
}

/// Read the [`ProposalMetadata`] from a stored proposal `DeltaObject`'s
/// `delta_payload`. Public re-export of [`decode_proposal_metadata`]
/// so callers don't need to reach into the `decode` submodule.
pub fn lift_proposal_metadata(proposal_payload: &Value) -> Option<ProposalMetadata> {
    decode_proposal_metadata(proposal_payload)
}

// ---------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------

fn assemble(summary: &TransactionSummary, proposal: Option<ProposalMetadata>) -> DeltaMetadata {
    let note_counts = project_note_counts(summary);

    let category = match proposal.as_ref() {
        Some(p) => category_from_proposal_type(&p.proposal_type),
        None => infer_category_from_summary(summary),
    };

    // Asset / counterparty preference:
    //   1. Proposal metadata is the strongest signal for p2id (operator
    //      declared exactly what they intended to do).
    //   2. For everything else (single-key push or non-p2id multisig
    //      ops), fall back to a shallow walk of the first output note.
    //   3. If neither yields anything, leave them None — the listing
    //      entry is still valid per FR-004.
    let (asset_from_proposal_block, counterparty_from_proposal_block) = proposal
        .as_ref()
        .map(|p| (asset_from_proposal(p), counterparty_from_proposal(p)))
        .unwrap_or((None, None));

    let (asset_from_notes, counterparty_from_notes) =
        if asset_from_proposal_block.is_some() && counterparty_from_proposal_block.is_some() {
            // Skip the walk if the proposal already gave us both.
            (None, None)
        } else {
            project_asset_and_counterparty_from_output_notes(summary)
        };

    let asset = asset_from_proposal_block.or(asset_from_notes);
    let counterparty = counterparty_from_proposal_block.or(counterparty_from_notes);

    DeltaMetadata {
        category,
        asset,
        counterparty,
        note_counts,
        proposal,
    }
}

fn asset_from_proposal(p: &ProposalMetadata) -> Option<AssetSummary> {
    if p.proposal_type != "p2id" {
        return None;
    }
    let faucet = p.faucet_id.as_ref()?;
    let amount = p.amount.as_ref()?;
    Some(AssetSummary {
        asset_id: faucet.clone(),
        kind: AssetKind::Fungible,
        amount: Some(format!("-{amount}")),
    })
}

fn counterparty_from_proposal(p: &ProposalMetadata) -> Option<CounterpartySummary> {
    if p.proposal_type != "p2id" {
        return None;
    }
    p.recipient_id
        .as_ref()
        .map(|recipient| CounterpartySummary {
            account_id: recipient.clone(),
            direction: CounterpartyDirection::Out,
        })
}

// ---------------------------------------------------------------------
// Convenience wrapper used by the storage-layer From impls — given a
// `serde_json::Value` lifted out of the JSONB column, deserialize it
// into the typed `DeltaMetadata`. Returns `None` if the column was
// missing/null or if the persisted shape no longer matches the typed
// struct (older rows from before this feature, schema drift).
// ---------------------------------------------------------------------

/// Parse a JSONB column value into a typed [`DeltaMetadata`].
pub fn metadata_from_value(value: Value) -> Option<DeltaMetadata> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// Serialize a typed [`DeltaMetadata`] back to a JSONB-compatible value.
pub fn metadata_to_value(metadata: &DeltaMetadata) -> Value {
    serde_json::to_value(metadata).expect("DeltaMetadata is serializable")
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::delta_summary::{AssetKind, CounterpartyDirection, DashboardDeltaCategory};
    use crate::testing::helpers::create_test_delta_payload;
    use serde_json::json;

    const TEST_ACCOUNT_ID_HEX: &str = "0x7bfb0f38b0fafa103f86a805594170";

    fn synthetic_proposal_payload(metadata: Value) -> Value {
        // Mirrors the wrapper shape that `delta_proposals.delta_payload`
        // carries (see `services/mod.rs::normalize_payload`).
        json!({
            "tx_summary": create_test_delta_payload(TEST_ACCOUNT_ID_HEX),
            "metadata": metadata,
            "signatures": [],
        })
    }

    #[test]
    fn build_without_proposal_uses_topology_for_category() {
        // No proposal → category derived from TransactionSummary
        // topology alone. Our test summary is empty (no notes), so it
        // collapses to account_storage_change per FR-002b.
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let metadata = build_metadata(&delta_payload, None).expect("metadata built");
        assert_eq!(
            metadata.category,
            DashboardDeltaCategory::AccountStorageChange,
        );
        assert!(metadata.proposal.is_none());
        assert!(metadata.asset.is_none());
        assert!(metadata.counterparty.is_none());
        assert_eq!(metadata.note_counts.input, 0);
        assert_eq!(metadata.note_counts.output, 0);
    }

    #[test]
    fn build_with_p2id_proposal_carries_asset_counterparty_and_proposal_block() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = synthetic_proposal_payload(json!({
            "proposal_type": "p2id",
            "recipient_id": "0xrecipient0000000000000000000001",
            "faucet_id": "0xfaucet000000000000000000000001",
            "amount": "100",
            "required_signatures": 2,
        }));
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert_eq!(metadata.category, DashboardDeltaCategory::AssetTransfer);
        let asset = metadata.asset.as_ref().expect("p2id surfaces asset");
        assert_eq!(asset.kind, AssetKind::Fungible);
        assert_eq!(asset.amount.as_deref(), Some("-100"));
        let cp = metadata.counterparty.as_ref().expect("recipient surfaces");
        assert_eq!(cp.direction, CounterpartyDirection::Out);
        let proposal = metadata.proposal.as_ref().expect("proposal block lifted");
        assert_eq!(proposal.proposal_type, "p2id");
        assert_eq!(proposal.amount.as_deref(), Some("100"));
        assert_eq!(proposal.required_signatures, Some(2));
    }

    #[test]
    fn build_with_add_signer_proposal_collapses_to_account_storage_change() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = synthetic_proposal_payload(json!({
            "proposal_type": "add_signer",
            "target_threshold": 2,
            "signer_commitments": ["0xc1", "0xc2"],
        }));
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert_eq!(
            metadata.category,
            DashboardDeltaCategory::AccountStorageChange,
        );
        assert!(metadata.asset.is_none());
        assert!(metadata.counterparty.is_none());
        let proposal = metadata.proposal.as_ref().expect("proposal lifted");
        assert_eq!(proposal.proposal_type, "add_signer");
        assert_eq!(proposal.target_threshold, Some(2));
        assert_eq!(proposal.signer_commitments.len(), 2);
    }

    #[test]
    fn build_with_consume_notes_proposal_categorizes_and_lifts_note_ids() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = synthetic_proposal_payload(json!({
            "proposal_type": "consume_notes",
            "note_ids": ["0xnote0000000000000000000000000001"],
            "consume_notes_metadata_version": 2,
            "consume_notes_notes": ["c29tZWJhc2U2NA=="],
        }));
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert_eq!(metadata.category, DashboardDeltaCategory::NoteConsumption);
        let proposal = metadata.proposal.as_ref().expect("proposal lifted");
        assert_eq!(proposal.note_ids.len(), 1);
        assert_eq!(proposal.consume_notes_metadata_version, Some(2));
        assert_eq!(proposal.consume_notes_notes.len(), 1);
    }

    #[test]
    fn build_with_switch_guardian_proposal_categorizes_correctly() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = synthetic_proposal_payload(json!({
            "proposal_type": "switch_guardian",
            "new_guardian_pubkey": "0xpubkey",
            "new_guardian_endpoint": "https://new-guardian.example",
        }));
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert_eq!(metadata.category, DashboardDeltaCategory::GuardianSwitch);
        let proposal = metadata.proposal.as_ref().expect("proposal lifted");
        assert_eq!(proposal.proposal_type, "switch_guardian");
        assert_eq!(proposal.new_guardian_pubkey.as_deref(), Some("0xpubkey"));
    }

    #[test]
    fn build_with_unknown_proposal_type_falls_back_to_custom_but_carries_proposal() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = synthetic_proposal_payload(json!({
            "proposal_type": "newfangled_thing_not_in_mapping_table",
            "description": "test",
        }));
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        // Unknown proposal_type → category = custom, but the
        // proposal block is preserved verbatim so downstream callers
        // can still see what the operator declared.
        assert_eq!(metadata.category, DashboardDeltaCategory::Custom);
        assert_eq!(
            metadata.proposal.as_ref().unwrap().proposal_type,
            "newfangled_thing_not_in_mapping_table",
        );
    }

    #[test]
    fn build_with_undecodable_delta_payload_returns_none() {
        // EVM-style payload — no TransactionSummary, no proposal.
        let payload = json!({"evm": "0xfeedface"});
        let metadata = build_metadata(&payload, None);
        assert!(metadata.is_none());
    }

    #[test]
    fn build_with_malformed_proposal_metadata_keeps_derived_block() {
        // Proposal payload exists but its metadata is malformed
        // (proposal_type missing). We get the derived block from the
        // TransactionSummary and `proposal = None`.
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = json!({
            "tx_summary": create_test_delta_payload(TEST_ACCOUNT_ID_HEX),
            "metadata": { "description": "missing proposal_type" },
            "signatures": [],
        });
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert!(metadata.proposal.is_none());
        // Derived block still populated via topology.
        assert_eq!(
            metadata.category,
            DashboardDeltaCategory::AccountStorageChange,
        );
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let original = DeltaMetadata {
            category: DashboardDeltaCategory::AssetTransfer,
            asset: Some(AssetSummary {
                asset_id: "0xfaucet".to_string(),
                kind: AssetKind::Fungible,
                amount: Some("-100".to_string()),
            }),
            counterparty: Some(CounterpartySummary {
                account_id: "0xrecipient".to_string(),
                direction: CounterpartyDirection::Out,
            }),
            note_counts: super::super::NoteCounts {
                input: 0,
                output: 1,
            },
            proposal: Some(ProposalMetadata {
                proposal_type: "p2id".to_string(),
                amount: Some("100".to_string()),
                ..ProposalMetadata::default()
            }),
        };
        let value = metadata_to_value(&original);
        let round_tripped = metadata_from_value(value).expect("metadata parses");
        assert_eq!(original, round_tripped);
    }
}
