//! Guardian's own [`DataStore`]: it answers the executor from the account state Guardian holds
//! and the chain view of one attempt, instead of a synced client store.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use miden_client::rpc::NodeRpcClient;
use miden_protocol::Word;
use miden_protocol::account::{
    Account, AccountId, PartialAccount, StorageMapKey, StorageMapWitness, StorageSlotContent,
};
use miden_protocol::asset::{AssetId, AssetWitness};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::note::{Note, NoteScript, NoteScriptRoot};
use miden_protocol::protocol_config::ProtocolConfig;
use miden_protocol::transaction::{AccountInputs, PartialBlockchain};
use miden_protocol::vm::FutureMaybeSend;
use miden_tx::{
    DataStore, DataStoreError, LoadedMastForest, MastForestStore, TransactionMastStore,
};

use super::chain::ChainView;
use super::foreign::{ForeignAccountUnavailable, ForeignAccounts};

/// Everything the executor may ask for during one execution attempt.
pub struct ExecutionDataStore {
    account: Account,
    chain: ChainView,
    foreign: ForeignAccounts,
    rpc: Arc<dyn NodeRpcClient>,
    mast_store: TransactionMastStore,
    note_scripts: BTreeMap<NoteScriptRoot, NoteScript>,
}

impl ExecutionDataStore {
    /// `input_notes` supplies the scripts of the notes the transaction consumes.
    pub fn new(
        account: Account,
        chain: ChainView,
        rpc: Arc<dyn NodeRpcClient>,
        input_notes: &[Note],
    ) -> Self {
        // `load_account_code` also registers the libraries the account code reaches through
        // external nodes: the multisig and guardian procedures are linked dynamically, so they
        // resolve through the store rather than being embedded in the account code.
        let mast_store = TransactionMastStore::new();
        mast_store.load_account_code(account.code());

        let mut note_scripts = BTreeMap::new();
        for note in input_notes {
            let script = note.script().clone();
            mast_store.insert(script.mast());
            note_scripts.insert(script.root(), script);
        }

        Self {
            foreign: ForeignAccounts::new(rpc.clone(), chain.reference_block()),
            account,
            chain,
            rpc,
            mast_store,
            note_scripts,
        }
    }

    pub fn chain(&self) -> &ChainView {
        &self.chain
    }

    /// The first foreign account the attempt could not load, if execution failed on one.
    pub fn foreign_failure(&self) -> Option<ForeignAccountUnavailable> {
        self.foreign.failure()
    }

    /// Time spent reading foreign accounts from the node so far in this attempt.
    pub fn foreign_fetch_time(&self) -> std::time::Duration {
        self.foreign.fetch_time()
    }

    /// Starts a new execution's record of foreign-account failures. The loaded accounts are
    /// kept: they are pinned to the same reference block.
    pub fn begin_execution(&self) {
        self.foreign.clear_failure();
    }

    fn is_own(&self, account_id: AccountId) -> bool {
        account_id == self.account.id()
    }

    fn transaction_inputs(
        &self,
        account_id: AccountId,
        ref_blocks: &BTreeSet<BlockNumber>,
    ) -> Result<
        (
            PartialAccount,
            BlockHeader,
            ProtocolConfig,
            PartialBlockchain,
        ),
        DataStoreError,
    > {
        if !self.is_own(account_id) {
            return Err(DataStoreError::AccountNotFound(account_id));
        }
        let reference = self.chain.reference_block();
        if let Some(highest) = ref_blocks.last()
            && *highest != reference
        {
            return Err(DataStoreError::other(format!(
                "executor asked for reference block {highest}, the attempt executes at {reference}"
            )));
        }
        if let Some(missing) = ref_blocks
            .iter()
            .copied()
            .find(|block| !self.chain.authenticates(*block))
        {
            return Err(DataStoreError::BlockNotFound(missing));
        }
        Ok((
            PartialAccount::from(&self.account),
            self.chain.reference_header().clone(),
            self.chain.protocol_config().clone(),
            self.chain.blockchain().clone(),
        ))
    }

