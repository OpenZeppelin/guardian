//! Foreign accounts an execution loads, read from the node at the attempt's reference block.
//!
//! On 0.17 these are on the ordinary path: moving an asset whose faucet sets the callback flag
//! opens a foreign context against the faucet (devnet's fee faucet does, on every fee payment),
//! and fee sponsorship prices network notes through foreign procedure invocation. Only public
//! accounts can be served; their state is read at the reference block so it authenticates
//! against the same account root the kernel checks, and is never cached across executions.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use miden_client::rpc::domain::account::{GetAccountRequest, StorageMapFetch, VaultFetch};
use miden_client::rpc::{AccountStateAt, NodeRpcClient};
use miden_protocol::Word;
use miden_protocol::account::{
    Account, AccountId, PartialAccount, StorageMapKey, StorageMapWitness, StorageSlotContent,
};
use miden_protocol::asset::{AssetId, AssetWitness};
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::AccountInputs;

/// Why a foreign account could not be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignAccountUnavailable {
    Private {
        account_id: AccountId,
    },
    Unavailable {
        account_id: AccountId,
        reason: String,
    },
}

/// Loads and holds the foreign accounts of one execution attempt.
pub struct ForeignAccounts {
    rpc: Arc<dyn NodeRpcClient>,
    reference_block: BlockNumber,
    loaded: Mutex<BTreeMap<AccountId, (Account, AccountInputs)>>,
    failure: Mutex<Option<ForeignAccountUnavailable>>,
}

impl ForeignAccounts {
    pub fn new(rpc: Arc<dyn NodeRpcClient>, reference_block: BlockNumber) -> Self {
        Self {
            rpc,
            reference_block,
            loaded: Mutex::new(BTreeMap::new()),
            failure: Mutex::new(None),
        }
    }

    /// The first foreign account this attempt could not serve, if any. The executor only sees
    /// an opaque data-store error, so the typed cause is kept here for the caller.
    pub fn failure(&self) -> Option<ForeignAccountUnavailable> {
        self.failure.lock().expect("foreign failure lock").clone()
    }

    /// Forgets the recorded failure, so the next execution reports only its own.
    pub fn clear_failure(&self) {
        *self.failure.lock().expect("foreign failure lock") = None;
    }

    /// The account's inputs at the reference block, fetched on first use.
    pub async fn inputs(
        &self,
        account_id: AccountId,
        ref_block: BlockNumber,
    ) -> Result<AccountInputs, ForeignAccountUnavailable> {
        let result = self.load(account_id, ref_block).await;
        if let Err(failure) = &result {
            self.failure
                .lock()
                .expect("foreign failure lock")
                .get_or_insert_with(|| failure.clone());
        }
        result
    }

    pub fn vault_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        asset_ids: impl IntoIterator<Item = AssetId>,
    ) -> Option<Vec<AssetWitness>> {
        let loaded = self.loaded.lock().expect("foreign account lock");
        let vault = loaded.get(&account_id)?.0.vault();
        (vault.root() == vault_root)
            .then(|| asset_ids.into_iter().map(|id| vault.open(id)).collect())
    }

    pub fn storage_map_witness(
        &self,
        account_id: AccountId,
        map_root: Word,
        map_key: StorageMapKey,
    ) -> Option<StorageMapWitness> {
        let loaded = self.loaded.lock().expect("foreign account lock");
        loaded
            .get(&account_id)?
            .0
            .storage()
            .slots()
            .iter()
            .find_map(|slot| match slot.content() {
                StorageSlotContent::Map(map) if map.root() == map_root => Some(map.open(&map_key)),
                _ => None,
            })
    }

    async fn load(
        &self,
        account_id: AccountId,
        ref_block: BlockNumber,
    ) -> Result<AccountInputs, ForeignAccountUnavailable> {
        let unavailable =
            |reason: String| ForeignAccountUnavailable::Unavailable { account_id, reason };
        if ref_block != self.reference_block {
            return Err(unavailable(format!(
                "requested at block {ref_block}, the attempt executes at {}",
                self.reference_block
            )));
        }
        if !account_id.is_public() {
            return Err(ForeignAccountUnavailable::Private { account_id });
        }
        if let Some((_, inputs)) = self
            .loaded
            .lock()
            .expect("foreign account lock")
            .get(&account_id)
        {
            return Ok(inputs.clone());
        }

        let (served_at, mut proof) = self
            .rpc
            .get_account(
                account_id,
                GetAccountRequest::new()
                    .with_storage(StorageMapFetch::All)
                    .with_vault(VaultFetch::Always)
                    .at(AccountStateAt::Block(self.reference_block)),
            )
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        if served_at != self.reference_block {
            return Err(unavailable(format!(
                "node served the account at block {served_at} instead of {}",
                self.reference_block
            )));
        }
        if let Some(details) = proof.details_mut() {
            self.rpc
                .resolve_oversize_vault(account_id, served_at, details)
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            self.rpc
                .resolve_oversize_storage_maps(account_id, served_at, details)
                .await
                .map_err(|e| unavailable(e.to_string()))?;
        }
        let (witness, details) = proof.into_parts();
        let details =
            details.ok_or_else(|| unavailable("node returned no public state".to_string()))?;
        let account = Account::try_from(&details).map_err(|e| unavailable(e.to_string()))?;
        if account.to_commitment() != witness.state_commitment() {
            return Err(unavailable(
                "served account state does not match its witness".to_string(),
            ));
        }
        let inputs = AccountInputs::new(PartialAccount::from(&account), witness);
        self.loaded
            .lock()
            .expect("foreign account lock")
            .insert(account_id, (account, inputs.clone()));
        Ok(inputs)
    }
}
