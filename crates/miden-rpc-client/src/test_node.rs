//! A scripted in-process Miden node for wire-level retry tests.
//!
//! Unit tests elsewhere inject typed errors at the trait boundary; this
//! module serves a real tonic gRPC server so tests exercise the actual
//! transport, status rendering, and deadline behavior — the layer where
//! classifier drift has historically gone unnoticed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{blockchain, rpc};

/// Serves `status` and `get_limits` from a shared failure script: each call
/// increments `calls`, burns one scripted failure while any remain, then
/// succeeds. With [`Self::with_chain_tip`] it also serves the latest block
/// header and scripted `SyncTransactions` pages under the same script.
/// Every other method answers `unimplemented`.
pub struct ScriptedNode {
    failures_before_success: AtomicU32,
    calls: Arc<AtomicU32>,
    error: fn() -> tonic::Status,
    response_delay: Duration,
    chain_tip: Option<u32>,
    transaction_pages: Mutex<VecDeque<rpc::SyncTransactionsResponse>>,
    transaction_requests: Arc<Mutex<Vec<rpc::SyncTransactionsRequest>>>,
}

impl ScriptedNode {
    pub fn failing(times: u32, error: fn() -> tonic::Status, calls: Arc<AtomicU32>) -> Self {
        Self {
            failures_before_success: AtomicU32::new(times),
            calls,
            error,
            response_delay: Duration::ZERO,
            chain_tip: None,
            transaction_pages: Mutex::new(VecDeque::new()),
            transaction_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Serves `tip` as the latest block header, and answers
    /// `SyncTransactions` with `pages` in order (then empty pages), each
    /// request recorded in `requests`. Like the real node, a range that
    /// ends past `tip` is rejected with `invalid_argument`.
    pub fn with_chain_tip(
        mut self,
        tip: u32,
        pages: Vec<rpc::SyncTransactionsResponse>,
        requests: Arc<Mutex<Vec<rpc::SyncTransactionsRequest>>>,
    ) -> Self {
        self.chain_tip = Some(tip);
        self.transaction_pages = Mutex::new(pages.into());
        self.transaction_requests = requests;
        self
    }

    /// Delays every scripted response, so a short client deadline expires
    /// against a real in-flight request.
    pub fn with_response_delay(mut self, delay: Duration) -> Self {
        self.response_delay = delay;
        self
    }

    async fn scripted_failure(&self) -> Result<(), tonic::Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.response_delay.is_zero() {
            tokio::time::sleep(self.response_delay).await;
        }
        let burns_a_failure = self
            .failures_before_success
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if burns_a_failure {
            return Err((self.error)());
        }
        Ok(())
    }
}

/// Binds an ephemeral local port, serves the node on a background task, and
/// returns the endpoint URL. The task lives until the test process exits —
/// fine for tests, which is this module's only audience.
pub async fn serve(node: ScriptedNode) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral local port must bind");
    let address = listener.local_addr().expect("bound socket has an address");
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(rpc::node_service_server::NodeServiceServer::new(node))
            .serve_with_incoming(tonic::transport::server::TcpIncoming::from(listener)),
    );
    format!("http://{address}")
}

#[tonic::async_trait]
impl rpc::node_service_server::NodeService for ScriptedNode {
    async fn status(
        &self,
        _: tonic::Request<rpc::StatusRequest>,
    ) -> std::result::Result<tonic::Response<rpc::StatusResponse>, tonic::Status> {
        self.scripted_failure().await?;
        Ok(tonic::Response::new(rpc::StatusResponse {
            version: "scripted".to_string(),
            genesis_commitment: None,
            chain_tip: 7,
            block_producer: None,
        }))
    }

