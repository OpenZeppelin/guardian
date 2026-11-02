//! Minimal Miden RPC client over bindings generated from miden-node-proto-build
use std::sync::Arc;
use std::time::Duration;

use guardian_shared::retry::{
    ProductionRetryRuntime, RPC_TRANSPORT_SIGNALS, RetryPolicy, RetryRuntime, StructuredEvidence,
    connect_failure_is_permanent, grpc_code_evidence, is_transient_error_with, run_retries,
};
use miden_protocol::account::AccountId;
use tonic::{
    Request,
    transport::{Channel, ClientTlsConfig},
};

// Generated from the node's protos, which define messages this client never
// uses (for example `asset::AssetVault` since node-proto-build 0.17.0-rc.2).
#[allow(dead_code)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/rpc_generated.rs"));
}

pub use generated::miden::node::v1 as rpc;
pub use generated::{account, blockchain, note, primitives, submission, transaction};
pub use rpc::node_service_client::NodeServiceClient;

#[cfg(any(test, feature = "scripted-node"))]
pub mod test_node;

/// Per-request deadline applied to the channel. Without one, a hung
/// node holds a caller (and everything awaiting it) indefinitely —
/// concurrent callers share the multiplexed channel, so no request may
/// be allowed to wait forever. Matches the 10s default used by
/// miden-client and the multisig SDK, so every Miden RPC surface shares
/// one default deadline.
pub const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Startup connect resilience is bounded by fixed constants rather than
/// configuration: it holds no lease or lock, and the bound keeps a failed
/// boot fast enough for orchestrator restart loops (worst case ≈34s).
pub const CONNECT_MAX_ATTEMPTS: u32 = 5;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Largest response the client decodes: 15% above tonic's 4 MiB default,
/// as miden-client allows. The node fills a `SyncTransactions` page up to
/// 4 MiB by its own size estimate, which undercounts the encoded size, so
/// a full page can exceed the default limit and be refused every time.
pub const MAX_RESPONSE_SIZE_BYTES: usize = 4 * 1024 * 1024 * 115 / 100;

/// Largest committed block the client decodes, from `GetBlockByNumber` or
/// the `BlockSubscription` stream. A block carries every account patch and
/// note of up to a full set of batches, so it can be far larger than an
/// ordinary response. A block over the limit would be refused on every
/// retry and stop a feed consumer at that block for good, so the limit is
/// generous rather than tight.
pub const MAX_BLOCK_MESSAGE_SIZE_BYTES: usize = 64 * 1024 * 1024;

/// HTTP/2 keep-alive of the channel [`MidenRpcClient::connect_streaming`]
/// opens: a ping every 30 s, and the connection counts as dead when a ping
/// goes unanswered for 10 s. A long-lived stream has no request deadline,
/// so the keep-alive is what notices a peer that silently went away.
pub const STREAM_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
pub const STREAM_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Failure surface of [`MidenRpcClient`]. `Call` retains the typed
/// `tonic::Status` so transient and permanent failures stay distinguishable.
/// Endpoint values are never echoed in errors.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    #[error("invalid Miden RPC endpoint: {reason}")]
    InvalidEndpoint { reason: String },
    #[error("failed to load TLS roots for the Miden RPC endpoint: {0}")]
    Tls(#[source] tonic::transport::Error),
    #[error("failed to connect to the Miden RPC endpoint: {0}")]
    Connect(#[source] tonic::transport::Error),
    #[error("{operation} RPC failed: {status}")]
    Call {
        operation: &'static str,
        #[source]
        status: tonic::Status,
    },
    #[error("{operation} returned a malformed response: {reason}")]
    MalformedResponse {
        operation: &'static str,
        reason: String,
    },
    #[error("unsupported request: {0}")]
    UnsupportedRequest(String),
}

fn rpc_client_link_evidence(cause: &(dyn std::error::Error + 'static)) -> StructuredEvidence {
    if let Some(status) = cause.downcast_ref::<tonic::Status>() {
        return grpc_code_evidence(status.code() as i32);
    }
    if let Some(error) = cause.downcast_ref::<RpcClientError>() {
        return match error {
            RpcClientError::Connect(source) => {
                if connect_failure_is_permanent(source) {
                    StructuredEvidence::Permanent
                } else {
                    StructuredEvidence::Transient
                }
            }
            RpcClientError::InvalidEndpoint { .. }
            | RpcClientError::Tls(_)
            | RpcClientError::MalformedResponse { .. }
            | RpcClientError::UnsupportedRequest(_) => StructuredEvidence::Permanent,
            RpcClientError::Call { .. } => StructuredEvidence::Indeterminate,
        };
    }
    StructuredEvidence::Indeterminate
}

pub fn is_transient_rpc_client_error(error: &RpcClientError) -> bool {
    is_transient_error_with(error, rpc_client_link_evidence, &RPC_TRANSPORT_SIGNALS)
}

/// Node RPC transport settings. The default applies the shared
/// [`DEFAULT_RPC_TIMEOUT`] per-request deadline and no read retries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcClientSettings {
    timeout: Duration,
    read_retry: RetryPolicy,
}

impl Default for RpcClientSettings {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_RPC_TIMEOUT,
            read_retry: RetryPolicy::single_attempt(),
        }
    }
}

