//! Projectors that derive structured wire fields from a decoded
//! [`TransactionSummary`]. Used at push time by `build_metadata` and
//! at read time by [`decode_full`] for the detail endpoint.

use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::crypto::utils::Serializable;
use miden_protocol::note::Note;
use miden_protocol::note::PartialNote;
use miden_protocol::transaction::{RawOutputNote, TransactionSummary};
use miden_standards::note::{P2idNoteStorage, P2ideNoteStorage, StandardNote};

use super::{
    AssetKind, AssetSummary, CounterpartyDirection, CounterpartySummary, DecodeWarning,
    DecodedNote, NoteCounts, NoteTag, StorageChange, VaultChange,
};

pub fn project_note_counts(summary: &TransactionSummary) -> NoteCounts {
    NoteCounts {
        input: summary.input_notes().num_notes() as u32,
        output: summary.output_notes().num_notes() as u32,
    }
}

/// Walk every output note and collect `(assets, counterparty)` for the
/// listing summary. Every asset on every output note is included so
/// multi-asset transactions are represented faithfully. Counterparty is
/// left `None` for single-key pushes; the multisig path overrides it
/// from `proposal.recipient_id` upstream in `build_metadata`.
pub fn project_assets_and_counterparty_from_output_notes(
    summary: &TransactionSummary,
) -> (Vec<AssetSummary>, Option<CounterpartySummary>) {
    let outputs = summary.output_notes();
    let assets: Vec<AssetSummary> = outputs
        .iter()
        .flat_map(|note| {
            note.assets()
                .iter()
                .map(|asset| asset_summary_from_note_asset(asset, false))
                .collect::<Vec<_>>()
        })
        .collect();
    (assets, None)
}

/// Walk every input note and collect `(assets, counterparty)` for
/// consumption-style transactions. Every asset on every input note is
/// included. The first input note's original sender becomes the
/// counterparty with direction `in`.
pub fn project_assets_and_counterparty_from_input_notes(
    summary: &TransactionSummary,
) -> (Vec<AssetSummary>, Option<CounterpartySummary>) {
    let inputs = summary.input_notes();
    let assets: Vec<AssetSummary> = inputs
        .iter()
        .flat_map(|input_note| {
            input_note
                .note()
                .assets()
                .iter()
                .map(|asset| asset_summary_from_note_asset(asset, true))
                .collect::<Vec<_>>()
        })
        .collect();
    let counterparty = inputs.iter().next().map(|input_note| CounterpartySummary {
        account_id: account_id_hex(input_note.note().metadata().sender()),
        direction: CounterpartyDirection::In,
    });
    (assets, counterparty)
}

/// Return shape for [`decode_full`]: the five per-section vectors
/// projected from a persisted `TransactionSummary` in fixed order:
/// `(input_notes, output_notes, vault_changes, storage_changes, warnings)`.
pub type DecodedFullSections = (
    Vec<DecodedNote>,
    Vec<DecodedNote>,
    Vec<VaultChange>,
    Vec<StorageChange>,
    Vec<DecodeWarning>,
);

/// Decode the full detail-view projection from a persisted
/// `TransactionSummary`. Storage changes carry only `after`; `before`
/// would require reading storage at `prev_commitment`. MAST scripts
/// are not exposed.
pub fn decode_full(summary: &TransactionSummary) -> DecodedFullSections {
    let warnings: Vec<DecodeWarning> = Vec::new();

    let input_notes: Vec<DecodedNote> = summary
        .input_notes()
        .iter()
        .map(|input_note| decoded_note_from_full_note(input_note.note()))
        .collect();

    let output_notes: Vec<DecodedNote> = summary
        .output_notes()
        .iter()
        .map(decoded_note_from_raw_output)
        .collect();

    let account_delta = summary.account_delta();
    let vault_changes = project_vault_changes(account_delta);
    let storage_changes = project_storage_changes(account_delta);

    (
        input_notes,
        output_notes,
        vault_changes,
        storage_changes,
        warnings,
    )
}

fn decoded_note_from_raw_output(raw: &RawOutputNote) -> DecodedNote {
    match raw {
        RawOutputNote::Full(note) => decoded_note_from_full_note(note),
        RawOutputNote::Partial(partial) => decoded_note_from_partial_note(partial),
    }
}

fn decoded_note_from_full_note(note: &Note) -> DecodedNote {
    let (sender, recipient) = project_parties_from_note(note);
    DecodedNote {
        note_id: note.id().to_hex(),
        tag: classify_note_tag(note),
        assets: note.assets().iter().map(decoded_asset_from).collect(),
        sender,
        recipient,
    }
}

fn decoded_note_from_partial_note(partial: &PartialNote) -> DecodedNote {
    DecodedNote {
        note_id: partial.id().to_hex(),
        tag: NoteTag::Custom,
        assets: partial.assets().iter().map(decoded_asset_from).collect(),
        sender: Some(account_id_hex(partial.metadata().sender())),
        recipient: None,
    }
}

