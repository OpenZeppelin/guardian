use std::collections::BTreeSet;
use std::error::Error;
use std::future::Future;
use std::sync::Arc;

use guardian_shared::retry::{
    ProductionRetryRuntime, RPC_TRANSPORT_SIGNALS, RetryPolicy, RetryRuntime, StructuredEvidence,
    connect_failure_is_permanent, is_transient_error_with, run_retries,
};
use miden_client::note_transport::{
    NoteInfo, NoteStream, NoteTransportClient, NoteTransportCursor, NoteTransportError,
};
use miden_client::rpc::domain::account::{AccountProof, GetAccountRequest};
use miden_client::rpc::domain::account_vault::AccountVaultInfo;
use miden_client::rpc::domain::limits::RpcLimits;
use miden_client::rpc::domain::note::{FetchedNote, SyncNotesBlock};
use miden_client::rpc::domain::nullifier::NullifierUpdate;
use miden_client::rpc::domain::storage_map::StorageMapInfo;
use miden_client::rpc::domain::sync::{ChainMmrInfo, SyncTarget};
use miden_client::rpc::domain::transaction::TransactionRecord;
use miden_client::rpc::encryption::{AttestedTransactionEncryptionKey, SealedTransactionInputs};
use miden_client::rpc::{GrpcError, NetworkNoteStatusInfo, NodeRpcClient, RpcError, RpcStatusInfo};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::address::NetworkId;
use miden_protocol::batch::{ProposedBatch, ProvenBatch};
use miden_protocol::block::{BlockHeader, BlockNumber, ProvenBlock};
use miden_protocol::crypto::merkle::mmr::MmrProof;
use miden_protocol::note::NoteHeader;
use miden_protocol::note::{NoteId, NoteScript, NoteTag};
use miden_protocol::transaction::ProvenTransaction;

use crate::error::{MultisigError, Result, rpc_kind};

const DEFAULT_MAX_ATTEMPTS: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRetryPolicy {
    inner: RetryPolicy,
}

impl Default for RpcRetryPolicy {
    fn default() -> Self {
        Self {
            inner: RetryPolicy::new(DEFAULT_MAX_ATTEMPTS),
        }
    }
}

impl RpcRetryPolicy {
    #[must_use]
    pub fn new(max_attempts: u32) -> Self {
        Self {
            inner: RetryPolicy::new(max_attempts),
        }
    }

    #[must_use]
    pub fn max_attempts(&self) -> u32 {
        self.inner.max_attempts()
    }

