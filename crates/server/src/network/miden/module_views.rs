//! Account views and transaction projections sent to policy modules.

use guardian_module_sdk::proto::{MapChanges, MapEntry, asset};
use guardian_module_sdk::{
    AccountView, Asset as ModuleAsset, Direction, InputNote, NoteType, OutputNote, StorageChange,
    StorageOp, StorageSlotKind, TxProjection, VaultChange,
};
use guardian_shared::FromJson;
use miden_protocol::Word;
use miden_protocol::account::{Account, StoragePatchOperation, StorageSlotContent};
use miden_protocol::asset::Asset;
use miden_protocol::note::{Note, NoteType as MidenNoteType};
use miden_protocol::transaction::{RawOutputNote, TransactionSummary};
use miden_protocol::utils::serde::Serializable;
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::note::{P2idNoteStorage, P2ideNoteStorage, StandardNote};

fn word_hex(word: &Word) -> String {
    format!("0x{}", hex::encode(word.as_bytes()))
}

fn felt_u32(felt: miden_protocol::Felt) -> Result<u32, String> {
    u32::try_from(felt.as_canonical_u64()).map_err(|_| "value does not fit in u32".into())
}

/// Signers, default threshold and procedure thresholds of a multisig account. An account
/// without the multisig component is single-key: its view carries no signers and no
/// `default_threshold`, and the caller fills the signer and the scheme from the account's
/// verified auth configuration.
pub fn account_view(state_json: &serde_json::Value) -> Result<AccountView, String> {
    let account = Account::from_json(state_json)?;
    let storage = account.storage();

    let Ok(threshold_config) = storage.get_item(AuthGuardedMultisig::threshold_config_slot())
    else {
        return Ok(AccountView {
            commitment: word_hex(&account.to_commitment()),
            ..Default::default()
        });
    };
    let default_threshold = felt_u32(threshold_config[0])?;
    let num_approvers = felt_u32(threshold_config[1])?;

    let signer_commitments = (0..num_approvers)
        .map(|index| {
            storage
                .get_map_item(
                    AuthGuardedMultisig::approver_public_keys_slot(),
                    miden_protocol::account::StorageMapKey::new(Word::from([index, 0, 0, 0])),
                )
                .map_err(|e| format!("approver {index} unreadable: {e}"))
                .and_then(|value| {
                    if value == Word::default() {
                        Err(format!("approver {index} missing"))
                    } else {
                        Ok(word_hex(&value))
                    }
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if signer_commitments.is_empty() {
        return Err("multisig account has no approvers".into());
    }

    let procedure_thresholds = match storage
        .get(AuthGuardedMultisig::procedure_thresholds_slot())
        .map(|slot| slot.content())
    {
        Some(StorageSlotContent::Map(map)) => map
            .entries()
            .map(|(root, value)| Ok((word_hex(&Word::from(*root)), felt_u32(value[0])?)))
            .collect::<Result<_, String>>()?,
        Some(_) => return Err("procedure thresholds slot is not a map".into()),
        None => Default::default(),
    };

    Ok(AccountView {
        commitment: word_hex(&account.to_commitment()),
        auth_scheme: String::new(),
        signer_commitments,
        default_threshold: Some(default_threshold),
        procedure_thresholds,
    })
}

fn module_asset(asset: &Asset) -> ModuleAsset {
    match asset.as_fungible() {
        Some(fungible) => ModuleAsset {
            faucet_id: fungible.faucet_id().to_hex(),
            value: Some(asset::Value::FungibleAmount(fungible.amount().to_string())),
        },
        None => ModuleAsset {
            faucet_id: asset.faucet_id().to_hex(),
            value: Some(asset::Value::NftId(word_hex(&asset.to_id_word()))),
        },
    }
}

fn note_type(note_type: MidenNoteType) -> i32 {
    match note_type {
        MidenNoteType::Public => NoteType::Public as i32,
        MidenNoteType::Private => NoteType::Private as i32,
    }
}

fn p2id_target(note: &Note) -> Option<String> {
    match StandardNote::from_script(note.script())? {
        StandardNote::P2ID => P2idNoteStorage::try_from(note.storage().items())
            .ok()
            .map(|storage| storage.target().to_hex()),
        StandardNote::P2IDE => P2ideNoteStorage::try_from(note.storage().items())
            .ok()
            .map(|storage| storage.target().to_hex()),
        _ => None,
    }
}

fn output_note(raw: &RawOutputNote) -> OutputNote {
    match raw {
        RawOutputNote::Full(note) => OutputNote {
            note_id: note.id().to_hex(),
            note_type: note_type(note.metadata().note_type()),
            partial: false,
            recipient_digest: word_hex(&note.recipient().digest()),
            assets: note.assets().iter().map(module_asset).collect(),
            script_root: word_hex(&Word::from(note.script().root())),
            p2id_target: p2id_target(note),
            tx_fee: matches!(
                StandardNote::from_script(note.script()),
                Some(StandardNote::TX_FEE)
            ),
        },
        RawOutputNote::Partial(partial) => OutputNote {
            note_id: partial.id().to_hex(),
            note_type: note_type(partial.metadata().note_type()),
            partial: true,
            recipient_digest: word_hex(&partial.recipient_digest()),
            assets: partial.assets().iter().map(module_asset).collect(),
            script_root: String::new(),
            p2id_target: None,
            tx_fee: false,
        },
    }
}

fn storage_op(op: StoragePatchOperation) -> i32 {
    match op {
        StoragePatchOperation::Create => StorageOp::Create as i32,
        StoragePatchOperation::Update => StorageOp::Update as i32,
        StoragePatchOperation::Remove => StorageOp::Remove as i32,
    }
}

/// Decodes exactly the summary the Guardian verifies and acks; a proposal wrapper is refused
/// rather than unwrapped, so a module never judges a different summary than the one signed.
pub fn module_transaction(
    tx_summary: &serde_json::Value,
) -> Result<crate::network::ModuleTransaction, String> {
    let summary = TransactionSummary::from_json(tx_summary)
        .map_err(|e| format!("transaction summary does not decode: {e}"))?;
    Ok(crate::network::ModuleTransaction {
        projection: projection_from_summary(&summary)?,
        raw_tx_summary: summary.to_bytes(),
    })
}

fn expiration_block(block_number: u32, expiration_delta: u16) -> Result<u32, String> {
    block_number
        .checked_add(u32::from(expiration_delta))
        .ok_or_else(|| format!("expiration overflows: block {block_number} + {expiration_delta}"))
}

fn projection_from_summary(summary: &TransactionSummary) -> Result<TxProjection, String> {
    let delta = summary.account_delta();
    let vault = delta.vault();
    let vault_changes = vault
        .added_assets()
        .map(|asset| (asset, Direction::In))
        .chain(vault.removed_assets().map(|asset| (asset, Direction::Out)))
        .map(|(asset, direction)| VaultChange {
            asset: Some(module_asset(&asset)),
            direction: direction as i32,
        })
        .collect();

    let storage = delta.storage();
    let mut storage_changes: Vec<StorageChange> = storage
        .values()
        .map(|(slot_name, patch)| StorageChange {
            slot_name: slot_name.as_str().to_string(),
            kind: StorageSlotKind::Value as i32,
            op: storage_op(patch.patch_op()),
            value: patch
                .value()
                .map(|word| word_hex(&word))
                .unwrap_or_default(),
            map: None,
        })
        .collect();
    storage_changes.extend(storage.maps().map(|(slot_name, patch)| StorageChange {
        slot_name: slot_name.as_str().to_string(),
        kind: StorageSlotKind::Map as i32,
        op: storage_op(patch.patch_op()),
        value: String::new(),
        map: patch.entries().map(|entries| {
            MapChanges {
                entries: entries
                    .as_map()
                    .iter()
                    .map(|(key, value)| MapEntry {
                        key: word_hex(&Word::from(*key)),
                        value: word_hex(value),
                    })
                    .collect(),
            }
        }),
    }));

    Ok(TxProjection {
        nonce_delta: delta.nonce_delta().as_canonical_u64() as i64,
        expiration_block: expiration_block(
            summary.block_number().as_u32(),
            summary.expiration_delta(),
        )?,
        vault_changes,
        output_notes: summary.output_notes().iter().map(output_note).collect(),
        input_notes: summary
            .input_notes()
            .iter()
            .map(|input| InputNote {
                note_id: input.note().id().to_hex(),
                assets: input.note().assets().iter().map(module_asset).collect(),
            })
            .collect(),
        storage_changes,
    })
}

#[cfg(test)]
mod tests {
    use guardian_shared::ToJson;
    use miden_protocol::account::{
        AccountCode, AccountId, AccountIdVersion, AccountStorage, AccountType, StorageMap,
        StorageMapKey, StorageSlot,
    };
    use miden_protocol::asset::AssetVault;

    use super::*;

    fn map_slot(
        name: &miden_protocol::account::StorageSlotName,
        entries: Vec<(Word, Word)>,
    ) -> StorageSlot {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (StorageMapKey::new(key), value));
        StorageSlot::with_map(name.clone(), StorageMap::with_entries(entries).unwrap())
    }

    fn account(slots: Vec<StorageSlot>) -> serde_json::Value {
        let account = Account::new_existing(
            AccountId::dummy(
                [3u8; 15],
                AccountIdVersion::Version1,
                AccountType::Private,
                miden_protocol::account::AssetCallbackFlag::Disabled,
            ),
            AssetVault::new(&[]).unwrap(),
            AccountStorage::new(slots).unwrap(),
            AccountCode::mock(),
            miden_protocol::Felt::new_unchecked(1),
        );
        account.to_json()
    }

    fn multisig(
        threshold: u32,
        approvers: u32,
        keys: u32,
        procedures: Vec<(Word, Word)>,
    ) -> serde_json::Value {
        account(vec![
            StorageSlot::with_value(
                AuthGuardedMultisig::threshold_config_slot().clone(),
                Word::from([threshold, approvers, 0, 0]),
            ),
            map_slot(
                AuthGuardedMultisig::approver_public_keys_slot(),
                (0..keys)
                    .map(|i| (Word::from([i, 0, 0, 0]), Word::from([100 + i, 1, 1, 1])))
                    .collect(),
            ),
            map_slot(AuthGuardedMultisig::procedure_thresholds_slot(), procedures),
        ])
    }

    #[test]
    fn view_of_the_fixture_account_has_signers_and_a_threshold() {
        let state: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON).unwrap();
        let view = account_view(&state).unwrap();
        assert!(!view.signer_commitments.is_empty());
        assert!(view.default_threshold.is_some());
        assert!(view.commitment.starts_with("0x"));
    }

    #[test]
    fn view_reads_signers_default_and_procedure_thresholds() {
        let root = Word::from([7u32, 7, 7, 7]);
        let view = account_view(&multisig(
            2,
            3,
            3,
            vec![(root, Word::from([3u32, 0, 0, 0]))],
        ))
        .unwrap();
        assert_eq!(view.signer_commitments.len(), 3);
        assert_eq!(view.default_threshold, Some(2));
        assert_eq!(view.procedure_thresholds.get(&word_hex(&root)), Some(&3));
    }

    #[test]
    fn missing_approver_key_is_an_error_not_a_shorter_list() {
        assert!(account_view(&multisig(1, 2, 1, vec![])).is_err());
    }

    #[test]
    fn account_without_multisig_auth_is_single_key() {
        let state = account(vec![StorageSlot::with_value(
            miden_protocol::account::StorageSlotName::new("example::other::slot").unwrap(),
            Word::default(),
        )]);
        let view = account_view(&state).unwrap();
        assert!(!view.commitment.is_empty());
        assert!(view.signer_commitments.is_empty());
        assert_eq!(view.default_threshold, None);
    }

    #[test]
    fn projection_decodes_every_delta_fixture() {
        for fixture in [
            crate::testing::fixtures::DELTA_1_JSON,
            crate::testing::fixtures::DELTA_2_JSON,
            crate::testing::fixtures::DELTA_3_JSON,
        ] {
            let delta: serde_json::Value = serde_json::from_str(fixture).unwrap();
            let transaction = module_transaction(&delta["delta_payload"]).unwrap();
            let projection = transaction.projection;
            assert!(projection.nonce_delta >= 0);
            for change in &projection.storage_changes {
                assert!(!change.slot_name.is_empty());
                assert_ne!(change.op, StorageOp::Unspecified as i32);
            }
            assert!(!transaction.raw_tx_summary.is_empty());
            let wrapped = serde_json::json!({ "tx_summary": delta["delta_payload"].clone() });
            assert!(module_transaction(&wrapped).is_err());
        }
    }

    #[test]
    fn an_embedded_tx_summary_key_cannot_replace_the_signed_summary() {
        let signed: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON).unwrap();
        let decoy: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::DELTA_2_JSON).unwrap();
        let mut payload = signed["delta_payload"].clone();
        payload["tx_summary"] = decoy["delta_payload"].clone();
        assert_eq!(
            module_transaction(&payload).unwrap(),
            module_transaction(&signed["delta_payload"]).unwrap()
        );
    }

    #[test]
    fn expiration_overflow_is_an_error() {
        assert_eq!(expiration_block(10, 5), Ok(15));
        assert!(expiration_block(u32::MAX, 1).is_err());
    }

    #[test]
    #[ignore = "timing report for the prototype gates; run with --ignored --nocapture"]
    fn view_and_projection_decode_cost() {
        let state: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON).unwrap();
        let delta: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON).unwrap();
        let runs = 200;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            account_view(&state).unwrap();
        }
        let view = started.elapsed() / runs;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            module_transaction(&delta["delta_payload"]).unwrap();
        }
        let projection = started.elapsed() / runs;
        println!("account_view: {view:?} per call, tx_projection: {projection:?} per call");
    }
}
