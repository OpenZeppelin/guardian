//! A mock node for the execution tests: the client's `MockRpcApi`, plus what a real node does that
//! the mock does not (serve an attested transaction encryption key), and the timing of submissions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use miden_client::rpc::domain::account::{AccountProof, GetAccountRequest};
use miden_client::rpc::domain::account_vault::AccountVaultInfo;
use miden_client::rpc::domain::note::{FetchedNote, SyncNotesBlock};
use miden_client::rpc::domain::nullifier::NullifierUpdate;
use miden_client::rpc::domain::storage_map::StorageMapInfo;
use miden_client::rpc::domain::sync::{ChainMmrInfo, SyncTarget};
use miden_client::rpc::domain::transaction::{TransactionLogsInfo, TransactionRecord};
use miden_client::rpc::encryption::{AttestedTransactionEncryptionKey, SealedTransactionInputs};
use miden_client::rpc::{NetworkNoteStatusInfo, NodeRpcClient, RpcError, RpcLimits, RpcStatusInfo};
use miden_client::testing::mock::MockRpcApi;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::address::NetworkId;
use miden_protocol::batch::{ProposedBatch, ProvenBatch};
use miden_protocol::block::{BlockHeader, BlockNumber, SignedBlock};
use miden_protocol::crypto::merkle::mmr::MmrProof;
use miden_protocol::note::{NoteId, NoteScript, NoteTag};
use miden_protocol::protocol_config::ProtocolConfig;
use miden_protocol::transaction::{ProvenTransaction, TransactionId};
use miden_protocol::vm::ExecutionProof;
use tokio::time::Instant;

pub(super) struct TestNode {
    pub inner: Arc<MockRpcApi>,
    /// Served in place of the mock's own key, as a real node serves its validator's.
    key: Option<AttestedTransactionEncryptionKey>,
    /// How long the node takes before it answers a batch submission.
    batch_latency: Mutex<Duration>,
    /// How long the node takes before it answers a chain sync (how a chain view reads the tip).
    sync_latency: Mutex<Duration>,
    /// When each single-transaction submission reached the node.
    single_sends: Mutex<Vec<Instant>>,
}

impl TestNode {
    pub fn new(inner: Arc<MockRpcApi>) -> Self {
        Self {
            inner,
            key: None,
            batch_latency: Mutex::new(Duration::ZERO),
            sync_latency: Mutex::new(Duration::ZERO),
            single_sends: Mutex::new(Vec::new()),
        }
    }

    /// A node that serves `key`, attested by the chain's validator.
    pub fn serving_key(inner: Arc<MockRpcApi>, key: AttestedTransactionEncryptionKey) -> Self {
        Self {
            key: Some(key),
            ..Self::new(inner)
        }
    }

    /// Answers every later batch submission only after `latency`.
    pub fn answer_batches_after(&self, latency: Duration) {
        *self
            .batch_latency
            .lock()
            .expect("the test node's lock is never poisoned") = latency;
    }

    /// Answers every later chain sync only after `latency`.
    pub fn answer_chain_syncs_after(&self, latency: Duration) {
        *self
            .sync_latency
            .lock()
            .expect("the test node's lock is never poisoned") = latency;
    }