impl RpcClientSettings {
    #[must_use]
    pub fn new(timeout: Duration, read_retry: RetryPolicy) -> Self {
        Self {
            timeout,
            read_retry,
        }
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    #[must_use]
    pub fn read_retry(&self) -> &RetryPolicy {
        &self.read_retry
    }
}

pub use guardian_shared::retry::RpcReadMode;

/// Observer for retry attempts beyond the first, keyed by operation name.
/// Lets consumers surface retry activity (e.g. metrics) without this crate
/// depending on their telemetry stack.
pub type RetryObserver = Arc<dyn Fn(&'static str) + Send + Sync>;

/// Hex encoding of a node-reported word: its 32 canonical bytes,
/// byte-identical to `Word::as_bytes()` / `Word::to_bytes()` on the
/// protocol side, so node values compare directly against locally
/// computed commitments and storage words. `None` unless the payload is
/// exactly 32 bytes: proto3's default `Word` is *empty*, not the zero
/// word, so a length check is the only way to tell them apart.
pub fn word_to_hex(word: &primitives::Word) -> Option<String> {
    (word.encoded.len() == 32).then(|| format!("0x{}", hex::encode(&word.encoded)))
}

/// Simple wrapper around the tonic-generated NodeServiceClient
pub struct MidenRpcClient {
    client: NodeServiceClient<Channel>,
    settings: RpcClientSettings,
    runtime: Arc<dyn RetryRuntime>,
    retry_observer: Option<RetryObserver>,
}

impl MidenRpcClient {
    pub async fn connect(endpoint: impl Into<String>) -> Result<Self, RpcClientError> {
        Self::connect_with_settings(endpoint, RpcClientSettings::default()).await
    }

    pub async fn connect_with_settings(
        endpoint: impl Into<String>,
        settings: RpcClientSettings,
    ) -> Result<Self, RpcClientError> {
        Self::connect_with_runtime(endpoint, settings, Arc::new(ProductionRetryRuntime)).await
    }

    async fn connect_with_runtime(
        endpoint: impl Into<String>,
        settings: RpcClientSettings,
        runtime: Arc<dyn RetryRuntime>,
    ) -> Result<Self, RpcClientError> {
        let endpoint_str = endpoint.into();

        let base = Channel::from_shared(endpoint_str)
            .map_err(|e| RpcClientError::InvalidEndpoint {
                reason: e.to_string(),
            })?
            .timeout(settings.timeout())
            .connect_timeout(CONNECT_TIMEOUT)
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(RpcClientError::Tls)?;

        let channel = run_retries(
            CONNECT_MAX_ATTEMPTS,
            runtime.as_ref(),
            |error: &tonic::transport::Error| !connect_failure_is_permanent(error),
            |_, _| {},
            || async { base.connect().await },
        )
        .await
        .map_err(RpcClientError::Connect)?;

        Ok(Self {
            client: NodeServiceClient::new(channel)
                .max_decoding_message_size(MAX_RESPONSE_SIZE_BYTES),
            settings,
            runtime,
            retry_observer: None,
        })
    }

    /// Builds a client over a lazily-created channel that is never proactively
    /// connected and skips TLS root loading. This lets pure, non-RPC call paths
    /// be unit-tested without a network or a system certificate store; issuing
    /// an actual RPC on the resulting client will fail to connect.
    pub fn lazy_unconnected(endpoint: impl Into<String>) -> Result<Self, RpcClientError> {
        let channel = Channel::from_shared(endpoint.into())
            .map_err(|e| RpcClientError::InvalidEndpoint {
                reason: e.to_string(),
            })?
            .connect_lazy();

        Ok(Self {
            client: NodeServiceClient::new(channel)
                .max_decoding_message_size(MAX_RESPONSE_SIZE_BYTES),
            settings: RpcClientSettings::default(),
            runtime: Arc::new(ProductionRetryRuntime),
            retry_observer: None,
        })
    }

    /// Installs an observer invoked once per retry attempt beyond the first,
    /// with the operation name.
    pub fn set_retry_observer(&mut self, observer: RetryObserver) {
        self.retry_observer = Some(observer);
    }

    /// Get the underlying tonic NodeServiceClient for full access to all RPC methods:
    pub fn client_mut(&mut self) -> &mut NodeServiceClient<Channel> {
        &mut self.client
    }

    /// Idempotent reads retry under the configured policy; each attempt
    /// re-issues the same request on a cloned handle of the multiplexed
    /// channel.
    async fn retry_read<T, F, Fut>(
        &self,
        operation: &'static str,
        read_mode: RpcReadMode,
        op: F,
    ) -> Result<T, RpcClientError>
    where
        F: Fn(NodeServiceClient<Channel>) -> Fut,
        Fut: Future<Output = Result<T, tonic::Status>>,
    {
        let attempts = match read_mode {
            RpcReadMode::Configured => self.settings.read_retry().max_attempts(),
            RpcReadMode::SingleAttempt => 1,
        };
        run_retries(
            attempts,
            self.runtime.as_ref(),
            is_transient_rpc_client_error,
            |_, _| {
                if let Some(observer) = &self.retry_observer {
                    observer(operation);
                }
            },
            || async {
                op(self.client.clone())
                    .await
                    .map_err(|status| RpcClientError::Call { operation, status })
            },
        )
        .await
    }

    /// Get the status of the Miden node. Takes `&self` like
    /// [`Self::get_account_commitment`].
    pub async fn get_status(&self) -> Result<rpc::StatusResponse, RpcClientError> {
        self.retry_read(
            "get_status",
            RpcReadMode::Configured,
            |mut client| async move {
                client
                    .status(Request::new(rpc::StatusRequest {}))
                    .await
                    .map(tonic::Response::into_inner)
            },
        )
        .await
    }

    /// Get block header by number with optional MMR proof
    pub async fn get_block_header(
        &mut self,
        block_num: Option<u32>,
        include_mmr_proof: bool,
    ) -> Result<rpc::GetBlockHeaderByNumberResponse, RpcClientError> {
        self.retry_read(
            "get_block_header",
            RpcReadMode::Configured,
            |mut client| async move {
                let request = rpc::GetBlockHeaderByNumberRequest {
                    block_num,
                    include_mmr_proof: Some(include_mmr_proof),
                    include_protocol_config: None,
                };
                client
                    .get_block_header_by_number(Request::new(request))
                    .await
                    .map(tonic::Response::into_inner)
            },
        )
        .await
    }

    /// Submit a proven transaction to the network.
    ///
    /// Never retried, regardless of the configured read-retry policy: a
    /// submission whose outcome is unknown could execute twice if re-sent.
    ///
    /// Since Miden 0.17 the node takes the canonical structured message rather
    /// than opaque transaction bytes, so the caller assembles the submission.
    pub async fn submit_transaction(
        &mut self,
        submission: submission::ProvenTransactionSubmission,
    ) -> Result<(), RpcClientError> {
        self.client
            .submit_proven_tx(Request::new(rpc::SubmitProvenTxRequest {
                submission: Some(submission),
            }))
            .await
            .map_err(|status| RpcClientError::Call {
                operation: "submit_transaction",
                status,
            })?;

        Ok(())
    }

    /// Sync state for specified accounts and note tags
    pub async fn sync_state(
        &mut self,
        block_num: u32,
        account_ids: Vec<Vec<u8>>,
        note_tags: Vec<u32>,
    ) -> Result<rpc::SyncNotesResponse, RpcClientError> {
        if !account_ids.is_empty() {
            return Err(RpcClientError::UnsupportedRequest(
                "Account syncing moved out of the raw node RPC wrapper in Miden 0.14; use miden-client state sync APIs for account state".to_string(),
            ));
        }

        let note_tags = &note_tags;
        self.retry_read(
            "sync_state",
            RpcReadMode::Configured,
            |mut client| async move {
                let request = rpc::SyncNotesRequest {
                    block_range: Some(rpc::BlockRange {
                        block_from: block_num,
                        block_to: u32::MAX,
                    }),
                    note_tags: note_tags.clone(),
                };
                client
                    .sync_notes(Request::new(request))
                    .await
                    .map(tonic::Response::into_inner)
            },
        )
        .await
    }

    /// Get notes by their IDs
    pub async fn get_notes_by_id(
        &mut self,
        note_ids: Vec<primitives::Word>,
    ) -> Result<rpc::GetNotesByIdResponse, RpcClientError> {
        let note_ids: Vec<note::NoteId> = note_ids
            .into_iter()
            .map(|id| note::NoteId { id: Some(id) })
            .collect();

        let note_ids = &note_ids;
        self.retry_read(
            "get_notes_by_id",
            RpcReadMode::Configured,
            |mut client| async move {
                let request = rpc::GetNotesByIdRequest {
                    note_ids: note_ids.clone(),
                };
                client
                    .get_notes_by_id(Request::new(request))
                    .await
                    .map(tonic::Response::into_inner)
            },
        )
        .await
    }

    /// Fetch account commitment from the Miden network. Takes `&self`:
    /// the tonic client is cloned per call (a cheap handle onto the
    /// same multiplexed HTTP/2 channel), so concurrent callers never
    /// serialize on this client.
    pub async fn get_account_commitment(
        &self,
        account_id: &AccountId,
        read_mode: RpcReadMode,
    ) -> Result<String, RpcClientError> {
        self.get_observed_account_commitment(account_id, read_mode)
            .await
            .map(|(commitment, _)| commitment)
    }

    /// [`Self::get_account_commitment`] with the block the node read it at, both from one
    /// response, so the commitment is the account's state as of that block.
    pub async fn get_observed_account_commitment(
        &self,
        account_id: &AccountId,
        read_mode: RpcReadMode,
    ) -> Result<(String, u32), RpcClientError> {
        const OPERATION: &str = "get_account_commitment";
        let proto_account_id = proto_account_id(account_id);

        let proto_account_id = &proto_account_id;
        let account_response = self
            .retry_read(OPERATION, read_mode, |mut client| async move {
                let request = Request::new(rpc::GetAccountRequest {
                    account_id: Some(*proto_account_id),
                    block_num: None,
                    details: None,
                });
                client
                    .get_account(request)
                    .await
                    .map(tonic::Response::into_inner)
            })
            .await?;

        let witness =
            account_response
                .witness
                .ok_or_else(|| RpcClientError::MalformedResponse {
                    operation: OPERATION,
                    reason: "no witness in account response".to_string(),
                })?;

        let commitment = witness
            .commitment
            .ok_or_else(|| RpcClientError::MalformedResponse {
                operation: OPERATION,
                reason: "no commitment in witness".to_string(),
            })?;

        let observed_at = account_response
            .block_num
            .map(|number| number.block_num)
            .ok_or_else(|| RpcClientError::MalformedResponse {
                operation: OPERATION,
                reason: "no block number in account response".to_string(),
            })?;
        let commitment =
            word_to_hex(&commitment).ok_or_else(|| RpcClientError::MalformedResponse {
                operation: OPERATION,
                reason: format!(
                    "commitment has {} bytes, expected the 32 bytes of a word",
                    commitment.encoded.len()
                ),
            })?;
        Ok((commitment, observed_at))
    }

    /// Fetch the account witness together with the storage-map details
    /// selected by `details`, at the chain tip. The witness carries the
    /// on-chain state commitment observed at the same block as the
    /// details, so callers can tie a storage read to the commitment it
    /// belongs to. Details are only valid for public accounts; callers
    /// decide whether to request them. Takes `&self` like
    /// [`Self::get_account_commitment`] so concurrent callers never
    /// serialize on this client.
    pub async fn get_account_with_details(
        &self,
        account_id: &AccountId,
        details: Option<rpc::get_account_request::AccountDetailRequest>,
        read_mode: RpcReadMode,
    ) -> Result<rpc::GetAccountResponse, RpcClientError> {
        let proto_account_id = proto_account_id(account_id);

        let proto_account_id = &proto_account_id;
        let details = &details;
        self.retry_read(
            "get_account_with_details",
            read_mode,
            |mut client| async move {
                let request = Request::new(rpc::GetAccountRequest {
                    account_id: Some(*proto_account_id),
                    block_num: None,
                    details: details.clone(),
                });
                client
                    .get_account(request)
                    .await
                    .map(tonic::Response::into_inner)
            },
        )
        .await
    }

    /// The node's chain tip: the number of the latest block header it
    /// serves. Takes `&self` like [`Self::get_account_commitment`].
    pub async fn get_chain_tip(&self, read_mode: RpcReadMode) -> Result<u32, RpcClientError> {
        const OPERATION: &str = "get_chain_tip";
        let response = self
            .retry_read(OPERATION, read_mode, |mut client| async move {
                let request = rpc::GetBlockHeaderByNumberRequest {
                    block_num: None,
                    include_mmr_proof: Some(false),
                    include_protocol_config: None,
                };
                client
                    .get_block_header_by_number(Request::new(request))
                    .await
                    .map(tonic::Response::into_inner)
            })
            .await?;
        response
            .block_header
            .and_then(|header| header.block_num)
            .map(|number| number.block_num)
            .ok_or_else(|| RpcClientError::MalformedResponse {
                operation: OPERATION,
                reason: "no block number in the latest block header".to_string(),
            })
    }

    /// One page of the transactions committed against `account_id` in
    /// `block_from..=block_to`. The node records a header for every
    /// account, private ones included, never prunes them, and rejects a
    /// range that ends past its tip. It caps the payload and paginates
    /// by block: `pagination_info.block_num` is the last block the page
    /// fully covers, so a caller continues from the next block until it
    /// reaches `block_to`. Takes `&self` like
    /// [`Self::get_account_commitment`].
    pub async fn sync_transactions(
        &self,
        account_id: &AccountId,
        block_from: u32,
        block_to: u32,
        read_mode: RpcReadMode,
    ) -> Result<rpc::SyncTransactionsResponse, RpcClientError> {
        let proto_account_id = proto_account_id(account_id);

        let proto_account_id = &proto_account_id;
        self.retry_read("sync_transactions", read_mode, |mut client| async move {
            let request = rpc::SyncTransactionsRequest {
                block_range: Some(rpc::BlockRange {
                    block_from,
                    block_to,
                }),
                account_ids: vec![*proto_account_id],
            };
            client
                .sync_transactions(Request::new(request))
                .await
                .map(tonic::Response::into_inner)
        })
        .await
    }

    /// One committed block by number, without its proof. `None` when the
    /// node does not serve the block: not produced yet, or no longer
    /// retained. Takes `&self` like [`Self::get_account_commitment`].
    pub async fn get_block(
        &self,
        block_num: u32,
        read_mode: RpcReadMode,
    ) -> Result<Option<blockchain::SignedBlock>, RpcClientError> {
        let response = self
            .retry_read("get_block", read_mode, |client| async move {
                client
                    .max_decoding_message_size(MAX_BLOCK_MESSAGE_SIZE_BYTES)
                    .get_block_by_number(Request::new(rpc::GetBlockByNumberRequest {
                        block_num,
                        include_proof: Some(false),
                    }))
                    .await
                    .map(tonic::Response::into_inner)
            })
            .await?;
        Ok(response.block)
    }

    /// Connect a channel of its own for long-lived server streams. Unlike
    /// [`Self::connect`] it sets **no** per-request deadline (a deadline
    /// would cut the stream, not a hung call), keeps the connection alive
    /// with HTTP/2 pings instead, and connects once: a stream consumer owns
    /// its reconnect policy.
    pub async fn connect_streaming(
        endpoint: impl Into<String>,
    ) -> Result<StreamingClient, RpcClientError> {
        let channel = Channel::from_shared(endpoint.into())
            .map_err(|e| RpcClientError::InvalidEndpoint {
                reason: e.to_string(),
            })?
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(STREAM_KEEP_ALIVE_INTERVAL)
            .keep_alive_timeout(STREAM_KEEP_ALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(RpcClientError::Tls)?
            .connect()
            .await
            .map_err(RpcClientError::Connect)?;
        Ok(StreamingClient {
            client: NodeServiceClient::new(channel)
                .max_decoding_message_size(MAX_BLOCK_MESSAGE_SIZE_BYTES),
        })
    }

    /// Fetch full account details including serialized account data
    pub async fn get_account_details(
        &mut self,
        account_id: &AccountId,
    ) -> Result<rpc::GetAccountResponse, RpcClientError> {
        let proto_account_id = proto_account_id(account_id);

        let proto_account_id = &proto_account_id;
        self.retry_read(
            "get_account_details",
            RpcReadMode::Configured,
            |mut client| async move {
                let request = Request::new(rpc::GetAccountRequest {
                    account_id: Some(*proto_account_id),
                    block_num: None,
                    details: None,
                });
                client
                    .get_account(request)
                    .await
                    .map(tonic::Response::into_inner)
            },
        )
        .await
    }
}

/// The canonical wire form of an account ID: a versioned pair of felts rather
/// than the serialized bytes the pre-0.17 schema carried.
fn proto_account_id(account_id: &AccountId) -> account::AccountId {
    account::AccountId {
        version: Some(account::account_id::Version::V1(account::AccountIdV1 {
            suffix: Some(primitives::Felt {
                value: account_id.suffix().as_canonical_u64(),
            }),
            prefix: Some(primitives::Felt {
                value: account_id.prefix().as_felt().as_canonical_u64(),
            }),
        })),
    }
}

/// A node client over a channel without a request deadline, for server
/// streams (see [`MidenRpcClient::connect_streaming`]).
pub struct StreamingClient {
    client: NodeServiceClient<Channel>,
}

impl StreamingClient {
    /// Subscribe to committed blocks from `block_from` on: the node
    /// replays the blocks it already committed from there, then streams
    /// each new one as it commits. The node closes the stream with a
    /// status when it shuts down (`UNAVAILABLE`), when the subscriber
    /// falls too far behind (`RESOURCE_EXHAUSTED`, followed by a temporary
    /// ban of the caller), or on an internal error.
    pub async fn subscribe_blocks(
        &mut self,
        block_from: u32,
    ) -> Result<tonic::Streaming<rpc::BlockSubscriptionResponse>, RpcClientError> {
        self.client
            .block_subscription(Request::new(rpc::BlockSubscriptionRequest { block_from }))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| RpcClientError::Call {
                operation: "block_subscription",
                status,
            })
    }
}

/// The account ID a node message names: the inverse of the versioned felt
/// pair this client sends. `None` when the message carries no version, a
/// felt is missing or not canonical, or the pair is not a valid account ID.
pub fn account_id_from_proto(id: &account::AccountId) -> Option<AccountId> {
    let account::account_id::Version::V1(v1) = id.version.as_ref()?;
    let suffix = miden_protocol::Felt::try_from(v1.suffix?.value).ok()?;
    let prefix = miden_protocol::Felt::try_from(v1.prefix?.value).ok()?;
    AccountId::try_from_elements(suffix, prefix).ok()
}

/// One entry of a committed block's `updated_accounts`, reduced to what a
/// consumer of the block feed compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAccountState {
    pub account_id: AccountId,
    /// The commitment the account ended the block at, encoded like
    /// [`word_to_hex`].
    pub commitment: String,
    /// The update carries the account's public patch (a public account).
    /// A private account publishes its commitment only.
    pub is_public: bool,
}

/// What a committed block says about accounts: its number and the final
/// state of every account it updated, private accounts included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAccountUpdates {
    pub block_num: u32,
    pub updates: Vec<BlockAccountState>,
}

