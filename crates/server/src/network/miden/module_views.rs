//! Account views and transaction projections sent to policy modules.

use guardian_module_sdk::proto::{MapChanges, MapEntry, StorageSlot, asset, storage_slot};
use guardian_module_sdk::{
    AccountView, Asset as ModuleAsset, Direction, InputNote, MidenAccountState, MidenTransaction,
    NoteType, OutputNote, StorageChange, StorageOp, StorageSlotKind, VaultChange,
};
use guardian_shared::{FromJson, SignatureScheme};
use miden_protocol::Felt;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::account::{Account, StoragePatchOperation, StorageSlotContent};
use miden_protocol::asset::Asset;
use miden_protocol::note::{Note, NoteId, NoteType as MidenNoteType};
use miden_protocol::transaction::{RawOutputNote, TransactionSummary};
use miden_protocol::utils::serde::Serializable;
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::note::{P2idNoteStorage, P2ideNoteStorage, StandardNote, TxFeeNote};

fn word_hex(word: &Word) -> String {
    format!("0x{}", hex::encode(word.as_bytes()))
}

fn felt_u32(felt: miden_protocol::Felt) -> Result<u32, String> {
    u32::try_from(felt.as_canonical_u64()).map_err(|_| "value does not fit in u32".into())
}

/// Code, signers with their schemes, default threshold and effective procedure thresholds of an
/// account. An account without the multisig component is single-key: its view carries no signers
/// and no `default_threshold`, and the caller fills the signer and its scheme from the account's
/// verified auth configuration. A signer whose scheme entry is missing or names no supported
/// scheme is an error, never a signer without a scheme.
pub fn account_view(state_json: &serde_json::Value) -> Result<AccountView, String> {
    account_view_of(&Account::from_json(state_json)?)
}

/// [`account_view_of`] together with the account's nonce.
pub fn module_account_of(account: &Account) -> Result<crate::network::ModuleAccount, String> {
    Ok(crate::network::ModuleAccount {
        view: account_view_of(account)?,
        nonce: account.nonce().as_canonical_u64(),
    })
}

/// The whole vault and storage of an account, as a registration's subject carries them.
pub fn account_state(state_json: &serde_json::Value) -> Result<MidenAccountState, String> {
    account_state_of(&Account::from_json(state_json)?)
}

/// [`account_state`] of an account already decoded.
pub fn account_state_of(account: &Account) -> Result<MidenAccountState, String> {
    Ok(MidenAccountState {
        vault: account
            .vault()
            .assets()
            .map(|asset| module_asset(&asset))
            .collect(),
        storage: account
            .storage()
            .slots()
            .iter()
            .map(|slot| {
                let (kind, content) = match slot.content() {
                    StorageSlotContent::Value(word) => (
                        StorageSlotKind::Value,
                        storage_slot::Content::Value(word_hex(word)),
                    ),
                    StorageSlotContent::Map(map) => (
                        StorageSlotKind::Map,
                        storage_slot::Content::Map(MapChanges {
                            entries: map
                                .entries()
                                .map(|(key, value)| MapEntry {
                                    key: word_hex(&Word::from(*key)),
                                    value: word_hex(value),
                                })
                                .collect(),
                        }),
                    ),
                };
                StorageSlot {
                    slot_name: slot.name().as_str().to_string(),
                    kind: kind as i32,
                    content: Some(content),
                }
            })
            .collect(),
    })
}