    /// When each single-transaction submission so far reached the node.
    pub fn single_sends(&self) -> Vec<Instant> {
        self.single_sends
            .lock()
            .expect("the test node's lock is never poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl NodeRpcClient for TestNode {
    async fn set_genesis_commitment(&self, commitment: Word) -> Result<(), RpcError> {
        self.inner.set_genesis_commitment(commitment).await
    }
    async fn get_genesis_block_header(&self) -> Result<(BlockHeader, ProtocolConfig), RpcError> {
        self.inner.get_genesis_block_header().await
    }
    async fn get_transaction_logs(
        &self,
        account_id: AccountId,
        transaction_id: TransactionId,
    ) -> Result<Option<TransactionLogsInfo>, RpcError> {
        self.inner
            .get_transaction_logs(account_id, transaction_id)
            .await
    }
    async fn get_transaction_encryption_key(
        &self,
    ) -> Result<AttestedTransactionEncryptionKey, RpcError> {
        match &self.key {
            Some(key) => Ok(key.clone()),
            None => self.inner.get_transaction_encryption_key().await,
        }
    }
    async fn submit_proven_transaction(
        &self,
        proven_transaction: &ProvenTransaction,
        sealed: SealedTransactionInputs,
    ) -> Result<BlockNumber, RpcError> {
        self.single_sends
            .lock()
            .expect("the test node's lock is never poisoned")
            .push(Instant::now());
        self.inner
            .submit_proven_transaction(proven_transaction, sealed)
            .await
    }
    async fn submit_proven_batch(
        &self,
        proven_batch: &ProvenBatch,
        proposed_batch: &ProposedBatch,
        transaction_inputs: Vec<SealedTransactionInputs>,
    ) -> Result<BlockNumber, RpcError> {
        let latency = *self
            .batch_latency
            .lock()
            .expect("the test node's lock is never poisoned");
        tokio::time::sleep(latency).await;
        self.inner
            .submit_proven_batch(proven_batch, proposed_batch, transaction_inputs)
            .await
    }
    async fn register_account(
        &self,
        invitation_code: &str,
        account_id: AccountId,
    ) -> Result<(), RpcError> {
        self.inner
            .register_account(invitation_code, account_id)
            .await
    }
    async fn is_account_allowed(&self, account_id: AccountId) -> Result<bool, RpcError> {
        self.inner.is_account_allowed(account_id).await
    }
    async fn is_invitation_code_valid(&self, invitation_code: &str) -> Result<bool, RpcError> {
        self.inner.is_invitation_code_valid(invitation_code).await
    }
    async fn get_block_header_by_number(
        &self,
        block_num: Option<BlockNumber>,
        include_mmr_proof: bool,
    ) -> Result<(BlockHeader, Option<MmrProof>), RpcError> {
        self.inner
            .get_block_header_by_number(block_num, include_mmr_proof)
            .await
    }
    async fn get_block_by_number(
        &self,
        block_num: BlockNumber,
        include_proof: bool,
    ) -> Result<(SignedBlock, Option<ExecutionProof>), RpcError> {
        self.inner
            .get_block_by_number(block_num, include_proof)
            .await
    }
    async fn get_notes_by_id(&self, note_ids: &[NoteId]) -> Result<Vec<FetchedNote>, RpcError> {
        self.inner.get_notes_by_id(note_ids).await
    }
    async fn sync_chain_mmr(
        &self,
        current_block_height: BlockNumber,
        upper_bound: SyncTarget,
    ) -> Result<ChainMmrInfo, RpcError> {
        let latency = *self
            .sync_latency
            .lock()
            .expect("the test node's lock is never poisoned");
        tokio::time::sleep(latency).await;
        self.inner
            .sync_chain_mmr(current_block_height, upper_bound)
            .await
    }
    async fn sync_notes(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        note_tags: &std::collections::BTreeSet<NoteTag>,
    ) -> Result<Vec<SyncNotesBlock>, RpcError> {
        self.inner.sync_notes(block_from, block_to, note_tags).await
    }
    async fn sync_nullifiers(
        &self,
        prefix: &[u16],
        block_from: BlockNumber,
        block_to: BlockNumber,
    ) -> Result<Vec<NullifierUpdate>, RpcError> {
        self.inner
            .sync_nullifiers(prefix, block_from, block_to)
            .await
    }
    async fn get_account(
        &self,
        request: GetAccountRequest,
    ) -> Result<(BlockNumber, AccountProof), RpcError> {
        self.inner.get_account(request).await
    }
    async fn get_note_script_by_root(&self, root: Word) -> Result<Option<NoteScript>, RpcError> {
        self.inner.get_note_script_by_root(root).await
    }
    async fn sync_storage_maps(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        account_id: AccountId,
    ) -> Result<StorageMapInfo, RpcError> {
        self.inner
            .sync_storage_maps(block_from, block_to, account_id)
            .await
    }
    async fn sync_account_vault(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        account_id: AccountId,
    ) -> Result<AccountVaultInfo, RpcError> {
        self.inner
            .sync_account_vault(block_from, block_to, account_id)
            .await
    }
    async fn sync_transactions(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        account_ids: Vec<AccountId>,
    ) -> Result<Vec<TransactionRecord>, RpcError> {
        self.inner
            .sync_transactions(block_from, block_to, account_ids)
            .await
    }
    async fn get_network_id(&self) -> Result<NetworkId, RpcError> {
        self.inner.get_network_id().await
    }
    async fn get_rpc_limits(&self) -> Result<RpcLimits, RpcError> {
        self.inner.get_rpc_limits().await
    }
    fn has_rpc_limits(&self) -> Option<RpcLimits> {
        self.inner.has_rpc_limits()
    }
    async fn set_rpc_limits(&self, limits: RpcLimits) {
        self.inner.set_rpc_limits(limits).await
    }
    async fn get_status_unversioned(&self) -> Result<RpcStatusInfo, RpcError> {
        self.inner.get_status_unversioned().await
    }
    async fn get_network_note_status(
        &self,
        note_id: NoteId,
    ) -> Result<NetworkNoteStatusInfo, RpcError> {
        self.inner.get_network_note_status(note_id).await
    }
}
