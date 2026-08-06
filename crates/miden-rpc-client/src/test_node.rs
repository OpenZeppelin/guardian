//! A scripted in-process Miden node for wire-level retry tests.
//!
//! Unit tests elsewhere inject typed errors at the trait boundary; this
//! module serves a real tonic gRPC server so tests exercise the actual
//! transport, status rendering, and deadline behavior — the layer where
//! classifier drift has historically gone unnoticed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::{blockchain, note, rpc, transaction};

/// Serves `status` and `get_limits` from a shared failure script: each call
/// increments `calls`, burns one scripted failure while any remain, then
/// succeeds. Every other method answers `unimplemented`.
pub struct ScriptedNode {
    failures_before_success: AtomicU32,
    calls: Arc<AtomicU32>,
    error: fn() -> tonic::Status,
    response_delay: Duration,
}

impl ScriptedNode {
    pub fn failing(times: u32, error: fn() -> tonic::Status, calls: Arc<AtomicU32>) -> Self {
        Self {
            failures_before_success: AtomicU32::new(times),
            calls,
            error,
            response_delay: Duration::ZERO,
        }
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
        let remaining = self.failures_before_success.load(Ordering::SeqCst);
        if remaining > 0 {
            self.failures_before_success
                .store(remaining - 1, Ordering::SeqCst);
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
            .add_service(rpc::api_server::ApiServer::new(node))
            .serve_with_incoming(tonic::transport::server::TcpIncoming::from(listener)),
    );
    format!("http://{address}")
}

#[tonic::async_trait]
impl rpc::api_server::Api for ScriptedNode {
    async fn status(
        &self,
        _: tonic::Request<()>,
    ) -> std::result::Result<tonic::Response<rpc::RpcStatus>, tonic::Status> {
        self.scripted_failure().await?;
        Ok(tonic::Response::new(rpc::RpcStatus {
            version: "scripted".to_string(),
            genesis_commitment: None,
            chain_tip: 7,
            block_producer: None,
        }))
    }

    async fn get_limits(
        &self,
        _: tonic::Request<()>,
    ) -> std::result::Result<tonic::Response<rpc::RpcLimits>, tonic::Status> {
        self.scripted_failure().await?;
        Ok(tonic::Response::new(rpc::RpcLimits::default()))
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
        _: tonic::Request<rpc::AccountRequest>,
    ) -> std::result::Result<tonic::Response<rpc::AccountResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_block_by_number(
        &self,
        _: tonic::Request<blockchain::BlockRequest>,
    ) -> std::result::Result<tonic::Response<blockchain::MaybeBlock>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_block_header_by_number(
        &self,
        _: tonic::Request<rpc::BlockHeaderByNumberRequest>,
    ) -> std::result::Result<tonic::Response<rpc::BlockHeaderByNumberResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_notes_by_id(
        &self,
        _: tonic::Request<note::NoteIdList>,
    ) -> std::result::Result<tonic::Response<note::CommittedNoteList>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_note_script_by_root(
        &self,
        _: tonic::Request<note::NoteScriptRoot>,
    ) -> std::result::Result<tonic::Response<rpc::MaybeNoteScript>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn submit_proven_tx(
        &self,
        _: tonic::Request<transaction::ProvenTransaction>,
    ) -> std::result::Result<tonic::Response<blockchain::BlockNumber>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn submit_proven_tx_batch(
        &self,
        _: tonic::Request<transaction::TransactionBatch>,
    ) -> std::result::Result<tonic::Response<blockchain::BlockNumber>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn sync_transactions(
        &self,
        _: tonic::Request<rpc::SyncTransactionsRequest>,
    ) -> std::result::Result<tonic::Response<rpc::SyncTransactionsResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("scripted node"))
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
        _: tonic::Request<note::NoteId>,
    ) -> std::result::Result<tonic::Response<rpc::GetNetworkNoteStatusResponse>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("scripted node"))
    }
}