/// A committed block the node sent without something every block has.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockDecodeError {
    #[error("block has no header")]
    MissingHeader,
    #[error("block header has no block number")]
    MissingBlockNumber,
    #[error("block has no body")]
    MissingBody,
    #[error("account update {index} names no valid account ID")]
    InvalidAccountId { index: usize },
    #[error("account update {index} has no 32-byte final state commitment")]
    InvalidCommitment { index: usize },
}

/// Read the account updates of a committed block. Notes, nullifiers,
/// transactions and signatures are not inspected. A block missing its
/// header or body, or an update without a valid account ID or commitment,
/// is an error rather than a partial answer: a feed that skipped an update
/// would silently miss that account's move.
pub fn block_account_updates(
    block: &blockchain::SignedBlock,
) -> Result<BlockAccountUpdates, BlockDecodeError> {
    let header = block
        .header
        .as_ref()
        .ok_or(BlockDecodeError::MissingHeader)?;
    let block_num = header
        .block_num
        .ok_or(BlockDecodeError::MissingBlockNumber)?
        .block_num;
    let body = block.body.as_ref().ok_or(BlockDecodeError::MissingBody)?;
    let updates = body
        .updated_accounts
        .iter()
        .enumerate()
        .map(|(index, update)| {
            let account_id = update
                .account_id
                .as_ref()
                .and_then(account_id_from_proto)
                .ok_or(BlockDecodeError::InvalidAccountId { index })?;
            let commitment = update
                .final_state_commitment
                .as_ref()
                .and_then(word_to_hex)
                .ok_or(BlockDecodeError::InvalidCommitment { index })?;
            let is_public = matches!(
                update
                    .details
                    .as_ref()
                    .and_then(|details| details.update.as_ref()),
                Some(account::account_update_details::Update::Public(_))
            );
            Ok(BlockAccountState {
                account_id,
                commitment,
                is_public,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BlockAccountUpdates { block_num, updates })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use guardian_shared::retry::retry_delay;

    use super::*;

    fn call_error(code: tonic::Code) -> RpcClientError {
        RpcClientError::Call {
            operation: "get_account_commitment",
            status: tonic::Status::new(code, "status message"),
        }
    }

    #[test]
    fn typed_status_codes_partition_into_retryable_and_permanent() {
        let transient = [
            tonic::Code::Cancelled,
            tonic::Code::DeadlineExceeded,
            tonic::Code::ResourceExhausted,
            tonic::Code::Unavailable,
        ];
        for code in transient {
            assert!(is_transient_rpc_client_error(&call_error(code)), "{code:?}");
        }

        let permanent = [
            tonic::Code::InvalidArgument,
            tonic::Code::NotFound,
            tonic::Code::AlreadyExists,
            tonic::Code::PermissionDenied,
            tonic::Code::FailedPrecondition,
            tonic::Code::Aborted,
            tonic::Code::OutOfRange,
            tonic::Code::Unimplemented,
            tonic::Code::Internal,
            tonic::Code::DataLoss,
            tonic::Code::Unauthenticated,
        ];
        for code in permanent {
            assert!(
                !is_transient_rpc_client_error(&call_error(code)),
                "{code:?}"
            );
        }
    }

    #[test]
    fn unknown_is_retryable_only_with_transport_evidence() {
        let transport = RpcClientError::Call {
            operation: "sync_state",
            status: tonic::Status::unknown("connection error: desc = \"i/o timeout\""),
        };
        assert!(is_transient_rpc_client_error(&transport));

        let rate_limit_text = RpcClientError::Call {
            operation: "sync_state",
            status: tonic::Status::unknown("Too Many Requests!"),
        };
        assert!(is_transient_rpc_client_error(&rate_limit_text));

        let fault = RpcClientError::Call {
            operation: "sync_state",
            status: tonic::Status::unknown("internal invariant violated"),
        };
        assert!(!is_transient_rpc_client_error(&fault));
    }

    #[test]
    fn structural_errors_classify_by_variant() {
        assert!(!is_transient_rpc_client_error(
            &RpcClientError::InvalidEndpoint {
                reason: "bad uri".to_string(),
            }
        ));
        assert!(!is_transient_rpc_client_error(
            &RpcClientError::MalformedResponse {
                operation: "get_account_commitment",
                reason: "no witness in account response".to_string(),
            }
        ));
        assert!(!is_transient_rpc_client_error(
            &RpcClientError::UnsupportedRequest("account syncing".to_string())
        ));
    }

    #[test]
    fn connect_worst_case_stays_within_the_documented_bound() {
        let attempt_time = CONNECT_TIMEOUT * CONNECT_MAX_ATTEMPTS;
        let sleep_time: Duration = (0..CONNECT_MAX_ATTEMPTS - 1)
            .map(|retry_index| retry_delay(retry_index, 1.0))
            .sum();
        let worst_case = attempt_time + sleep_time;
        assert!(
            worst_case <= Duration::from_secs(35),
            "worst case {worst_case:?} exceeds the documented ~35s bound"
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

    fn scripted_client(read_retry: RetryPolicy) -> MidenRpcClient {
        let Ok(mut client) = MidenRpcClient::lazy_unconnected("http://127.0.0.1:1") else {
            panic!("lazy client construction is infallible for a valid endpoint");
        };
        client.settings = RpcClientSettings::new(DEFAULT_RPC_TIMEOUT, read_retry);
        client.runtime = Arc::new(RecordingRuntime::default());
        client
    }

    type ScriptedFuture =
        std::pin::Pin<Box<dyn Future<Output = Result<u32, tonic::Status>> + Send>>;

    fn scripted_failures(
        failures_before_success: u32,
        code: tonic::Code,
    ) -> (
        Arc<AtomicU32>,
        impl Fn(NodeServiceClient<Channel>) -> ScriptedFuture,
    ) {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let op = move |_client: NodeServiceClient<Channel>| {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            let fut: ScriptedFuture = Box::pin(async move {
                if attempt < failures_before_success {
                    Err(tonic::Status::new(code, "scripted failure"))
                } else {
                    Ok(attempt)
                }
            });
            fut
        };
        (calls, op)
    }

    #[tokio::test]
    async fn reads_retry_transient_failures_up_to_the_budget() {
        let mut client = scripted_client(RetryPolicy::new(3));
        let observed = Arc::new(AtomicU32::new(0));
        let counter = observed.clone();
        client.set_retry_observer(Arc::new(move |operation| {
            assert_eq!(operation, "get_status");
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        let (calls, op) = scripted_failures(2, tonic::Code::ResourceExhausted);
        let value = client
            .retry_read("get_status", RpcReadMode::Configured, op)
            .await
            .unwrap();

        assert_eq!(value, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(observed.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_exhausted_budget_returns_the_final_error() {
        let client = scripted_client(RetryPolicy::new(2));
        let (calls, op) = scripted_failures(10, tonic::Code::Unavailable);

        let error = client
            .retry_read("sync_state", RpcReadMode::Configured, op)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            RpcClientError::Call {
                operation: "sync_state",
                ..
            }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn permanent_failures_are_not_retried() {
        let client = scripted_client(RetryPolicy::new(5));
        let (calls, op) = scripted_failures(10, tonic::Code::InvalidArgument);

        client
            .retry_read("get_account_details", RpcReadMode::Configured, op)
            .await
            .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_single_attempt_policy_never_invokes_the_observer() {
        let mut client = scripted_client(RetryPolicy::single_attempt());
        let observed = Arc::new(AtomicU32::new(0));
        let counter = observed.clone();
        client.set_retry_observer(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        let (calls, op) = scripted_failures(1, tonic::Code::ResourceExhausted);
        client
            .retry_read("get_status", RpcReadMode::Configured, op)
            .await
            .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(observed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn submissions_fail_without_retry_even_when_transient() {
        let mut client = scripted_client(RetryPolicy::new(5));
        let observed = Arc::new(AtomicU32::new(0));
        let counter = observed.clone();
        client.set_retry_observer(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        let error = client
            .submit_transaction(submission::ProvenTransactionSubmission::default())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            RpcClientError::Call {
                operation: "submit_transaction",
                ..
            }
        ));
        assert_eq!(observed.load(Ordering::SeqCst), 0);
    }

    async fn wire_client(
        failures: u32,
        error: fn() -> tonic::Status,
        max_attempts: u32,
        timeout: Duration,
        delay: Duration,
    ) -> (MidenRpcClient, Arc<AtomicU32>, Arc<RecordingRuntime>) {
        let calls = Arc::new(AtomicU32::new(0));
        let node = test_node::ScriptedNode::failing(failures, error, calls.clone())
            .with_response_delay(delay);
        let endpoint = test_node::serve(node).await;
        let runtime = Arc::new(RecordingRuntime::default());
        let client = MidenRpcClient::connect_with_runtime(
            endpoint,
            RpcClientSettings::new(timeout, RetryPolicy::new(max_attempts)),
            runtime.clone(),
        )
        .await
        .expect("the scripted node must accept connections");
        (client, calls, runtime)
    }

    #[tokio::test]
    async fn a_rate_limited_read_recovers_over_a_real_wire() {
        let (client, calls, runtime) = wire_client(
            2,
            || tonic::Status::resource_exhausted("Too Many Requests!"),
            3,
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .await;

        let status = client.get_status().await.expect("third attempt succeeds");

        assert_eq!(status.version, "scripted");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_nodes_rate_limit_text_rendering_retries_over_a_real_wire() {
        let (client, calls, runtime) = wire_client(
            u32::MAX,
            || tonic::Status::unknown("Too Many Requests!"),
            2,
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .await;

        client.get_status().await.expect_err("budget exhausts");

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_permanent_status_fails_fast_over_a_real_wire() {
        let (client, calls, runtime) = wire_client(
            u32::MAX,
            || tonic::Status::invalid_argument("malformed account id"),
            3,
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .await;

        client.get_status().await.expect_err("permanent failure");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(runtime.sleeps.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_request_deadline_expiry_is_transient_over_a_real_wire() {
        let (client, calls, runtime) = wire_client(
            0,
            || unreachable!("the deadline expires before the scripted response"),
            2,
            Duration::from_millis(100),
            Duration::from_millis(500),
        )
        .await;

        let error = client
            .get_status()
            .await
            .expect_err("both attempts time out");

        assert!(is_transient_rpc_client_error(&error));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_dropped_connection_renders_transient_and_the_read_retries() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });

        let runtime = Arc::new(RecordingRuntime::default());
        let client = MidenRpcClient::connect_with_runtime(
            format!("http://{address}"),
            RpcClientSettings::new(Duration::from_secs(2), RetryPolicy::new(2)),
            runtime.clone(),
        )
        .await
        .expect("plain TCP accept satisfies the eager connect");

        let error = client
            .get_status()
            .await
            .expect_err("requests on a dropped connection must fail");

        assert!(is_transient_rpc_client_error(&error));
        assert_eq!(runtime.sleeps.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_invalid_endpoint_fails_immediately_without_connect_attempts() {
        let Err(error) = MidenRpcClient::connect("not a uri").await else {
            panic!("an invalid endpoint must fail");
        };
        assert!(matches!(error, RpcClientError::InvalidEndpoint { .. }));
    }

    #[tokio::test]
    async fn connect_retries_the_fixed_budget_against_a_down_node() {
        let runtime = Arc::new(RecordingRuntime::default());
        let result = MidenRpcClient::connect_with_runtime(
            "http://127.0.0.1:1",
            RpcClientSettings::default(),
            runtime.clone(),
        )
        .await;

        let Err(error) = result else {
            panic!("connecting to a closed port must fail");
        };
        assert!(matches!(error, RpcClientError::Connect(_)));
        assert!(is_transient_rpc_client_error(&error));
        assert_eq!(
            runtime.sleeps.lock().unwrap().len(),
            (CONNECT_MAX_ATTEMPTS - 1) as usize
        );
    }

    #[tokio::test]
    async fn single_attempt_mode_ignores_the_configured_budget() {
        let mut client = scripted_client(RetryPolicy::new(5));
        let observed = Arc::new(AtomicU32::new(0));
        let counter = observed.clone();
        client.set_retry_observer(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        let (calls, op) = scripted_failures(10, tonic::Code::ResourceExhausted);
        client
            .retry_read("get_account_commitment", RpcReadMode::SingleAttempt, op)
            .await
            .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(observed.load(Ordering::SeqCst), 0);
    }

    async fn history_client(
        tip: u32,
        pages: Vec<rpc::SyncTransactionsResponse>,
    ) -> (
        MidenRpcClient,
        Arc<Mutex<Vec<rpc::SyncTransactionsRequest>>>,
    ) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let node = test_node::ScriptedNode::failing(
            0,
            || tonic::Status::unavailable("unused"),
            Arc::new(AtomicU32::new(0)),
        )
        .with_chain_tip(tip, pages, requests.clone());
        let endpoint = test_node::serve(node).await;
        let client = MidenRpcClient::connect_with_runtime(
            endpoint,
            RpcClientSettings::new(Duration::from_secs(2), RetryPolicy::new(1)),
            Arc::new(RecordingRuntime::default()),
        )
        .await
        .expect("the scripted node must accept connections");
        (client, requests)
    }

    fn test_account_id() -> AccountId {
        AccountId::from_hex("0xe33cde61b266e9c1753136d03a5572").expect("valid account id")
    }

    #[tokio::test]
    async fn the_chain_tip_is_the_latest_block_header_number_over_a_real_wire() {
        let (client, _) = history_client(548_942, vec![]).await;
        assert_eq!(
            client
                .get_chain_tip(RpcReadMode::SingleAttempt)
                .await
                .expect("tip"),
            548_942
        );
    }

    #[tokio::test]
    async fn sync_transactions_sends_one_account_and_the_inclusive_range_over_a_real_wire() {
        let page = rpc::SyncTransactionsResponse {
            pagination_info: Some(rpc::PaginationInfo {
                chain_tip: 100,
                block_num: 60,
            }),
            transactions: vec![rpc::TransactionRecord {
                block_num: 42,
                header: Some(transaction::TransactionHeader {
                    transaction_id: None,
                    account_id: None,
                    initial_state_commitment: Some(primitives::Word {
                        encoded: vec![0; 32],
                    }),
                    final_state_commitment: Some(primitives::Word {
                        encoded: vec![7; 32],
                    }),
                    input_notes: vec![],
                    output_notes: vec![],
                }),
                output_note_proofs: vec![],
                consumed_note_refs: vec![],
            }],
        };
        let (client, requests) = history_client(100, vec![page.clone()]).await;
        let account_id = test_account_id();

        let answered = client
            .sync_transactions(&account_id, 0, 100, RpcReadMode::SingleAttempt)
            .await
            .expect("page");
        assert_eq!(answered, page);

        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].block_range,
            Some(rpc::BlockRange {
                block_from: 0,
                block_to: 100
            })
        );
        assert_eq!(requests[0].account_ids, vec![proto_account_id(&account_id)]);
    }

    #[tokio::test]
    async fn a_range_past_the_tip_is_rejected_like_the_node_does() {
        let (client, _) = history_client(100, vec![]).await;
        let error = client
            .sync_transactions(&test_account_id(), 0, 101, RpcReadMode::SingleAttempt)
            .await
            .expect_err("block_to past the tip");
        let RpcClientError::Call { operation, status } = error else {
            panic!("expected a call error, got {error:?}");
        };
        assert_eq!(operation, "sync_transactions");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    fn second_account_id() -> AccountId {
        AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").expect("valid account id")
    }

    #[test]
    fn an_account_id_survives_the_proto_round_trip() {
        for id in [test_account_id(), second_account_id()] {
            assert_eq!(account_id_from_proto(&proto_account_id(&id)), Some(id));
        }
        // No version, a missing felt, a non-canonical felt, an invalid pair.
        assert_eq!(
            account_id_from_proto(&account::AccountId { version: None }),
            None
        );
        let v1 = |suffix: Option<u64>, prefix: Option<u64>| account::AccountId {
            version: Some(account::account_id::Version::V1(account::AccountIdV1 {
                suffix: suffix.map(|value| primitives::Felt { value }),
                prefix: prefix.map(|value| primitives::Felt { value }),
            })),
        };
        let id = test_account_id();
        let suffix = id.suffix().as_canonical_u64();
        let prefix = id.prefix().as_felt().as_canonical_u64();
        assert_eq!(account_id_from_proto(&v1(Some(suffix), None)), None);
        assert_eq!(
            account_id_from_proto(&v1(Some(u64::MAX), Some(prefix))),
            None
        );
        assert_eq!(account_id_from_proto(&v1(Some(suffix), Some(0))), None);
    }

    #[test]
    fn a_block_reduces_to_its_account_updates_private_ones_included() {
        let block = test_node::block(
            574_041,
            vec![
                test_node::account_update(test_account_id(), [7; 32], true),
                test_node::account_update(second_account_id(), [9; 32], false),
            ],
        );
        let decoded = block_account_updates(&block).expect("a well-formed block decodes");
        assert_eq!(decoded.block_num, 574_041);
        assert_eq!(
            decoded.updates,
            vec![
                BlockAccountState {
                    account_id: test_account_id(),
                    commitment: format!("0x{}", "07".repeat(32)),
                    is_public: true,
                },
                BlockAccountState {
                    account_id: second_account_id(),
                    commitment: format!("0x{}", "09".repeat(32)),
                    is_public: false,
                },
            ]
        );
        // An empty block has a body with no updates.
        let empty = block_account_updates(&test_node::block(7, vec![])).expect("empty block");
        assert!(empty.updates.is_empty());
        // An update without details is not public (nothing to patch from).
        let mut bare = test_node::block(
            8,
            vec![test_node::account_update(test_account_id(), [1; 32], true)],
        );
        bare.body.as_mut().unwrap().updated_accounts[0].details = None;
        assert!(!block_account_updates(&bare).unwrap().updates[0].is_public);
    }

    #[test]
    fn a_block_missing_what_every_block_has_is_an_error_not_a_partial_answer() {
        let good = || {
            test_node::block(
                10,
                vec![
                    test_node::account_update(test_account_id(), [7; 32], true),
                    test_node::account_update(second_account_id(), [9; 32], false),
                ],
            )
        };
        let mut no_header = good();
        no_header.header = None;
        assert_eq!(
            block_account_updates(&no_header),
            Err(BlockDecodeError::MissingHeader)
        );
        let mut no_number = good();
        no_number.header.as_mut().unwrap().block_num = None;
        assert_eq!(
            block_account_updates(&no_number),
            Err(BlockDecodeError::MissingBlockNumber)
        );
        let mut no_body = good();
        no_body.body = None;
        assert_eq!(
            block_account_updates(&no_body),
            Err(BlockDecodeError::MissingBody)
        );
        let mut no_id = good();
        no_id.body.as_mut().unwrap().updated_accounts[1].account_id = None;
        assert_eq!(
            block_account_updates(&no_id),
            Err(BlockDecodeError::InvalidAccountId { index: 1 })
        );
        let mut short_word = good();
        short_word.body.as_mut().unwrap().updated_accounts[0].final_state_commitment =
            Some(primitives::Word {
                encoded: vec![7; 31],
            });
        assert_eq!(
            block_account_updates(&short_word),
            Err(BlockDecodeError::InvalidCommitment { index: 0 })
        );
        let mut empty_word = good();
        empty_word.body.as_mut().unwrap().updated_accounts[1].final_state_commitment =
            Some(primitives::Word::default());
        assert_eq!(
            block_account_updates(&empty_word),
            Err(BlockDecodeError::InvalidCommitment { index: 1 })
        );
    }

    async fn block_node(node: test_node::ScriptedNode) -> String {
        test_node::serve(node).await
    }

    #[tokio::test]
    async fn get_block_serves_a_committed_block_and_none_for_one_not_served() {
        let node = test_node::ScriptedNode::healthy().with_blocks(vec![
            test_node::block(5, vec![]),
            test_node::block(
                6,
                vec![test_node::account_update(test_account_id(), [3; 32], false)],
            ),
        ]);
        let requests = node.block_requests();
        let client = MidenRpcClient::connect(block_node(node).await)
            .await
            .expect("connect");
        let block = client
            .get_block(6, RpcReadMode::SingleAttempt)
            .await
            .expect("get_block")
            .expect("block 6 is committed");
        assert_eq!(block_account_updates(&block).unwrap().updates.len(), 1);
        assert_eq!(
            client
                .get_block(7, RpcReadMode::SingleAttempt)
                .await
                .expect("get_block"),
            None,
            "a block the node does not serve is no block, not an error"
        );
        assert_eq!(requests.lock().unwrap().clone(), vec![6, 7]);
        assert_eq!(
            client
                .get_chain_tip(RpcReadMode::SingleAttempt)
                .await
                .expect("tip"),
            6
        );
    }

    #[tokio::test]
    async fn a_subscription_replays_committed_blocks_then_streams_live_ones() {
        let node = test_node::ScriptedNode::healthy()
            .with_blocks((3..=6).map(|n| test_node::block(n, vec![])).collect())
            .with_live_blocks(
                vec![test_node::block(7, vec![]), test_node::block(8, vec![])],
                Duration::from_millis(50),
            );
        let requests = node.subscription_requests();
        let mut streaming = MidenRpcClient::connect_streaming(block_node(node).await)
            .await
            .expect("connect streaming");
        let mut stream = streaming.subscribe_blocks(5).await.expect("subscribe");
        let mut numbers = Vec::new();
        let mut tips = Vec::new();
        for _ in 0..4 {
            let item = stream
                .message()
                .await
                .expect("stream item")
                .expect("stream open");
            numbers.push(
                block_account_updates(item.block.as_ref().unwrap())
                    .unwrap()
                    .block_num,
            );
            tips.push(item.committed_chain_tip);
        }
        assert_eq!(
            numbers,
            vec![5, 6, 7, 8],
            "replay from block_from, then live"
        );
        assert_eq!(tips, vec![6, 6, 7, 8]);
        assert_eq!(requests.lock().unwrap().clone(), vec![5]);
        // An idle chain keeps the stream open: no item, no end.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), stream.message())
                .await
                .is_err(),
            "the stream stays open on an idle chain"
        );
    }

    #[tokio::test]
    async fn a_closing_status_reaches_the_subscriber_after_the_blocks() {
        let node = test_node::ScriptedNode::healthy()
            .with_blocks(vec![test_node::block(1, vec![])])
            .with_stream_terminal(tonic::Code::ResourceExhausted, "subscriber lagging");
        let mut streaming = MidenRpcClient::connect_streaming(block_node(node).await)
            .await
            .expect("connect streaming");
        let mut stream = streaming.subscribe_blocks(1).await.expect("subscribe");
        assert!(stream.message().await.expect("first item").is_some());
        let status = stream.message().await.expect_err("terminal status");
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(status.message(), "subscriber lagging");
    }

    #[tokio::test]
    async fn a_node_without_the_stream_refuses_the_subscription_as_unimplemented() {
        let node = test_node::ScriptedNode::healthy()
            .with_blocks(vec![test_node::block(1, vec![])])
            .with_block_subscription_disabled();
        let mut streaming = MidenRpcClient::connect_streaming(block_node(node).await)
            .await
            .expect("connect streaming");
        let error = streaming
            .subscribe_blocks(1)
            .await
            .expect_err("subscription refused");
        let RpcClientError::Call { operation, status } = error else {
            panic!("expected a call error, got {error:?}");
        };
        assert_eq!(operation, "block_subscription");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }

    #[tokio::test]
    async fn a_streaming_connect_to_a_closed_port_fails_without_retrying() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = MidenRpcClient::connect_streaming(format!("http://{address}"))
            .await
            .err()
            .expect("nothing listens there");
        assert!(matches!(error, RpcClientError::Connect(_)), "{error:?}");
    }

    #[test]
    fn word_to_hex_requires_exactly_32_bytes() {
        let zero = primitives::Word {
            encoded: vec![0; 32],
        };
        assert_eq!(
            word_to_hex(&zero).as_deref(),
            Some("0x0000000000000000000000000000000000000000000000000000000000000000")
        );
        // proto3's default word is empty, not the zero word.
        assert_eq!(word_to_hex(&primitives::Word::default()), None);
        assert_eq!(
            word_to_hex(&primitives::Word {
                encoded: vec![1; 31]
            }),
            None
        );
    }
}