/// [`account_view`] of an account already decoded.
pub fn account_view_of(account: &Account) -> Result<AccountView, String> {
    let storage = account.storage();
    let code = account.code();
    let procedure_roots: Vec<String> = code.procedure_roots().map(|root| word_hex(&root)).collect();

    let Ok(threshold_config) = storage.get_item(AuthGuardedMultisig::threshold_config_slot())
    else {
        return Ok(AccountView {
            commitment: word_hex(&account.to_commitment()),
            code_commitment: word_hex(&code.commitment()),
            procedure_roots,
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
    let signer_schemes = signer_commitments
        .iter()
        .zip(0u32..)
        .map(|(commitment, index)| {
            let id = storage
                .get_map_item(
                    AuthGuardedMultisig::approver_scheme_ids_slot(),
                    miden_protocol::account::StorageMapKey::new(Word::from([index, 0, 0, 0])),
                )
                .map_err(|e| format!("approver {index} scheme unreadable: {e}"))?[0]
                .as_canonical_u64();
            SignatureScheme::from_auth_scheme_id(id)
                .map(|scheme| (commitment.clone(), scheme.as_str().to_string()))
                .ok_or_else(|| format!("approver {index} has no supported scheme (id {id})"))
        })
        .collect::<Result<_, String>>()?;

    // On chain an override applies only to a non-auth procedure the code exports, and zero means
    // the default; other entries change nothing, so a threshold floor must not see them.
    let auth_procedure = code.procedure_roots().next();
    let procedure_thresholds = match storage
        .get(AuthGuardedMultisig::procedure_thresholds_slot())
        .map(|slot| slot.content())
    {
        Some(StorageSlotContent::Map(map)) => map
            .entries()
            .map(|(root, value)| Ok((Word::from(*root), felt_u32(value[0])?)))
            .filter(|entry| {
                entry.as_ref().map_or(true, |(root, threshold)| {
                    *threshold != 0 && code.has_procedure(*root) && Some(*root) != auth_procedure
                })
            })
            .map(|entry| entry.map(|(root, threshold)| (word_hex(&root), threshold)))
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
        code_commitment: word_hex(&code.commitment()),
        procedure_roots,
        signer_schemes,
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

fn p2ide_reclaimer(note: &Note) -> Option<String> {
    match StandardNote::from_script(note.script())? {
        StandardNote::P2IDE => P2ideNoteStorage::try_from(note.storage().items())
            .ok()
            .filter(|storage| storage.reclaim_height().is_some())
            .map(|storage| storage.reclaimer().to_hex()),
        _ => None,
    }
}

/// How long a chain fee read is used before it is read again.
pub const CHAIN_FEE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// The fee is the base fee times a logarithmic measure of the transaction's cycle count, which
/// stays below 32 for any cycle count the kernel accepts; twice that leaves room for the cycle
/// margins `pay_fee` adds.
const MAX_FEE_VERIFICATION_CYCLES: u64 = 64;

/// The chain's fee faucet and the largest amount of its asset one transaction can owe as a fee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainFee {
    faucet: AccountId,
    max_amount: u64,
}

impl ChainFee {
    pub fn new(faucet: AccountId, verification_base_fee: u32) -> Self {
        Self {
            faucet,
            max_amount: u64::from(verification_base_fee) * MAX_FEE_VERIFICATION_CYCLES,
        }
    }
}

fn tx_fee_notes(summary: &TransactionSummary) -> impl Iterator<Item = &Note> {
    summary.output_notes().iter().filter_map(|raw| match raw {
        RawOutputNote::Full(note)
            if matches!(
                StandardNote::from_script(note.script()),
                Some(StandardNote::TX_FEE)
            ) =>
        {
            Some(note)
        }
        _ => None,
    })
}

/// The transaction's fee note: a note with the TX_FEE script is a payment like any other, since
/// any transaction script can create one, with any assets, that anyone can consume. Only the note
/// `pay_fee` creates is the fee, and it is recognised by everything `pay_fee` fixes: it is the
/// only TX_FEE note, it is public with the TX_FEE tag and no storage or attachments, its serial
/// number is derived from the account, the nonce the transaction starts from and the block its
/// summary binds, and it carries one fungible asset of the chain's fee faucet, no more of it than
/// a transaction can owe. On a chain that charges nothing `pay_fee`
/// creates no note, so no note qualifies there.
fn fee_note(summary: &TransactionSummary, initial_nonce: u64, fee: ChainFee) -> Option<NoteId> {
    let mut tx_fee_notes = tx_fee_notes(summary);
    let note = tx_fee_notes.next()?;
    if tx_fee_notes.next().is_some() {
        return None;
    }
    let shaped_like_pay_fee = note.metadata().note_type() == MidenNoteType::Public
        && note.metadata().tag() == TxFeeNote::TAG
        && usize::from(note.storage().num_items()) == TxFeeNote::NUM_STORAGE_ITEMS
        && note.attachments().is_empty();
    if !shaped_like_pay_fee {
        return None;
    }
    let serial_number = TxFeeNote::derive_serial_number(
        summary.account_delta().id(),
        Felt::new_unchecked(initial_nonce),
        summary.block_number(),
    );
    if note.recipient().serial_num() != serial_number {
        return None;
    }
    let assets: Vec<&Asset> = note.assets().iter().collect();
    match assets.as_slice() {
        [asset] => asset
            .as_fungible()
            .filter(|asset| {
                asset.faucet_id() == fee.faucet && asset.amount().as_u64() <= fee.max_amount
            })
            .map(|_| note.id()),
        _ => None,
    }
}

fn output_note(raw: &RawOutputNote, fee_note: Option<NoteId>) -> OutputNote {
    match raw {
        RawOutputNote::Full(note) => OutputNote {
            note_id: note.id().to_hex(),
            note_type: note_type(note.metadata().note_type()),
            partial: false,
            recipient_digest: word_hex(&note.recipient().digest()),
            assets: note.assets().iter().map(module_asset).collect(),
            script_root: word_hex(&Word::from(note.script().root())),
            p2id_target: p2id_target(note),
            tx_fee: fee_note == Some(note.id()),
            reclaimer: p2ide_reclaimer(note),
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
            reclaimer: None,
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
    initial_nonce: u64,
    fee: Option<ChainFee>,
) -> Result<crate::network::ModuleTransaction, String> {
    let summary = TransactionSummary::from_json(tx_summary)
        .map_err(|e| format!("transaction summary does not decode: {e}"))?;
    module_transaction_of(&summary, initial_nonce, fee)
}

/// [`module_transaction`] of a summary already decoded.
pub fn module_transaction_of(
    summary: &TransactionSummary,
    initial_nonce: u64,
    fee: Option<ChainFee>,
) -> Result<crate::network::ModuleTransaction, String> {
    let fee_note = match fee {
        Some(fee) => fee_note(summary, initial_nonce, fee),
        None if tx_fee_notes(summary).next().is_some() => {
            return Err(
                "the chain's fee asset is unknown, so the transaction's fee note cannot be told \
                 from a payment"
                    .into(),
            );
        }
        None => None,
    };
    Ok(crate::network::ModuleTransaction {
        projection: projection_from_summary(summary, fee_note)?,
        raw_tx_summary: summary.to_bytes(),
    })
}

fn projection_from_summary(
    summary: &TransactionSummary,
    fee_note: Option<NoteId>,
) -> Result<MidenTransaction, String> {
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

    Ok(MidenTransaction {
        nonce_delta: delta.nonce_delta().as_canonical_u64(),
        bound_block: summary.block_number().as_u32(),
        expiration_delta: Some(u32::from(summary.expiration_delta())).filter(|delta| *delta != 0),
        approval_expiration_block: Some(super::execution::approval_expiration_block(summary))
            .filter(|block| *block != 0),
        new_code_commitment: delta
            .code()
            .as_code()
            .map(|code| word_hex(&code.commitment())),
        vault_changes,
        output_notes: summary
            .output_notes()
            .iter()
            .map(|note| output_note(note, fee_note))
            .collect(),
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
        let schemes = vec![SignatureScheme::Falcon.auth_scheme() as u64; keys as usize];
        multisig_with_schemes(threshold, approvers, keys, procedures, &schemes)
    }

    /// `schemes[i]` is the auth scheme id stored for approver `i`.
    fn multisig_with_schemes(
        threshold: u32,
        approvers: u32,
        keys: u32,
        procedures: Vec<(Word, Word)>,
        schemes: &[u64],
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
            map_slot(
                AuthGuardedMultisig::approver_scheme_ids_slot(),
                schemes
                    .iter()
                    .zip(0u32..)
                    .map(|(id, i)| {
                        (
                            Word::from([i, 0, 0, 0]),
                            Word::from([
                                miden_protocol::Felt::new_unchecked(*id),
                                0u32.into(),
                                0u32.into(),
                                0u32.into(),
                            ]),
                        )
                    })
                    .collect(),
            ),
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

    fn mock_roots() -> Vec<Word> {
        AccountCode::mock().procedure_roots().collect()
    }

    #[test]
    fn view_reads_signers_default_and_only_effective_procedure_thresholds() {
        let roots = mock_roots();
        assert!(roots.len() >= 3, "mock code: auth plus two procedures");
        let (auth, effective, zeroed) = (roots[0], roots[1], roots[2]);
        let unexported = Word::from([7u32, 7, 7, 7]);
        let view = account_view(&multisig(
            2,
            3,
            3,
            vec![
                (effective, Word::from([3u32, 0, 0, 0])),
                (zeroed, Word::from([0u32, 0, 0, 0])),
                (auth, Word::from([1u32, 0, 0, 0])),
                (unexported, Word::from([3u32, 0, 0, 0])),
            ],
        ))
        .unwrap();
        assert_eq!(view.signer_commitments.len(), 3);
        assert_eq!(view.default_threshold, Some(2));
        assert_eq!(
            view.procedure_thresholds,
            [(word_hex(&effective), 3)].into(),
            "zero, auth and unexported overrides change nothing on chain"
        );
        assert_eq!(
            view.procedure_roots,
            roots.iter().map(word_hex).collect::<Vec<_>>()
        );
        assert_eq!(
            view.code_commitment,
            word_hex(&AccountCode::mock().commitment())
        );
        assert_eq!(view.signer_schemes.len(), 3);
        assert!(
            view.signer_schemes
                .values()
                .all(|scheme| scheme == "falcon")
        );
    }

    #[test]
    fn view_reads_each_signers_scheme_of_a_mixed_account() {
        let falcon = SignatureScheme::Falcon.auth_scheme() as u64;
        let ecdsa = SignatureScheme::Ecdsa.auth_scheme() as u64;
        let view = account_view(&multisig_with_schemes(2, 2, 2, vec![], &[falcon, ecdsa])).unwrap();
        assert_eq!(
            view.signer_schemes
                .get(&view.signer_commitments[0])
                .map(String::as_str),
            Some("falcon")
        );
        assert_eq!(
            view.signer_schemes
                .get(&view.signer_commitments[1])
                .map(String::as_str),
            Some("ecdsa")
        );
    }

    #[test]
    fn a_missing_or_unknown_approver_scheme_is_an_error_not_a_signer_without_one() {
        let falcon = SignatureScheme::Falcon.auth_scheme() as u64;
        assert!(account_view(&multisig_with_schemes(1, 2, 2, vec![], &[falcon])).is_err());
        assert!(account_view(&multisig_with_schemes(1, 2, 2, vec![], &[falcon, 99])).is_err());
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
        assert_eq!(
            view.code_commitment,
            word_hex(&AccountCode::mock().commitment())
        );
        assert_eq!(view.procedure_roots.len(), mock_roots().len());
    }

    #[test]
    fn projection_decodes_every_delta_fixture() {
        for fixture in [
            crate::testing::fixtures::DELTA_1_JSON,
            crate::testing::fixtures::DELTA_2_JSON,
            crate::testing::fixtures::DELTA_3_JSON,
        ] {
            let delta: serde_json::Value = serde_json::from_str(fixture).unwrap();
            let transaction = module_transaction(&delta["delta_payload"], 0, None).unwrap();
            let projection = transaction.projection;
            for change in &projection.storage_changes {
                assert!(!change.slot_name.is_empty());
                assert_ne!(change.op, StorageOp::Unspecified as i32);
            }
            assert!(!transaction.raw_tx_summary.is_empty());
            let wrapped = serde_json::json!({ "tx_summary": delta["delta_payload"].clone() });
            assert!(module_transaction(&wrapped, 0, None).is_err());
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
            module_transaction(&payload, 0, None).unwrap(),
            module_transaction(&signed["delta_payload"], 0, None).unwrap()
        );
    }

    fn summary(code: Option<AccountCode>, expiration_delta: u16) -> TransactionSummary {
        use miden_protocol::account::{
            AccountCodePatch, AccountDelta, AccountStoragePatch, AccountVaultDelta,
        };
        use miden_protocol::transaction::{InputNotes, RawOutputNotes};
        let delta = AccountDelta::new(
            AccountId::dummy(
                [3u8; 15],
                AccountIdVersion::Version1,
                AccountType::Private,
                miden_protocol::account::AssetCallbackFlag::Disabled,
            ),
            AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            AccountCodePatch::new(code),
            miden_protocol::Felt::new_unchecked(1),
        )
        .unwrap();
        TransactionSummary::new(
            delta,
            InputNotes::new(vec![]).unwrap(),
            RawOutputNotes::new(vec![]).unwrap(),
            miden_protocol::block::BlockNumber::from(100u32),
            Word::default(),
            expiration_delta,
            Default::default(),
        )
    }

    fn dummy_id(byte: u8) -> AccountId {
        AccountId::dummy(
            [byte; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            miden_protocol::account::AssetCallbackFlag::Disabled,
        )
    }

    const NONCE: u64 = 7;
    const BLOCK: u32 = 100;

    fn fee_note_with(serial: Word, assets: Vec<Asset>) -> RawOutputNote {
        let note = TxFeeNote::builder()
            .sender(dummy_id(3))
            .serial_number(serial)
            .assets(assets)
            .build()
            .unwrap();
        RawOutputNote::Full(note.into())
    }

    fn canonical_serial() -> Word {
        TxFeeNote::derive_serial_number(
            dummy_id(3),
            Felt::new_unchecked(NONCE),
            miden_protocol::block::BlockNumber::from(BLOCK),
        )
    }

    fn fungible(faucet: u8, amount: u64) -> Asset {
        miden_protocol::asset::FungibleAsset::new(dummy_id(faucet), amount)
            .unwrap()
            .into()
    }

    fn summary_with(notes: Vec<RawOutputNote>) -> TransactionSummary {
        use miden_protocol::account::{
            AccountCodePatch, AccountDelta, AccountStoragePatch, AccountVaultDelta,
        };
        use miden_protocol::transaction::{InputNotes, RawOutputNotes};
        TransactionSummary::new(
            AccountDelta::new(
                dummy_id(3),
                AccountStoragePatch::default(),
                AccountVaultDelta::default(),
                AccountCodePatch::new(None),
                Felt::new_unchecked(1),
            )
            .unwrap(),
            InputNotes::new(vec![]).unwrap(),
            RawOutputNotes::new(notes).unwrap(),
            miden_protocol::block::BlockNumber::from(BLOCK),
            Word::default(),
            0,
            Default::default(),
        )
    }

    fn p2ide(reclaimer: Option<AccountId>, reclaim_height: Option<u32>) -> RawOutputNote {
        let note = miden_standards::note::P2ideNote::builder()
            .sender(dummy_id(3))
            .target(dummy_id(4))
            .maybe_reclaimer(reclaimer)
            .maybe_reclaim_height(reclaim_height.map(miden_protocol::block::BlockNumber::from))
            .serial_number(canonical_serial())
            .assets(vec![fungible(9, 5)])
            .build()
            .unwrap();
        RawOutputNote::Full(note.into())
    }

    #[test]
    fn a_reclaimable_p2ide_note_names_its_reclaimer_beside_its_target() {
        let summary = summary_with(vec![
            p2ide(Some(dummy_id(5)), Some(1)),
            p2ide(Some(dummy_id(5)), None),
            p2ide(None, Some(1)),
        ]);
        let notes = projection_from_summary(&summary, None)
            .unwrap()
            .output_notes;
        let target = Some(dummy_id(4).to_hex());
        assert!(notes.iter().all(|note| note.p2id_target == target));
        assert_eq!(
            notes
                .iter()
                .map(|note| note.reclaimer.clone())
                .collect::<Vec<_>>(),
            vec![Some(dummy_id(5).to_hex()), None, Some(dummy_id(3).to_hex()),],
            "a reclaimer is named only when a reclaim height enables it; it defaults to the sender"
        );
    }

    fn fee_flags(notes: Vec<RawOutputNote>, fee: Option<ChainFee>) -> Vec<bool> {
        let summary = summary_with(notes);
        projection_from_summary(&summary, fee.and_then(|fee| fee_note(&summary, NONCE, fee)))
            .unwrap()
            .output_notes
            .iter()
            .map(|note| note.tx_fee)
            .collect()
    }

    #[test]
    fn a_fee_note_cannot_be_judged_while_the_fee_asset_is_unknown() {
        use guardian_shared::ToJson;
        let with_fee_note = summary_with(vec![fee_note_with(
            canonical_serial(),
            vec![fungible(9, 5)],
        )])
        .to_json();
        let error = module_transaction(&with_fee_note, NONCE, None).unwrap_err();
        assert!(error.contains("fee asset is unknown"), "{error}");
        let without = summary_with(vec![]).to_json();
        assert!(module_transaction(&without, NONCE, None).is_ok());
    }

    #[test]
    fn a_tx_fee_note_not_shaped_like_the_one_pay_fee_creates_is_a_payment() {
        use miden_protocol::note::{
            NoteAssets, NoteRecipient, NoteStorage, NoteTag, PartialNoteMetadata,
        };
        let fee_faucet = Some(ChainFee::new(dummy_id(9), 1));
        let shaped = |note_type: MidenNoteType, tag: NoteTag, storage: NoteStorage| {
            RawOutputNote::Full(Note::new(
                NoteAssets::new(vec![fungible(9, 5)]).unwrap(),
                PartialNoteMetadata::new(dummy_id(3), note_type).with_tag(tag),
                NoteRecipient::new(canonical_serial(), TxFeeNote::script(), storage),
            ))
        };
        assert_eq!(
            fee_flags(
                vec![shaped(
                    MidenNoteType::Public,
                    TxFeeNote::TAG,
                    NoteStorage::default()
                )],
                fee_faucet
            ),
            [true],
            "the shape pay_fee creates"
        );
        assert_eq!(
            fee_flags(
                vec![shaped(
                    MidenNoteType::Private,
                    TxFeeNote::TAG,
                    NoteStorage::default()
                )],
                fee_faucet
            ),
            [false],
            "a private note"
        );
        assert_eq!(
            fee_flags(
                vec![shaped(
                    MidenNoteType::Public,
                    NoteTag::new(7),
                    NoteStorage::default()
                )],
                fee_faucet
            ),
            [false],
            "another tag"
        );
        assert_eq!(
            fee_flags(
                vec![shaped(
                    MidenNoteType::Public,
                    TxFeeNote::TAG,
                    NoteStorage::new(vec![Felt::new_unchecked(1)]).unwrap()
                )],
                fee_faucet
            ),
            [false],
            "a note with storage"
        );
        let with_attachment = {
            use miden_protocol::note::{NoteAttachment, NoteAttachmentScheme, NoteAttachments};
            let attachment = NoteAttachment::with_word(
                NoteAttachmentScheme::new(1).unwrap(),
                Word::from([1u32, 2, 3, 4]),
            );
            RawOutputNote::Full(Note::with_attachments(
                NoteAssets::new(vec![fungible(9, 5)]).unwrap(),
                PartialNoteMetadata::new(dummy_id(3), MidenNoteType::Public)
                    .with_tag(TxFeeNote::TAG),
                NoteRecipient::new(
                    canonical_serial(),
                    TxFeeNote::script(),
                    NoteStorage::default(),
                ),
                NoteAttachments::new(vec![attachment]).unwrap(),
            ))
        };
        assert_eq!(
            fee_flags(vec![with_attachment], fee_faucet),
            [false],
            "a note with an attachment"
        );
    }

    #[test]
    fn only_the_note_pay_fee_creates_is_the_fee_note() {
        let fee_faucet = Some(ChainFee::new(dummy_id(9), 1));
        let canonical = || fee_note_with(canonical_serial(), vec![fungible(9, 5)]);
        assert_eq!(fee_flags(vec![canonical()], fee_faucet), [true]);
        assert_eq!(
            fee_flags(
                vec![fee_note_with(
                    canonical_serial(),
                    vec![fungible(9, MAX_FEE_VERIFICATION_CYCLES)]
                )],
                fee_faucet
            ),
            [true],
            "the largest fee a transaction can owe"
        );
        assert_eq!(
            fee_flags(
                vec![fee_note_with(
                    canonical_serial(),
                    vec![fungible(9, MAX_FEE_VERIFICATION_CYCLES + 1)]
                )],
                fee_faucet
            ),
            [false],
            "more of the fee asset than a transaction can owe is a payment"
        );
        assert_eq!(
            fee_flags(vec![canonical()], Some(ChainFee::new(dummy_id(9), 0))),
            [false],
            "a chain that charges nothing has no fee note"
        );
        assert_eq!(
            fee_flags(vec![canonical()], None),
            [false],
            "unknown fee faucet"
        );
        assert_eq!(
            fee_flags(
                vec![
                    canonical(),
                    fee_note_with(canonical_serial(), vec![fungible(9, 1_000_000)])
                ],
                fee_faucet
            ),
            [false, false],
            "a second note with the same serial number makes neither the fee"
        );
        assert_eq!(
            fee_flags(
                vec![
                    fee_note_with(Word::from([1u32, 2, 3, 4]), vec![fungible(9, 5)]),
                    canonical()
                ],
                fee_faucet
            ),
            [false, false],
            "an extra TX_FEE note with another serial number"
        );
        assert_eq!(
            fee_flags(
                vec![fee_note_with(
                    Word::from([1u32, 2, 3, 4]),
                    vec![fungible(9, 5)]
                )],
                fee_faucet
            ),
            [false],
            "not the serial number pay_fee derives"
        );
        assert_eq!(
            fee_flags(
                vec![fee_note_with(
                    canonical_serial(),
                    vec![fungible(4, 1_000_000)]
                )],
                fee_faucet
            ),
            [false],
            "an asset of another faucet"
        );
        assert_eq!(
            fee_flags(
                vec![fee_note_with(
                    canonical_serial(),
                    vec![fungible(9, 5), fungible(4, 1_000_000)]
                )],
                fee_faucet
            ),
            [false],
            "more than the fee asset"
        );
    }

    #[test]
    fn projection_carries_the_bound_block_and_expirations_only_when_set() {
        let expiring = projection_from_summary(&summary(None, 20), None).unwrap();
        assert_eq!(expiring.bound_block, 100);
        assert_eq!(expiring.expiration_delta, Some(20));
        let open = projection_from_summary(&summary(None, 0), None).unwrap();
        assert_eq!(open.bound_block, 100);
        assert_eq!(open.expiration_delta, None);
        assert_eq!(open.approval_expiration_block, None);
    }

    #[test]
    fn projection_carries_the_approval_expiry_from_the_user_params() {
        use miden_protocol::transaction::TransactionSummaryUserParams;
        let open = summary(None, 0);
        let mut user_params = [Felt::ZERO; 6];
        user_params[0] = Felt::new_unchecked(500);
        let approved_until = TransactionSummary::new(
            open.account_delta().clone(),
            open.input_notes().clone(),
            open.output_notes().clone(),
            open.block_number(),
            Word::default(),
            0,
            TransactionSummaryUserParams::new(user_params),
        );
        assert_eq!(
            projection_from_summary(&approved_until, None)
                .unwrap()
                .approval_expiration_block,
            Some(500)
        );
    }

    #[test]
    fn an_account_state_lists_every_storage_slot_with_its_content() {
        let account: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON).unwrap();
        let state = account_state(&account).unwrap();
        assert!(!state.storage.is_empty());
        for slot in &state.storage {
            assert!(!slot.slot_name.is_empty());
            assert!(slot.content.is_some());
            assert_ne!(slot.kind, StorageSlotKind::Unspecified as i32);
        }
        assert_eq!(
            state.vault.len(),
            Account::from_json(&account)
                .unwrap()
                .vault()
                .assets()
                .count()
        );
    }

    #[test]
    fn a_code_replacement_names_the_new_code_commitment() {
        let code = AccountCode::mock();
        assert_eq!(
            projection_from_summary(&summary(Some(code.clone()), 0), None)
                .unwrap()
                .new_code_commitment,
            Some(word_hex(&code.commitment()))
        );
        assert_eq!(
            projection_from_summary(&summary(None, 0), None)
                .unwrap()
                .new_code_commitment,
            None
        );
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
            module_transaction(&delta["delta_payload"], 0, None).unwrap();
        }
        let projection = started.elapsed() / runs;
        println!("account_view: {view:?} per call, tx_projection: {projection:?} per call");
    }
}

#[cfg(test)]
mod standard_account_tests {
    use guardian_shared::SignatureScheme::{self, Ecdsa, Falcon};
    use guardian_shared::ToJson;
    use miden_protocol::account::Account;

    use super::*;
    use crate::testing::helpers::{build_guarded_multisig_account, build_single_key_account};

    fn key(n: u32) -> Word {
        Word::from([n, n + 1, n + 2, n + 3])
    }

    fn assert_code_fields(view: &AccountView, account: &Account) {
        assert_eq!(view.code_commitment, word_hex(&account.code().commitment()));
        assert_eq!(
            view.procedure_roots,
            account
                .code()
                .procedure_roots()
                .map(|root| word_hex(&root))
                .collect::<Vec<_>>()
        );
    }

    fn guarded_multisig_with_overrides(
        seed: [u8; 32],
        threshold: u32,
        approvers: &[(Word, SignatureScheme)],
        guardian: (Word, SignatureScheme),
        overrides: impl Fn(&Account) -> Vec<(miden_protocol::account::AccountProcedureRoot, u32)>,
    ) -> Account {
        use miden_protocol::account::AccountBuilder;
        use miden_protocol::account::auth::PublicKeyCommitment;
        use miden_standards::account::auth::{
            Approver, ApproverSet, AuthGuardedMultisigConfig, GuardianConfig,
        };
        use miden_standards::account::wallets::BasicWallet;

        let plain = build_guarded_multisig_account(seed, threshold, approvers, guardian);
        let approver = |(commitment, scheme): (Word, SignatureScheme)| {
            Approver::new(PublicKeyCommitment::from(commitment), scheme.auth_scheme())
        };
        let config = AuthGuardedMultisigConfig::new(
            ApproverSet::new(approvers.iter().copied().map(approver).collect(), threshold).unwrap(),
            GuardianConfig::new(approver(guardian)),
        )
        .unwrap()
        .with_proc_thresholds(overrides(&plain))
        .unwrap();
        AccountBuilder::new(seed)
            .with_component(miden_protocol::account::AccountComponent::from(
                AuthGuardedMultisig::new(config).unwrap(),
            ))
            .with_component(BasicWallet)
            .build()
            .unwrap()
    }

    #[test]
    fn single_key_accounts_carry_their_code_and_leave_the_signer_to_the_registration() {
        for scheme in [Falcon, Ecdsa] {
            let account = build_single_key_account([1; 32], (key(9), scheme));
            let view = account_view(&account.to_json()).unwrap();
            assert!(view.signer_commitments.is_empty());
            assert!(view.signer_schemes.is_empty());
            assert_eq!(view.default_threshold, None);
            assert!(view.procedure_thresholds.is_empty());
            assert_code_fields(&view, &account);
        }
    }

    #[test]
    fn a_two_of_three_falcon_multisig_reads_back_as_built() {
        let approvers = [(key(1), Falcon), (key(2), Falcon), (key(3), Falcon)];
        let account = build_guarded_multisig_account([2; 32], 2, &approvers, (key(7), Falcon));
        let view = account_view(&account.to_json()).unwrap();
        assert_eq!(
            view.signer_commitments,
            approvers
                .iter()
                .map(|(commitment, _)| word_hex(commitment))
                .collect::<Vec<_>>()
        );
        assert_eq!(view.signer_schemes.len(), 3);
        assert!(
            view.signer_schemes
                .values()
                .all(|scheme| scheme == "falcon")
        );
        assert_eq!(view.default_threshold, Some(2));
        assert!(view.procedure_thresholds.is_empty());
        assert_code_fields(&view, &account);
    }

    #[test]
    fn a_mixed_scheme_multisig_keeps_each_scheme_and_its_effective_override() {
        let approvers = [(key(1), Falcon), (key(2), Ecdsa), (key(3), Falcon)];
        let account =
            guarded_multisig_with_overrides([3; 32], 2, &approvers, (key(7), Ecdsa), |plain| {
                vec![(plain.code().procedures()[1], 1)]
            });
        let view = account_view(&account.to_json()).unwrap();
        for (commitment, scheme) in approvers {
            assert_eq!(
                view.signer_schemes
                    .get(&word_hex(&commitment))
                    .map(String::as_str),
                Some(scheme.as_str())
            );
        }
        let overridden = Word::from(account.code().procedures()[1]);
        assert_eq!(
            view.procedure_thresholds,
            [(word_hex(&overridden), 1)].into()
        );
        assert_code_fields(&view, &account);
    }

    #[test]
    fn an_override_on_the_auth_procedure_changes_nothing_and_is_left_out() {
        let approvers = [(key(1), Falcon), (key(2), Falcon)];
        let account =
            guarded_multisig_with_overrides([4; 32], 2, &approvers, (key(7), Falcon), |plain| {
                vec![(plain.code().procedures()[0], 1)]
            });
        let view = account_view(&account.to_json()).unwrap();
        assert!(view.procedure_thresholds.is_empty());
    }
}
