//! Push-time orchestrator that builds the [`DeltaMetadata`] blob.
//!
//! Decodes the persisted `TransactionSummary`, optionally lifts a
//! matching `delta_proposals` row's metadata as operator intent, then
//! derives `category`, `note_counts`, `asset`, and `counterparty`.
//! Returns `None` when nothing meaningful can be derived (e.g. EVM
//! deltas); the column is persisted NULL in that case.

use miden_protocol::transaction::TransactionSummary;
use serde_json::Value;

use super::category::{category_from_proposal_type, infer_category_from_summary};
use super::decode::{decode_proposal_metadata, decode_transaction_summary};
use super::projection::{
    project_assets_and_counterparty_from_input_notes,
    project_assets_and_counterparty_from_output_notes, project_note_counts,
};
use super::{
    AssetKind, AssetSummary, CounterpartyDirection, CounterpartySummary, DeltaMetadata,
    ProposalMetadata,
};

/// Build the typed [`DeltaMetadata`] blob for a delta being persisted.
///
/// When `matching_proposal_payload` is `Some`, its `proposal_type`
/// drives `category` and seeds `assets` / `counterparty`. Returns
/// `None` when no `TransactionSummary` decodes and no proposal
/// metadata exists — caller persists NULL.
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
            // Intent without a decodable summary: surface the proposal
            // block with category inferred from proposal_type alone;
            // note_counts defaults to (0, 0).
            Some(DeltaMetadata {
                category: category_from_proposal_type(&proposal.proposal_type),
                assets: asset_from_proposal(&proposal).into_iter().collect(),
                counterparty: counterparty_from_proposal(&proposal),
                note_counts: Default::default(),
                proposal: Some(proposal),
            })
        }
    }
}

/// Read the [`ProposalMetadata`] from a stored proposal's `delta_payload`.
pub fn lift_proposal_metadata(proposal_payload: &Value) -> Option<ProposalMetadata> {
    decode_proposal_metadata(proposal_payload)
}

