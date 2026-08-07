//! GUARDIAN's own [`DataStore`] implementation (Gate 0 spike for issue #254).
//!
//! GUARDIAN holds the complete account state, so it answers the executor's queries
//! directly rather than through a synced client store. State is built per execution
//! and discarded when the execution reaches a terminal state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use miden_protocol::account::{
    Account, AccountId, PartialAccount, StorageMapKey, StorageMapWitness, StorageSlotContent,
};
use miden_protocol::asset::{AssetVaultKey, AssetWitness};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::note::{Note, NoteScript, NoteScriptRoot};
use miden_protocol::transaction::{AccountInputs, PartialBlockchain};
use miden_protocol::vm::FutureMaybeSend;
use miden_protocol::{MastForest, Word};
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};

/// Everything the executor may ask for during one transaction execution.
///
/// The reference block is always the chain tip observed when the execution started, so
/// the reference block equals the blockchain view's height. That is what keeps the
/// [`PartialBlockchain`] valid.
pub struct ExecutionDataStore {
    account_id: AccountId,
    account: Account,
    ref_block: BlockHeader,
    blockchain: PartialBlockchain,
    mast_store: TransactionMastStore,
    note_scripts: BTreeMap<NoteScriptRoot, NoteScript>,
}

impl ExecutionDataStore {
    /// Builds the per-execution state from the account GUARDIAN holds plus chain data
    /// read at the tip. `input_notes` supplies the scripts for notes the transaction
    /// consumes; GUARDIAN carries them in the proposal payload.
    pub fn new(
        account: Account,
        ref_block: BlockHeader,
        blockchain: PartialBlockchain,
        input_notes: &[Note],
    ) -> Result<Self, String> {
        // `load_account_code` registers the account's own procedures *and* the
        // transaction kernel / component libraries its code reaches by `ExternalNode`.
        // Registering only `account.code().mast()` is not enough: the multisig and
        // guardian libraries are dynamically linked, so their procedures resolve
        // through the store rather than being embedded in the account code.
        let mast_store = TransactionMastStore::new();
        mast_store.load_account_code(account.code());

        let mut note_scripts = BTreeMap::new();
        for note in input_notes {
            let script = note.script().clone();
            mast_store.insert(script.mast());
            note_scripts.insert(script.root(), script);
        }

        Ok(Self {
            account_id: account.id(),
            account,
            ref_block,
            blockchain,
            mast_store,
            note_scripts,
        })
    }

    fn ensure_own_account(&self, account_id: AccountId) -> Result<(), DataStoreError> {
        if account_id == self.account_id {
            Ok(())
        } else {
            Err(DataStoreError::AccountNotFound(account_id))
        }
    }
}

impl DataStore for ExecutionDataStore {
    fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        ref_blocks: BTreeSet<BlockNumber>,
    ) -> impl FutureMaybeSend<Result<(PartialAccount, BlockHeader, PartialBlockchain), DataStoreError>>
    {
        async move {
            self.ensure_own_account(account_id)?;

            if let Some(highest) = ref_blocks.iter().max()
                && *highest > self.ref_block.block_num()
            {
                return Err(DataStoreError::BlockNotFound(*highest));
            }

            Ok((
                PartialAccount::from(&self.account),
                self.ref_block.clone(),
                self.blockchain.clone(),
            ))
        }
    }

    fn get_foreign_account_inputs(
        &self,
        foreign_account_id: AccountId,
        _ref_block: BlockNumber,
    ) -> impl FutureMaybeSend<Result<AccountInputs, DataStoreError>> {
        async move {
            Err(DataStoreError::other(format!(
                "foreign account inputs are not supported by GUARDIAN-side execution \
                 (requested {})",
                foreign_account_id.to_hex()
            )))
        }
    }

    fn get_vault_asset_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        vault_keys: BTreeSet<AssetVaultKey>,
    ) -> impl FutureMaybeSend<Result<Vec<AssetWitness>, DataStoreError>> {
        async move {
            self.ensure_own_account(account_id)?;

            let vault = self.account.vault();
            if vault.root() != vault_root {
                return Err(DataStoreError::other(format!(
                    "vault root mismatch: executor asked for {vault_root}, account is at {}",
                    vault.root()
                )));
            }

            // `AssetVault::open` yields a witness whether or not the key is present, so
            // proofs of non-inclusion work. This is why the vault SMT is opened directly
            // rather than through miden-client's `AccountSmtForest`, whose
            // `get_asset_and_witness` rejects absent assets outright.
            Ok(vault_keys.into_iter().map(|key| vault.open(key)).collect())
        }
    }

    fn get_storage_map_witness(
        &self,
        account_id: AccountId,
        map_root: Word,
        map_key: StorageMapKey,
    ) -> impl FutureMaybeSend<Result<StorageMapWitness, DataStoreError>> {
        async move {
            self.ensure_own_account(account_id)?;

            self.account
                .storage()
                .slots()
                .iter()
                .find_map(|slot| match slot.content() {
                    StorageSlotContent::Map(map) if map.root() == map_root => {
                        Some(map.open(&map_key))
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    DataStoreError::other(format!(
                        "account {} has no storage map with root {map_root}",
                        self.account_id.to_hex()
                    ))
                })
        }
    }

    fn get_note_script(
        &self,
        script_root: NoteScriptRoot,
    ) -> impl FutureMaybeSend<Result<Option<NoteScript>, DataStoreError>> {
        async move { Ok(self.note_scripts.get(&script_root).cloned()) }
    }
}

impl MastForestStore for ExecutionDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<Arc<MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