fn classify_note_tag(note: &Note) -> NoteTag {
    match StandardNote::from_script(note.script()) {
        Some(StandardNote::P2ID) => NoteTag::P2id,
        Some(StandardNote::P2IDE) => NoteTag::P2ide,
        Some(StandardNote::SWAP) => NoteTag::Pswap,
        Some(StandardNote::MINT) => NoteTag::Mint,
        Some(StandardNote::BURN) => NoteTag::Burn,
        None => NoteTag::Custom,
    }
}

fn project_parties_from_note(note: &Note) -> (Option<String>, Option<String>) {
    let sender = Some(account_id_hex(note.metadata().sender()));
    let recipient = recipient_account_from_note(note);
    (sender, recipient)
}

fn recipient_account_from_note(note: &Note) -> Option<String> {
    match StandardNote::from_script(note.script())? {
        StandardNote::P2ID => P2idNoteStorage::try_from(note.storage().items())
            .ok()
            .map(|storage| account_id_hex(storage.target())),
        StandardNote::P2IDE => P2ideNoteStorage::try_from(note.storage().items())
            .ok()
            .map(|storage| account_id_hex(storage.target())),
        _ => None,
    }
}

fn asset_summary_from_note_asset(asset: &Asset, consumed: bool) -> AssetSummary {
    match asset {
        Asset::Fungible(a) => {
            let magnitude = a.amount();
            let signed = if consumed {
                format!("+{magnitude}")
            } else {
                format!("-{magnitude}")
            };
            AssetSummary {
                asset_id: a.faucet_id().to_hex(),
                kind: AssetKind::Fungible,
                amount: Some(signed),
            }
        }
        Asset::NonFungible(a) => AssetSummary {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::NonFungible,
            amount: None,
        },
    }
}

fn account_id_hex(account_id: AccountId) -> String {
    account_id.to_hex()
}

fn decoded_asset_from(asset: &Asset) -> super::DecodedAsset {
    use miden_protocol::asset::Asset;
    match asset {
        Asset::Fungible(a) => super::DecodedAsset {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::Fungible,
            amount: Some(a.amount().to_string()),
        },
        Asset::NonFungible(a) => super::DecodedAsset {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::NonFungible,
            amount: None,
        },
    }
}

fn project_vault_changes(delta: &miden_protocol::account::delta::AccountDelta) -> Vec<VaultChange> {
    use miden_protocol::asset::Asset;
    use std::collections::BTreeMap;

    let vault = delta.vault();
    let mut out: Vec<VaultChange> = Vec::new();

    let mut fungible_net: BTreeMap<String, i128> = BTreeMap::new();
    for asset in vault.added_assets() {
        if let Asset::Fungible(a) = asset {
            *fungible_net.entry(a.faucet_id().to_hex()).or_insert(0) += a.amount() as i128;
        }
    }
    for asset in vault.removed_assets() {
        if let Asset::Fungible(a) = asset {
            *fungible_net.entry(a.faucet_id().to_hex()).or_insert(0) -= a.amount() as i128;
        }
    }
    for (asset_id, net) in fungible_net {
        if net == 0 {
            continue;
        }
        let change = if net > 0 {
            format!("+{net}")
        } else {
            format!("{net}")
        };
        out.push(VaultChange::Fungible { asset_id, change });
    }

    let mut nf_added: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut nf_removed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for asset in vault.added_assets() {
        if let Asset::NonFungible(a) = asset {
            let faucet = a.faucet_id().to_hex();
            let id = format!("0x{}", hex::encode(a.vault_key().to_bytes()));
            nf_added.entry(faucet).or_default().push(id);
        }
    }
    for asset in vault.removed_assets() {
        if let Asset::NonFungible(a) = asset {
            let faucet = a.faucet_id().to_hex();
            let id = format!("0x{}", hex::encode(a.vault_key().to_bytes()));
            nf_removed.entry(faucet).or_default().push(id);
        }
    }
    let mut nf_faucets: std::collections::BTreeSet<String> = Default::default();
    nf_faucets.extend(nf_added.keys().cloned());
    nf_faucets.extend(nf_removed.keys().cloned());
    for faucet in nf_faucets {
        out.push(VaultChange::NonFungible {
            asset_id: faucet.clone(),
            added: nf_added.remove(&faucet).unwrap_or_default(),
            removed: nf_removed.remove(&faucet).unwrap_or_default(),
        });
    }

    out
}