fn assemble(summary: &TransactionSummary, proposal: Option<ProposalMetadata>) -> DeltaMetadata {
    let note_counts = project_note_counts(summary);

    let category = match proposal.as_ref() {
        Some(p) => category_from_proposal_type(&p.proposal_type),
        None => infer_category_from_summary(summary),
    };

    let proposal_asset = proposal.as_ref().and_then(asset_from_proposal);
    let proposal_counterparty = proposal.as_ref().and_then(counterparty_from_proposal);

    let (output_assets, output_counterparty) =
        project_assets_and_counterparty_from_output_notes(summary);
    let (input_assets, input_counterparty) =
        project_assets_and_counterparty_from_input_notes(summary);

    // Assets: prefer notes (richer, multi-asset aware); fall back to
    // the proposal's single declared asset only if both note paths are
    // empty so single-asset p2id intents are still represented.
    let assets = if !output_assets.is_empty() {
        output_assets
    } else if !input_assets.is_empty() {
        input_assets
    } else {
        proposal_asset.into_iter().collect()
    };

    // Counterparty stays single-valued: proposal recipient → output
    // note (always None today) → input note sender.
    let counterparty = proposal_counterparty
        .or(output_counterparty)
        .or(input_counterparty);

    DeltaMetadata {
        category,
        assets,
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

/// Parse a JSONB column value into a typed [`DeltaMetadata`]. Returns
/// `None` for null columns or when the persisted shape no longer
/// matches the typed struct (schema drift, pre-feature rows).
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
        json!({
            "tx_summary": create_test_delta_payload(TEST_ACCOUNT_ID_HEX),
            "metadata": metadata,
            "signatures": [],
        })
    }

    #[test]
    fn build_without_proposal_uses_topology_for_category() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let metadata = build_metadata(&delta_payload, None).expect("metadata built");
        assert_eq!(
            metadata.category,
            DashboardDeltaCategory::AccountStorageChange,
        );
        assert!(metadata.proposal.is_none());
        assert!(metadata.assets.is_empty());
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
        assert_eq!(metadata.assets.len(), 1, "p2id surfaces a single asset");
        let asset = &metadata.assets[0];
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
        assert!(metadata.assets.is_empty());
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
    fn build_with_consume_notes_and_input_note_surfaces_asset_on_listing() {
        use guardian_shared::ToJson;
        use miden_protocol::account::AccountId;
        use miden_protocol::account::delta::{
            AccountDelta, AccountStorageDelta, AccountVaultDelta,
        };
        use miden_protocol::asset::FungibleAsset;
        use miden_protocol::crypto::rand::RandomCoin;
        use miden_protocol::note::NoteType;
        use miden_protocol::transaction::InputNote;
        use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
        use miden_protocol::{Felt, Word, ZERO};
        use miden_standards::note::P2idNote;

        const CONSUMER: &str = "0x9d03b229c1a649905f70588309fe71";
        const NOTE_SENDER: &str = "0x7bfb0f38b0fafa103f86a805594170";
        const FAUCET: &str = "0x16f6c85d5652c9200879145bfdda93";

        let sender = AccountId::from_hex(NOTE_SENDER).expect("sender");
        let consumer = AccountId::from_hex(CONSUMER).expect("consumer");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let asset = FungibleAsset::new(faucet, 100_000_000)
            .expect("fungible asset")
            .into();
        let mut rng = RandomCoin::new(Word::from([9u32, 8, 7, 6]));
        let note = P2idNote::create(
            sender,
            consumer,
            vec![asset],
            NoteType::Public,
            Default::default(),
            &mut rng,
        )
        .expect("p2id note");
        let delta = AccountDelta::new(
            consumer,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("account delta");
        let summary = TransactionSummary::new(
            delta,
            InputNotes::new(vec![InputNote::unauthenticated(note)]).expect("inputs"),
            RawOutputNotes::new(Vec::new()).expect("outputs"),
            Word::from([ZERO; 4]),
        );
        let delta_payload = summary.to_json();
        let proposal_payload = synthetic_proposal_payload(json!({
            "proposal_type": "consume_notes",
            "note_ids": ["0xb83a44fe769b101be22cc5fd35ec09292483eb69b2dbc2e4c1b94095cbecf7da"],
        }));
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert_eq!(metadata.category, DashboardDeltaCategory::NoteConsumption);
        assert_eq!(metadata.assets.len(), 1, "asset from single input note");
        let listing_asset = &metadata.assets[0];
        assert_eq!(listing_asset.asset_id, FAUCET);
        assert_eq!(listing_asset.amount.as_deref(), Some("+100000000"));
        let cp = metadata
            .counterparty
            .as_ref()
            .expect("note sender counterparty");
        assert_eq!(cp.account_id, NOTE_SENDER);
        assert_eq!(cp.direction, CounterpartyDirection::In);
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
        assert_eq!(metadata.category, DashboardDeltaCategory::Custom);
        assert_eq!(
            metadata.proposal.as_ref().unwrap().proposal_type,
            "newfangled_thing_not_in_mapping_table",
        );
    }

    #[test]
    fn build_with_undecodable_delta_payload_returns_none() {
        let payload = json!({"evm": "0xfeedface"});
        let metadata = build_metadata(&payload, None);
        assert!(metadata.is_none());
    }

    #[test]
    fn build_with_malformed_proposal_metadata_keeps_derived_block() {
        let delta_payload = create_test_delta_payload(TEST_ACCOUNT_ID_HEX);
        let proposal_payload = json!({
            "tx_summary": create_test_delta_payload(TEST_ACCOUNT_ID_HEX),
            "metadata": { "description": "missing proposal_type" },
            "signatures": [],
        });
        let metadata =
            build_metadata(&delta_payload, Some(&proposal_payload)).expect("metadata built");
        assert!(metadata.proposal.is_none());
        assert_eq!(
            metadata.category,
            DashboardDeltaCategory::AccountStorageChange,
        );
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let original = DeltaMetadata {
            category: DashboardDeltaCategory::AssetTransfer,
            assets: vec![AssetSummary {
                asset_id: "0xfaucet".to_string(),
                kind: AssetKind::Fungible,
                amount: Some("-100".to_string()),
            }],
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