    pub(crate) fn as_retry_policy(&self) -> &RetryPolicy {
        &self.inner
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RpcConfig {
    timeout_ms: Option<u64>,
    retry_policy: RpcRetryPolicy,
}

impl RpcConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Result<Self> {
        if timeout_ms == 0 {
            return Err(MultisigError::InvalidConfig(
                "rpc timeout must be a positive number of milliseconds".to_string(),
            ));
        }
        self.timeout_ms = Some(timeout_ms);
        Ok(self)
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: RpcRetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    #[must_use]
    pub fn timeout_ms(&self) -> Option<u64> {
        self.timeout_ms
    }

    #[must_use]
    pub fn retry_policy(&self) -> &RpcRetryPolicy {
        &self.retry_policy
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RpcSelection {
    Passthrough,
    Configured {
        timeout_ms: u64,
        retry_policy: RpcRetryPolicy,
    },
}

impl RpcConfig {
    pub(crate) fn resolve(&self, default_timeout_ms: u64) -> RpcSelection {
        if self.timeout_ms.is_none() && self.retry_policy.max_attempts() == 1 {
            return RpcSelection::Passthrough;
        }
        RpcSelection::Configured {
            timeout_ms: self.timeout_ms.unwrap_or(default_timeout_ms),
            retry_policy: self.retry_policy.clone(),
        }
    }
}

fn grpc_error_evidence(kind: &GrpcError) -> StructuredEvidence {
    match kind {
        GrpcError::Cancelled
        | GrpcError::DeadlineExceeded
        | GrpcError::ResourceExhausted
        | GrpcError::Unavailable => StructuredEvidence::Transient,
        GrpcError::NotFound
        | GrpcError::InvalidArgument
        | GrpcError::PermissionDenied
        | GrpcError::AlreadyExists
        | GrpcError::FailedPrecondition
        | GrpcError::Internal
        | GrpcError::Unimplemented
        | GrpcError::Unauthenticated
        | GrpcError::Aborted
        | GrpcError::OutOfRange
        | GrpcError::DataLoss => StructuredEvidence::Permanent,
        GrpcError::Unknown(_) => StructuredEvidence::Indeterminate,
    }
}

fn rpc_link_evidence(cause: &(dyn Error + 'static)) -> StructuredEvidence {
    cause
        .downcast_ref::<RpcError>()
        .and_then(rpc_kind)
        .map(grpc_error_evidence)
        .unwrap_or(StructuredEvidence::Indeterminate)
}

pub(crate) fn is_transient_rpc_error(error: &RpcError) -> bool {
    is_transient_error_with(error, rpc_link_evidence, &RPC_TRANSPORT_SIGNALS)
}

/// `Connection` wraps endpoint parsing, TLS configuration, and actual
/// connect failures indiscriminately; only the last class is worth another
/// attempt, so the wrapped chain decides.
fn note_transport_link_evidence(cause: &(dyn Error + 'static)) -> StructuredEvidence {
    match cause.downcast_ref::<NoteTransportError>() {
        Some(NoteTransportError::Connection(inner)) => {
            if connect_failure_is_permanent(inner.as_ref()) {
                StructuredEvidence::Permanent
            } else {
                StructuredEvidence::Transient
            }
        }
        Some(
            NoteTransportError::Disabled
            | NoteTransportError::Deserialization(_)
            | NoteTransportError::PaginationDidNotTerminate(_),
        ) => StructuredEvidence::Permanent,
        Some(NoteTransportError::Network(_)) | None => StructuredEvidence::Indeterminate,
    }
}

pub(crate) fn is_transient_note_transport_error(error: &NoteTransportError) -> bool {
    is_transient_error_with(error, note_transport_link_evidence, &RPC_TRANSPORT_SIGNALS)
}

pub(crate) struct RetryingNodeRpcClient {
    inner: Arc<dyn NodeRpcClient>,
    policy: RetryPolicy,
    runtime: Arc<dyn RetryRuntime>,
}

impl RetryingNodeRpcClient {
    pub(crate) fn new(inner: Arc<dyn NodeRpcClient>, policy: &RpcRetryPolicy) -> Self {
        Self {
            inner,
            policy: policy.as_retry_policy().clone(),
            runtime: Arc::new(ProductionRetryRuntime),
        }
    }

    #[cfg(test)]
    fn with_runtime(
        inner: Arc<dyn NodeRpcClient>,
        policy: &RpcRetryPolicy,
        runtime: Arc<dyn RetryRuntime>,
    ) -> Self {
        Self {
            inner,
            policy: policy.as_retry_policy().clone(),
            runtime,
        }
    }

    /// Idempotent reads retry under the configured policy. Submissions never
    /// route through here — they delegate to the inner client directly.
    async fn execute<T, F, Fut>(&self, op: F) -> std::result::Result<T, RpcError>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: Future<Output = std::result::Result<T, RpcError>> + Send,
    {
        run_retries(
            self.policy.max_attempts(),
            self.runtime.as_ref(),
            is_transient_rpc_error,
            |_, _| {},
            op,
        )
        .await
    }
}

#[async_trait::async_trait]
impl NodeRpcClient for RetryingNodeRpcClient {
    async fn set_genesis_commitment(&self, commitment: Word) -> std::result::Result<(), RpcError> {
        self.execute(|| self.inner.set_genesis_commitment(commitment))
            .await
    }

    fn has_genesis_commitment(&self) -> Option<Word> {
        self.inner.has_genesis_commitment()
    }

    async fn get_transaction_encryption_key(
        &self,
    ) -> std::result::Result<AttestedTransactionEncryptionKey, RpcError> {
        self.execute(|| self.inner.get_transaction_encryption_key())
            .await
    }

    /// Never retried, regardless of the policy: re-sending a submission whose
    /// outcome is unknown could execute it twice.
    async fn submit_proven_transaction(
        &self,
        proven_transaction: ProvenTransaction,
        sealed_transaction_inputs: SealedTransactionInputs,
    ) -> std::result::Result<BlockNumber, RpcError> {
        self.inner
            .submit_proven_transaction(proven_transaction, sealed_transaction_inputs)
            .await
    }

    /// Never retried, regardless of the policy: re-sending a submission whose
    /// outcome is unknown could execute it twice.
    async fn submit_proven_batch(
        &self,
        proven_batch: ProvenBatch,
        proposed_batch: ProposedBatch,
        transaction_inputs: Vec<SealedTransactionInputs>,
    ) -> std::result::Result<BlockNumber, RpcError> {
        self.inner
            .submit_proven_batch(proven_batch, proposed_batch, transaction_inputs)
            .await
    }

    async fn get_block_header_by_number(
        &self,
        block_num: Option<BlockNumber>,
        include_mmr_proof: bool,
    ) -> std::result::Result<(BlockHeader, Option<MmrProof>), RpcError> {
        self.execute(|| {
            self.inner
                .get_block_header_by_number(block_num, include_mmr_proof)
        })
        .await
    }

    async fn get_block_by_number(
        &self,
        block_num: BlockNumber,
        include_proof: bool,
    ) -> std::result::Result<ProvenBlock, RpcError> {
        self.execute(|| self.inner.get_block_by_number(block_num, include_proof))
            .await
    }

    async fn get_notes_by_id(
        &self,
        note_ids: &[NoteId],
    ) -> std::result::Result<Vec<FetchedNote>, RpcError> {
        self.execute(|| self.inner.get_notes_by_id(note_ids)).await
    }

    async fn sync_chain_mmr(
        &self,
        current_block_height: BlockNumber,
        upper_bound: SyncTarget,
    ) -> std::result::Result<ChainMmrInfo, RpcError> {
        self.execute(|| self.inner.sync_chain_mmr(current_block_height, upper_bound))
            .await
    }

    async fn sync_notes(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        note_tags: &BTreeSet<NoteTag>,
    ) -> std::result::Result<Vec<SyncNotesBlock>, RpcError> {
        self.execute(|| self.inner.sync_notes(block_from, block_to, note_tags))
            .await
    }

    async fn sync_nullifiers(
        &self,
        prefix: &[u16],
        block_from: BlockNumber,
        block_to: BlockNumber,
    ) -> std::result::Result<Vec<NullifierUpdate>, RpcError> {
        self.execute(|| self.inner.sync_nullifiers(prefix, block_from, block_to))
            .await
    }

    async fn get_account(
        &self,
        account_id: AccountId,
        request: GetAccountRequest,
    ) -> std::result::Result<(BlockNumber, AccountProof), RpcError> {
        self.execute(|| self.inner.get_account(account_id, request.clone()))
            .await
    }

    async fn get_note_script_by_root(
        &self,
        root: Word,
    ) -> std::result::Result<Option<NoteScript>, RpcError> {
        self.execute(|| self.inner.get_note_script_by_root(root))
            .await
    }

    async fn sync_storage_maps(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        account_id: AccountId,
    ) -> std::result::Result<StorageMapInfo, RpcError> {
        self.execute(|| {
            self.inner
                .sync_storage_maps(block_from, block_to, account_id)
        })
        .await
    }

    async fn sync_account_vault(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        account_id: AccountId,
    ) -> std::result::Result<AccountVaultInfo, RpcError> {
        self.execute(|| {
            self.inner
                .sync_account_vault(block_from, block_to, account_id)
        })
        .await
    }

    async fn sync_transactions(
        &self,
        block_from: BlockNumber,
        block_to: BlockNumber,
        account_ids: Vec<AccountId>,
    ) -> std::result::Result<Vec<TransactionRecord>, RpcError> {
        self.execute(|| {
            self.inner
                .sync_transactions(block_from, block_to, account_ids.clone())
        })
        .await
    }

    async fn get_network_id(&self) -> std::result::Result<NetworkId, RpcError> {
        self.execute(|| self.inner.get_network_id()).await
    }

    async fn get_rpc_limits(&self) -> std::result::Result<RpcLimits, RpcError> {
        self.execute(|| self.inner.get_rpc_limits()).await
    }

    fn has_rpc_limits(&self) -> Option<RpcLimits> {
        self.inner.has_rpc_limits()
    }

    async fn set_rpc_limits(&self, limits: RpcLimits) {
        self.inner.set_rpc_limits(limits).await;
    }

    async fn get_status_unversioned(&self) -> std::result::Result<RpcStatusInfo, RpcError> {
        self.execute(|| self.inner.get_status_unversioned()).await
    }

    async fn get_network_note_status(
        &self,
        note_id: NoteId,
    ) -> std::result::Result<NetworkNoteStatusInfo, RpcError> {
        self.execute(|| self.inner.get_network_note_status(note_id))
            .await
    }
}

/// Wraps a note transport in the read-retry decorator when the policy asks
/// for more than one attempt; a single-attempt policy passes the transport
/// through untouched.
pub(crate) fn configured_note_transport_client(
    inner: Arc<dyn NoteTransportClient>,
    rpc_config: &RpcConfig,
) -> Arc<dyn NoteTransportClient> {
    if rpc_config.retry_policy().max_attempts() > 1 {
        Arc::new(RetryingNoteTransportClient::new(
            inner,
            rpc_config.retry_policy(),
        ))
    } else {
        inner
    }
}

pub(crate) struct RetryingNoteTransportClient {
    inner: Arc<dyn NoteTransportClient>,
    policy: RetryPolicy,
    runtime: Arc<dyn RetryRuntime>,
}

impl RetryingNoteTransportClient {
    pub(crate) fn new(inner: Arc<dyn NoteTransportClient>, policy: &RpcRetryPolicy) -> Self {
        Self {
            inner,
            policy: policy.as_retry_policy().clone(),
            runtime: Arc::new(ProductionRetryRuntime),
        }
    }

    #[cfg(test)]
    fn with_runtime(
        inner: Arc<dyn NoteTransportClient>,
        policy: &RpcRetryPolicy,
        runtime: Arc<dyn RetryRuntime>,
    ) -> Self {
        Self {
            inner,
            policy: policy.as_retry_policy().clone(),
            runtime,
        }
    }

    async fn retry_fetch<T, F, Fut>(&self, op: F) -> std::result::Result<T, NoteTransportError>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: Future<Output = std::result::Result<T, NoteTransportError>> + Send,
    {
        run_retries(
            self.policy.max_attempts(),
            self.runtime.as_ref(),
            is_transient_note_transport_error,
            |_, _| {},
            op,
        )
        .await
    }
}

#[async_trait::async_trait]
impl NoteTransportClient for RetryingNoteTransportClient {
    /// Never retried in-call: the client's relay outbox already re-sends
    /// undelivered notes on later syncs, and an in-call resend could deliver
    /// the same note twice.
    async fn send_note(
        &self,
        header: NoteHeader,
        details: Vec<u8>,
    ) -> std::result::Result<(), NoteTransportError> {
        self.inner.send_note(header, details).await
    }

    async fn fetch_notes(
        &self,
        tag: &[NoteTag],
        cursor: NoteTransportCursor,
    ) -> std::result::Result<(Vec<NoteInfo>, NoteTransportCursor), NoteTransportError> {
        self.retry_fetch(|| self.inner.fetch_notes(tag, cursor))
            .await
    }

    async fn stream_notes(
        &self,
        tag: NoteTag,
        cursor: NoteTransportCursor,
    ) -> std::result::Result<Box<dyn NoteStream>, NoteTransportError> {
        self.retry_fetch(|| self.inner.stream_notes(tag, cursor))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use miden_client::rpc::RpcEndpoint;

    use super::*;

    fn request_error(kind: GrpcError) -> RpcError {
        RpcError::RequestError {
            endpoint: RpcEndpoint::GetAccount,
            error_kind: kind,
            endpoint_error: None,
            source: None,
        }
    }

    #[test]
    fn typed_grpc_kinds_partition_into_retryable_and_permanent() {
        let transient = [
            GrpcError::Cancelled,
            GrpcError::DeadlineExceeded,
            GrpcError::ResourceExhausted,
            GrpcError::Unavailable,
        ];
        for kind in transient {
            assert!(is_transient_rpc_error(&request_error(kind)));
        }

        let permanent = [
            GrpcError::NotFound,
            GrpcError::InvalidArgument,
            GrpcError::PermissionDenied,
            GrpcError::AlreadyExists,
            GrpcError::FailedPrecondition,
            GrpcError::Internal,
            GrpcError::Unimplemented,
            GrpcError::Unauthenticated,
            GrpcError::Aborted,
            GrpcError::OutOfRange,
            GrpcError::DataLoss,
        ];
        for kind in permanent {
            assert!(!is_transient_rpc_error(&request_error(kind)));
        }
    }

    #[test]
    fn unknown_is_retryable_only_when_the_connection_failed() {
        let transport = request_error(GrpcError::Unknown(
            "transport error: code: 'Unknown error', message: \"transport error\", source: \
             tonic::transport::Error(Transport, hyper::Error(Io, Kind(TimedOut)))"
                .to_string(),
        ));
        assert!(is_transient_rpc_error(&transport));

        let io_timeout = request_error(GrpcError::Unknown(
            "connection error: desc = \"i/o timeout\"".to_string(),
        ));
        assert!(is_transient_rpc_error(&io_timeout));

        let fault = request_error(GrpcError::Unknown(
            "internal invariant violated".to_string(),
        ));
        assert!(!is_transient_rpc_error(&fault));
    }

    #[test]
    fn connection_error_with_transport_source_is_retryable() {
        let error = RpcError::ConnectionError(Box::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "transport error: i/o timeout",
        )));
        assert!(is_transient_rpc_error(&error));
    }

    #[test]
    fn deserialization_failures_are_not_retried() {
        assert!(!is_transient_rpc_error(&RpcError::DeserializationError(
            "unexpected field".to_string()
        )));
    }

    #[test]
    fn timeout_config_rejects_zero() {
        assert!(RpcConfig::new().with_timeout_ms(0).is_err());
        assert_eq!(
            RpcConfig::new().with_timeout_ms(1).unwrap().timeout_ms(),
            Some(1)
        );
    }

    #[test]
    fn attempt_budget_vectors_match_contract() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fixtures {
            attempt_budgets: Vec<AttemptBudget>,
        }
        #[derive(serde::Deserialize)]
        struct AttemptBudget {
            input: Option<u32>,
            normalized: u32,
        }
        let fixtures: Fixtures = serde_json::from_str(include_str!(
            "../../../fixtures/miden-multisig-client/rpc-policy-fixtures.json"
        ))
        .expect("fixtures must parse");
        for fixture in fixtures.attempt_budgets {
            let policy = fixture.input.map(RpcRetryPolicy::new).unwrap_or_default();
            assert_eq!(policy.max_attempts(), fixture.normalized);
        }
    }

    #[test]
    fn default_config_resolves_to_one_configured_retry() {
        assert_eq!(
            RpcConfig::new().resolve(10_000),
            RpcSelection::Configured {
                timeout_ms: 10_000,
                retry_policy: RpcRetryPolicy::new(2),
            }
        );
    }

    #[test]
    fn an_explicit_single_attempt_policy_opts_out_entirely() {
        assert_eq!(
            RpcConfig::new()
                .with_retry_policy(RpcRetryPolicy::new(1))
                .resolve(10_000),
            RpcSelection::Passthrough
        );
    }

    #[test]
    fn configured_timeout_or_retries_resolve_to_configured() {
        assert_eq!(
            RpcConfig::new()
                .with_timeout_ms(5_000)
                .unwrap()
                .resolve(10_000),
            RpcSelection::Configured {
                timeout_ms: 5_000,
                retry_policy: RpcRetryPolicy::default(),
            }
        );
        assert_eq!(
            RpcConfig::new()
                .with_retry_policy(RpcRetryPolicy::new(3))
                .resolve(10_000),
            RpcSelection::Configured {
                timeout_ms: 10_000,
                retry_policy: RpcRetryPolicy::new(3),
            }
        );
    }

    #[derive(Default)]
    struct RecordingRuntime {
        sleeps: Mutex<Vec<Duration>>,
    }

    #[async_trait::async_trait]
    impl RetryRuntime for RecordingRuntime {
        async fn sleep(&self, duration: Duration) {
            self.sleeps.lock().unwrap().push(duration);
        }

        fn unit_random(&self) -> f64 {
            0.5
        }
    }

    struct ScriptedNetworkIdInner {
        failures_before_success: AtomicU32,
        calls: AtomicU32,
        error: fn() -> RpcError,
    }

    #[async_trait::async_trait]
    impl NodeRpcClient for ScriptedNetworkIdInner {
        async fn get_network_id(&self) -> std::result::Result<NetworkId, RpcError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let remaining = self.failures_before_success.load(Ordering::SeqCst);
            if remaining > 0 {
                self.failures_before_success
                    .store(remaining - 1, Ordering::SeqCst);
                return Err((self.error)());
            }
            Ok(NetworkId::Devnet)
        }

        fn has_genesis_commitment(&self) -> Option<Word> {
            None
        }
        fn has_rpc_limits(&self) -> Option<RpcLimits> {
            None
        }
        async fn set_rpc_limits(&self, _: RpcLimits) {}
        async fn set_genesis_commitment(&self, _: Word) -> std::result::Result<(), RpcError> {
            unimplemented!()
        }
        async fn get_transaction_encryption_key(
            &self,
        ) -> std::result::Result<AttestedTransactionEncryptionKey, RpcError> {
            unimplemented!()
        }
        async fn submit_proven_transaction(
            &self,
            _: ProvenTransaction,
            _: SealedTransactionInputs,
        ) -> std::result::Result<BlockNumber, RpcError> {
            unimplemented!()
        }
        async fn submit_proven_batch(
            &self,
            _: ProvenBatch,
            _: ProposedBatch,
            _: Vec<SealedTransactionInputs>,
        ) -> std::result::Result<BlockNumber, RpcError> {
            unimplemented!()
        }
        async fn get_block_header_by_number(
            &self,
            _: Option<BlockNumber>,
            _: bool,
        ) -> std::result::Result<(BlockHeader, Option<MmrProof>), RpcError> {
            unimplemented!()
        }
        async fn get_block_by_number(
            &self,
            _: BlockNumber,
            _: bool,
        ) -> std::result::Result<ProvenBlock, RpcError> {
            unimplemented!()
        }
        async fn get_notes_by_id(
            &self,
            _: &[NoteId],
        ) -> std::result::Result<Vec<FetchedNote>, RpcError> {
            unimplemented!()
        }
        async fn sync_chain_mmr(
            &self,
            _: BlockNumber,
            _: SyncTarget,
        ) -> std::result::Result<ChainMmrInfo, RpcError> {
            unimplemented!()
        }
        async fn sync_notes(
            &self,
            _: BlockNumber,
            _: BlockNumber,
            _: &BTreeSet<NoteTag>,
        ) -> std::result::Result<Vec<SyncNotesBlock>, RpcError> {
            unimplemented!()
        }
        async fn sync_nullifiers(
            &self,
            _: &[u16],
            _: BlockNumber,
            _: BlockNumber,
        ) -> std::result::Result<Vec<NullifierUpdate>, RpcError> {
            unimplemented!()
        }
        async fn get_account(
            &self,
            _: AccountId,
            _: GetAccountRequest,
        ) -> std::result::Result<(BlockNumber, AccountProof), RpcError> {
            unimplemented!()
        }
        async fn get_note_script_by_root(
            &self,
            _: Word,
        ) -> std::result::Result<Option<NoteScript>, RpcError> {
            unimplemented!()
        }
        async fn sync_storage_maps(
            &self,
            _: BlockNumber,
            _: BlockNumber,
            _: AccountId,
        ) -> std::result::Result<StorageMapInfo, RpcError> {
            unimplemented!()
        }
        async fn sync_account_vault(
            &self,
            _: BlockNumber,
            _: BlockNumber,
            _: AccountId,
        ) -> std::result::Result<AccountVaultInfo, RpcError> {
            unimplemented!()
        }
        async fn sync_transactions(
            &self,
            _: BlockNumber,
            _: BlockNumber,
            _: Vec<AccountId>,
        ) -> std::result::Result<Vec<TransactionRecord>, RpcError> {
            unimplemented!()
        }
        async fn get_rpc_limits(&self) -> std::result::Result<RpcLimits, RpcError> {
            unimplemented!()
        }
        async fn get_status_unversioned(&self) -> std::result::Result<RpcStatusInfo, RpcError> {
            unimplemented!()
        }
        async fn get_network_note_status(
            &self,
            _: NoteId,
        ) -> std::result::Result<NetworkNoteStatusInfo, RpcError> {
            unimplemented!()
        }
    }

    fn rate_limit_error() -> RpcError {
        RpcError::RequestError {
            endpoint: RpcEndpoint::SyncChainMmr,
            error_kind: GrpcError::ResourceExhausted,
            endpoint_error: None,
            source: None,
        }
    }

    #[tokio::test]
    async fn a_rate_limited_read_retries_until_it_succeeds() {
        let inner = Arc::new(ScriptedNetworkIdInner {
            failures_before_success: AtomicU32::new(2),
            calls: AtomicU32::new(0),
            error: rate_limit_error,
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNodeRpcClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::new(3),
            runtime.clone(),
        );

        let network = client.get_network_id().await.unwrap();

        assert_eq!(network, NetworkId::Devnet);
        assert_eq!(inner.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            runtime.sleeps.lock().unwrap().as_slice(),
            [Duration::from_millis(500), Duration::from_millis(1000)]
        );
    }

    #[tokio::test]
    async fn an_exhausted_budget_returns_the_final_upstream_error_unchanged() {
        let inner = Arc::new(ScriptedNetworkIdInner {
            failures_before_success: AtomicU32::new(10),
            calls: AtomicU32::new(0),
            error: rate_limit_error,
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNodeRpcClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::new(2),
            runtime.clone(),
        );

        let error = client.get_network_id().await.unwrap_err();

        assert!(matches!(
            error,
            RpcError::RequestError {
                error_kind: GrpcError::ResourceExhausted,
                ..
            }
        ));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_permanent_failure_does_not_retry_or_sleep() {
        let inner = Arc::new(ScriptedNetworkIdInner {
            failures_before_success: AtomicU32::new(10),
            calls: AtomicU32::new(0),
            error: || request_error(GrpcError::InvalidArgument),
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNodeRpcClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::new(5),
            runtime.clone(),
        );

        client.get_network_id().await.unwrap_err();

        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        assert!(runtime.sleeps.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_default_policy_retries_a_transient_read_once() {
        let inner = Arc::new(ScriptedNetworkIdInner {
            failures_before_success: AtomicU32::new(1),
            calls: AtomicU32::new(0),
            error: rate_limit_error,
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNodeRpcClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::default(),
            runtime.clone(),
        );

        client.get_network_id().await.unwrap();

        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_explicit_single_attempt_policy_never_retries() {
        let inner = Arc::new(ScriptedNetworkIdInner {
            failures_before_success: AtomicU32::new(1),
            calls: AtomicU32::new(0),
            error: rate_limit_error,
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNodeRpcClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::new(1),
            runtime.clone(),
        );

        client.get_network_id().await.unwrap_err();

        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        assert!(runtime.sleeps.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrent_initializations_survive_a_rate_limit_window() {
        let runtime = Arc::new(RecordingRuntime::default());

        let clients: Vec<_> = (0..64)
            .map(|_| {
                let inner = Arc::new(ScriptedNetworkIdInner {
                    failures_before_success: AtomicU32::new(2),
                    calls: AtomicU32::new(0),
                    error: rate_limit_error,
                });
                RetryingNodeRpcClient::with_runtime(inner, &RpcRetryPolicy::new(4), runtime.clone())
            })
            .collect();

        let results =
            futures::future::join_all(clients.iter().map(|client| client.get_network_id())).await;

        assert_eq!(results.len(), 64);
        assert!(results.iter().all(|result| result.is_ok()));
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 128);
    }

    struct ScriptedNoteTransportInner {
        failures_before_success: AtomicU32,
        fetch_calls: AtomicU32,
        send_calls: AtomicU32,
        error: fn() -> NoteTransportError,
    }

    #[async_trait::async_trait]
    impl NoteTransportClient for ScriptedNoteTransportInner {
        async fn send_note(
            &self,
            _: NoteHeader,
            _: Vec<u8>,
        ) -> std::result::Result<(), NoteTransportError> {
            self.send_calls.fetch_add(1, Ordering::SeqCst);
            Err((self.error)())
        }

        async fn fetch_notes(
            &self,
            _: &[NoteTag],
            cursor: NoteTransportCursor,
        ) -> std::result::Result<(Vec<NoteInfo>, NoteTransportCursor), NoteTransportError> {
            self.fetch_calls.fetch_add(1, Ordering::SeqCst);
            let remaining = self.failures_before_success.load(Ordering::SeqCst);
            if remaining > 0 {
                self.failures_before_success
                    .store(remaining - 1, Ordering::SeqCst);
                return Err((self.error)());
            }
            Ok((Vec::new(), cursor))
        }

        async fn stream_notes(
            &self,
            _: NoteTag,
            _: NoteTransportCursor,
        ) -> std::result::Result<Box<dyn NoteStream>, NoteTransportError> {
            unimplemented!()
        }
    }

    fn test_note_header() -> NoteHeader {
        let sender = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let metadata = miden_protocol::note::NoteMetadata::new(
            miden_protocol::note::PartialNoteMetadata::new(
                sender,
                miden_protocol::note::NoteType::Private,
            ),
            &miden_protocol::note::NoteAttachments::default(),
        );
        NoteHeader::new(
            miden_protocol::note::NoteDetailsCommitment::from_raw_commitments(
                Word::default(),
                Word::default(),
            ),
            metadata,
        )
    }

    fn note_fetch_timeout() -> NoteTransportError {
        NoteTransportError::Network(
            "Fetch notes failed: Status { code: Cancelled, message: \"Timeout expired\" }"
                .to_string(),
        )
    }

    #[test]
    fn note_transport_errors_classify_by_variant_and_text() {
        assert!(is_transient_note_transport_error(&note_fetch_timeout()));
        assert!(is_transient_note_transport_error(
            &NoteTransportError::Connection(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "i/o timeout",
            )))
        ));
        assert!(!is_transient_note_transport_error(
            &NoteTransportError::Disabled
        ));
        assert!(!is_transient_note_transport_error(
            &NoteTransportError::Network("note not recognized by relay".to_string())
        ));
        assert!(!is_transient_note_transport_error(
            &NoteTransportError::PaginationDidNotTerminate(64)
        ));
    }

    #[tokio::test]
    async fn a_timed_out_note_fetch_retries_until_it_succeeds() {
        let inner = Arc::new(ScriptedNoteTransportInner {
            failures_before_success: AtomicU32::new(2),
            fetch_calls: AtomicU32::new(0),
            send_calls: AtomicU32::new(0),
            error: note_fetch_timeout,
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNoteTransportClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::new(3),
            runtime.clone(),
        );

        client
            .fetch_notes(&[], NoteTransportCursor::from(0))
            .await
            .unwrap();

        assert_eq!(inner.fetch_calls.load(Ordering::SeqCst), 3);
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_note_send_is_never_retried_even_when_transient() {
        let inner = Arc::new(ScriptedNoteTransportInner {
            failures_before_success: AtomicU32::new(10),
            fetch_calls: AtomicU32::new(0),
            send_calls: AtomicU32::new(0),
            error: note_fetch_timeout,
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let client = RetryingNoteTransportClient::with_runtime(
            inner.clone(),
            &RpcRetryPolicy::new(5),
            runtime.clone(),
        );

        let error = client
            .send_note(test_note_header(), Vec::new())
            .await
            .unwrap_err();

        assert!(is_transient_note_transport_error(&error));
        assert_eq!(inner.send_calls.load(Ordering::SeqCst), 1);
        assert!(runtime.sleeps.lock().unwrap().is_empty());
    }

    #[test]
    fn connection_failures_classify_by_wrapped_cause() {
        let refused = NoteTransportError::Connection(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        )));
        assert!(is_transient_note_transport_error(&refused));

        let bad_cert = NoteTransportError::Connection(Box::new(std::io::Error::other(
            "invalid peer certificate: UnknownIssuer",
        )));
        assert!(!is_transient_note_transport_error(&bad_cert));

        let bad_uri = NoteTransportError::Connection(Box::new(std::io::Error::other(
            "transport error: invalid URI",
        )));
        assert!(!is_transient_note_transport_error(&bad_uri));

        let tls_config = NoteTransportError::Connection(Box::new(std::io::Error::other(
            "tls config error: no roots",
        )));
        assert!(!is_transient_note_transport_error(&tls_config));
    }

    #[tokio::test]
    async fn configured_note_transport_installs_retries_only_when_asked() {
        let retried_inner = Arc::new(ScriptedNoteTransportInner {
            failures_before_success: AtomicU32::new(1),
            fetch_calls: AtomicU32::new(0),
            send_calls: AtomicU32::new(0),
            error: note_fetch_timeout,
        });
        let wrapped = configured_note_transport_client(
            retried_inner.clone(),
            &RpcConfig::new().with_retry_policy(RpcRetryPolicy::new(2)),
        );
        wrapped
            .fetch_notes(&[], NoteTransportCursor::from(0))
            .await
            .unwrap();
        assert_eq!(retried_inner.fetch_calls.load(Ordering::SeqCst), 2);

        let passthrough_inner = Arc::new(ScriptedNoteTransportInner {
            failures_before_success: AtomicU32::new(1),
            fetch_calls: AtomicU32::new(0),
            send_calls: AtomicU32::new(0),
            error: note_fetch_timeout,
        });
        let passthrough = configured_note_transport_client(
            passthrough_inner.clone(),
            &RpcConfig::new().with_retry_policy(RpcRetryPolicy::new(1)),
        );
        passthrough
            .fetch_notes(&[], NoteTransportCursor::from(0))
            .await
            .unwrap_err();
        assert_eq!(passthrough_inner.fetch_calls.load(Ordering::SeqCst), 1);

        let send_inner = Arc::new(ScriptedNoteTransportInner {
            failures_before_success: AtomicU32::new(10),
            fetch_calls: AtomicU32::new(0),
            send_calls: AtomicU32::new(0),
            error: note_fetch_timeout,
        });
        let wrapped_send = configured_note_transport_client(
            send_inner.clone(),
            &RpcConfig::new().with_retry_policy(RpcRetryPolicy::new(5)),
        );
        wrapped_send
            .send_note(test_note_header(), Vec::new())
            .await
            .unwrap_err();
        assert_eq!(send_inner.send_calls.load(Ordering::SeqCst), 1);
    }
}