fn project_storage_changes(
    delta: &miden_protocol::account::delta::AccountDelta,
) -> Vec<StorageChange> {
    let storage = delta.storage();
    let mut out: Vec<StorageChange> = storage
        .values()
        .map(|(slot_name, word)| StorageChange {
            slot_name: slot_name.as_str().to_string(),
            key: None,
            before: None,
            after: Some(format!("0x{}", hex::encode(word.as_bytes()))),
        })
        .collect();
    for (slot_name, map_delta) in storage.maps() {
        for (map_key, word) in map_delta.entries() {
            out.push(StorageChange {
                slot_name: slot_name.as_str().to_string(),
                key: Some(format!("0x{}", hex::encode(map_key.as_bytes()))),
                before: None,
                after: Some(format!("0x{}", hex::encode(word.as_bytes()))),
            });
        }
    }
    out
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
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

    fn summary_with_consumed_p2id_note() -> TransactionSummary {
        let sender = AccountId::from_hex(NOTE_SENDER).expect("sender");
        let consumer = AccountId::from_hex(CONSUMER).expect("consumer");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let asset = FungibleAsset::new(faucet, 100_000_000)
            .expect("fungible asset")
            .into();
        let mut rng = RandomCoin::new(Word::from([1u32, 2, 3, 4]));
        let note = P2idNote::create(
            sender,
            consumer,
            vec![asset],
            NoteType::Public,
            Default::default(),
            &mut rng,
        )
        .expect("p2id note");
        let input = InputNote::unauthenticated(note);
        let delta = AccountDelta::new(
            consumer,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("account delta");
        TransactionSummary::new(
            delta,
            InputNotes::new(vec![input]).expect("input notes"),
            RawOutputNotes::new(Vec::new()).expect("output notes"),
            Word::from([ZERO; 4]),
        )
    }

    #[test]
    fn project_input_notes_surfaces_consumed_assets_and_sender_counterparty() {
        let summary = summary_with_consumed_p2id_note();
        let (assets, counterparty) = project_assets_and_counterparty_from_input_notes(&summary);
        assert_eq!(assets.len(), 1);
        let asset = &assets[0];
        assert_eq!(asset.kind, AssetKind::Fungible);
        assert_eq!(asset.asset_id, FAUCET);
        assert_eq!(asset.amount.as_deref(), Some("+100000000"));
        let cp = counterparty.expect("counterparty");
        assert_eq!(cp.account_id, NOTE_SENDER);
        assert_eq!(cp.direction, CounterpartyDirection::In);
    }

    #[test]
    fn decode_full_classifies_p2id_input_note_tag_and_parties() {
        let summary = summary_with_consumed_p2id_note();
        let (inputs, outputs, _, storage, warnings) = decode_full(&summary);
        assert!(warnings.is_empty());
        assert_eq!(inputs.len(), 1);
        assert!(outputs.is_empty());
        assert!(storage.is_empty());
        assert_eq!(inputs[0].tag, NoteTag::P2id);
        assert_eq!(inputs[0].sender.as_deref(), Some(NOTE_SENDER));
        assert_eq!(inputs[0].recipient.as_deref(), Some(CONSUMER));
        assert_eq!(inputs[0].assets[0].amount.as_deref(), Some("100000000"));
    }

    #[test]
    fn storage_change_json_omits_before_when_unpopulated() {
        let change = StorageChange {
            slot_name: "openzeppelin::multisig::threshold_config".to_string(),
            key: None,
            before: None,
            after: Some("0x0200".to_string()),
        };
        let json = serde_json::to_value(&change).expect("serializable");
        assert!(json.get("before").is_none());
        assert!(json.get("key").is_none());
        assert_eq!(json.get("after").and_then(|v| v.as_str()), Some("0x0200"));
    }

    #[test]
    fn project_storage_changes_emits_one_entry_per_map_key() {
        use miden_protocol::account::delta::{
            AccountStorageDelta, StorageMapDelta, StorageSlotDelta,
        };
        use miden_protocol::account::{StorageMapKey, StorageSlotName};

        let proc_root =
            Word::parse("0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89")
                .expect("proc root");
        let threshold_word = Word::from([Felt::new(1), ZERO, ZERO, ZERO]);

        let mut map_delta = StorageMapDelta::default();
        map_delta.insert(StorageMapKey::new(proc_root), threshold_word);

        let slot_name =
            StorageSlotName::new("openzeppelin::multisig::proc_threshold_overrides").unwrap();
        let storage =
            AccountStorageDelta::from_raw([(slot_name, StorageSlotDelta::Map(map_delta))].into());
        let delta = AccountDelta::new(
            AccountId::from_hex(CONSUMER).expect("acct"),
            storage,
            AccountVaultDelta::default(),
            Felt::new(1),
        )
        .expect("delta");

        let changes = project_storage_changes(&delta);
        assert_eq!(changes.len(), 1);
        let c = &changes[0];
        assert_eq!(
            c.slot_name,
            "openzeppelin::multisig::proc_threshold_overrides"
        );
        assert_eq!(
            c.key.as_deref(),
            Some("0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89")
        );
        assert!(c.after.is_some());
    }
}