    fn own_vault_witnesses(
        &self,
        vault_root: Word,
        asset_ids: BTreeSet<AssetId>,
    ) -> Result<Vec<AssetWitness>, DataStoreError> {
        let vault = self.account.vault();
        if vault.root() != vault_root {
            return Err(DataStoreError::other(format!(
                "vault root mismatch: executor asked for {vault_root}, account is at {}",
                vault.root()
            )));
        }
        // `AssetVault::open` yields a witness whether or not the asset is present, so proofs of
        // absence work too.
        Ok(asset_ids.into_iter().map(|id| vault.open(id)).collect())
    }

    fn own_storage_map_witness(
        &self,
        map_root: Word,
        map_key: StorageMapKey,
    ) -> Result<StorageMapWitness, DataStoreError> {
        self.account
            .storage()
            .slots()
            .iter()
            .find_map(|slot| match slot.content() {
                StorageSlotContent::Map(map) if map.root() == map_root => Some(map.open(&map_key)),
                _ => None,
            })
            .ok_or_else(|| {
                DataStoreError::other(format!(
                    "account {} has no storage map with root {map_root}",
                    self.account.id().to_hex()
                ))
            })
    }
}

impl DataStore for ExecutionDataStore {
    fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        ref_blocks: BTreeSet<BlockNumber>,
    ) -> impl FutureMaybeSend<
        Result<
            (
                PartialAccount,
                BlockHeader,
                ProtocolConfig,
                PartialBlockchain,
            ),
            DataStoreError,
        >,
    > {
        async move { self.transaction_inputs(account_id, &ref_blocks) }
    }

    fn get_foreign_account_inputs(
        &self,
        foreign_account_id: AccountId,
        ref_block: BlockNumber,
    ) -> impl FutureMaybeSend<Result<AccountInputs, DataStoreError>> {
        async move {
            let inputs = self
                .foreign
                .inputs(foreign_account_id, ref_block)
                .await
                .map_err(|failure| {
                    DataStoreError::other(format!("foreign account unavailable: {failure:?}"))
                })?;
            self.mast_store.load_account_code(inputs.code());
            Ok(inputs)
        }
    }

    fn get_vault_asset_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        asset_ids: BTreeSet<AssetId>,
    ) -> impl FutureMaybeSend<Result<Vec<AssetWitness>, DataStoreError>> {
        async move {
            if self.is_own(account_id) {
                return self.own_vault_witnesses(vault_root, asset_ids);
            }
            self.foreign
                .vault_witnesses(account_id, vault_root, asset_ids)
                .ok_or_else(|| {
                    DataStoreError::other(format!(
                        "no loaded vault with root {vault_root} for foreign account {}",
                        account_id.to_hex()
                    ))
                })
        }
    }

    fn get_storage_map_witness(
        &self,
        account_id: AccountId,
        map_root: Word,
        map_key: StorageMapKey,
    ) -> impl FutureMaybeSend<Result<StorageMapWitness, DataStoreError>> {
        async move {
            if self.is_own(account_id) {
                return self.own_storage_map_witness(map_root, map_key);
            }
            self.foreign
                .storage_map_witness(account_id, map_root, map_key)
                .ok_or_else(|| {
                    DataStoreError::other(format!(
                        "no loaded storage map with root {map_root} for foreign account {}",
                        account_id.to_hex()
                    ))
                })
        }
    }

    fn get_note_script(
        &self,
        script_root: NoteScriptRoot,
    ) -> impl FutureMaybeSend<Result<Option<NoteScript>, DataStoreError>> {
        async move {
            if let Some(script) = self.note_scripts.get(&script_root) {
                return Ok(Some(script.clone()));
            }
            self.rpc
                .get_note_script_by_root(script_root.into())
                .await
                .map_err(|e| {
                    DataStoreError::other_with_source(
                        format!("failed to fetch note script {script_root}"),
                        e,
                    )
                })
        }
    }
}

impl MastForestStore for ExecutionDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<LoadedMastForest> {
        self.mast_store.get(procedure_hash)
    }
}