    async fn get_limits(
        &self,
        _: tonic::Request<rpc::GetLimitsRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetLimitsResponse>, tonic::Status> {
        self.scripted_failure().await?;
        Ok(tonic::Response::new(rpc::GetLimitsResponse::default()))
    }

    type BlockSubscriptionStream = std::pin::Pin<
        Box<
            dyn tonic::codegen::tokio_stream::Stream<
                    Item = std::result::Result<rpc::BlockSubscriptionResponse, tonic::Status>,
                > + Send,
        >,
    >;
    async fn block_subscription(
        &self,
        _: tonic::Request<rpc::BlockSubscriptionRequest>,
    ) -> std::result::Result<tonic::Response<Self::BlockSubscriptionStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    type ProofSubscriptionStream = std::pin::Pin<
        Box<
            dyn tonic::codegen::tokio_stream::Stream<
                    Item = std::result::Result<rpc::ProofSubscriptionResponse, tonic::Status>,
                > + Send,
        >,
    >;
    async fn proof_subscription(
        &self,
        _: tonic::Request<rpc::ProofSubscriptionRequest>,
    ) -> std::result::Result<tonic::Response<Self::ProofSubscriptionStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_account(
        &self,
        _: tonic::Request<rpc::GetAccountRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetAccountResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn register_account(
        &self,
        _: tonic::Request<rpc::RegisterAccountRequest>,
    ) -> std::result::Result<tonic::Response<rpc::RegisterAccountResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn is_account_allowed(
        &self,
        _: tonic::Request<rpc::IsAccountAllowedRequest>,
    ) -> std::result::Result<tonic::Response<rpc::IsAccountAllowedResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_block_by_number(
        &self,
        _: tonic::Request<rpc::GetBlockByNumberRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetBlockByNumberResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_block_header_by_number(
        &self,
        _: tonic::Request<rpc::GetBlockHeaderByNumberRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetBlockHeaderByNumberResponse>, tonic::Status>
    {
        let Some(tip) = self.chain_tip else {
            return Err(tonic::Status::unimplemented("scripted node"));
        };
        self.scripted_failure().await?;
        Ok(tonic::Response::new(rpc::GetBlockHeaderByNumberResponse {
            block_header: Some(blockchain::BlockHeader {
                block_num: Some(blockchain::BlockNumber { block_num: tip }),
                ..Default::default()
            }),
            mmr_path: None,
            chain_length: Some(tip + 1),
            protocol_config: None,
        }))
    }

    async fn get_notes_by_id(
        &self,
        _: tonic::Request<rpc::GetNotesByIdRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetNotesByIdResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_note_script_by_root(
        &self,
        _: tonic::Request<rpc::GetNoteScriptByRootRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetNoteScriptByRootResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_transaction_encryption_key(
        &self,
        _: tonic::Request<rpc::GetTransactionEncryptionKeyRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetTransactionEncryptionKeyResponse>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn submit_proven_tx(
        &self,
        _: tonic::Request<rpc::SubmitProvenTxRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SubmitProvenTxResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn submit_proven_tx_batch(
        &self,
        _: tonic::Request<rpc::SubmitProvenTxBatchRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SubmitProvenTxBatchResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn sync_transactions(
        &self,
        request: tonic::Request<rpc::SyncTransactionsRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncTransactionsResponse>, tonic::Status> {
        let Some(tip) = self.chain_tip else {
            return Err(tonic::Status::unimplemented("scripted node"));
        };
        let request = request.into_inner();
        self.transaction_requests
            .lock()
            .expect("request log lock")
            .push(request.clone());
        self.scripted_failure().await?;
        let block_to = request.block_range.map_or(0, |range| range.block_to);
        if block_to > tip {
            return Err(tonic::Status::invalid_argument(format!(
                "block_to ({block_to}) is greater than chain tip ({tip})"
            )));
        }
        let page = self
            .transaction_pages
            .lock()
            .expect("page script lock")
            .pop_front()
            .unwrap_or_else(|| rpc::SyncTransactionsResponse {
                pagination_info: Some(rpc::PaginationInfo {
                    chain_tip: tip,
                    block_num: block_to,
                }),
                transactions: Vec::new(),
            });
        Ok(tonic::Response::new(page))
    }

    async fn sync_notes(
        &self,
        _: tonic::Request<rpc::SyncNotesRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncNotesResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn sync_nullifiers(
        &self,
        _: tonic::Request<rpc::SyncNullifiersRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncNullifiersResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn sync_account_vault(
        &self,
        _: tonic::Request<rpc::SyncAccountVaultRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncAccountVaultResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn sync_account_storage_maps(
        &self,
        _: tonic::Request<rpc::SyncAccountStorageMapsRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncAccountStorageMapsResponse>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn sync_chain_mmr(
        &self,
        _: tonic::Request<rpc::SyncChainMmrRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncChainMmrResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_network_note_status(
        &self,
        _: tonic::Request<rpc::GetNetworkNoteStatusRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetNetworkNoteStatusResponse>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("scripted node"))
    }
}
