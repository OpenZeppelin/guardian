//! A scripted in-process Miden node for wire-level retry tests.
//!
//! Unit tests elsewhere inject typed errors at the trait boundary; this
//! module serves a real tonic gRPC server so tests exercise the actual
//! transport, status rendering, and deadline behavior — the layer where
//! classifier drift has historically gone unnoticed.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use miden_protocol::account::AccountId;
use tonic::codegen::tokio_stream::Stream;

use crate::{account, blockchain, primitives, rpc};

/// Serves `status` and `get_limits` from a shared failure script: each call
/// increments `calls`, burns one scripted failure while any remain, then
/// succeeds. With [`Self::with_chain_tip`] it also serves the latest block
/// header and scripted `SyncTransactions` pages under the same script.
/// With [`Self::with_blocks`] it serves a scripted chain of committed
/// blocks through `GetBlockByNumber`, `BlockSubscription` and the latest
/// block header. Every other method answers `unimplemented`.
pub struct ScriptedNode {
    failures_before_success: AtomicU32,
    calls: Arc<AtomicU32>,
    error: fn() -> tonic::Status,
    response_delay: Duration,
    chain_tip: Option<u32>,
    transaction_pages: Mutex<VecDeque<rpc::SyncTransactionsResponse>>,
    transaction_requests: Arc<Mutex<Vec<rpc::SyncTransactionsRequest>>>,
    chain: Arc<ScriptedChain>,
}

/// The committed blocks of a scripted node, shared with the streams it
/// serves so a live block both reaches subscribers and becomes readable.
#[derive(Default)]
struct ScriptedChain {
    /// Committed blocks by number.
    blocks: Mutex<BTreeMap<u32, blockchain::SignedBlock>>,
    /// Blocks each subscription commits after it opened, one per
    /// `live_interval`, then the chain idles.
    live_blocks: Mutex<Vec<blockchain::SignedBlock>>,
    live_interval: Mutex<Duration>,
    /// Status every subscription ends with once its blocks are sent;
    /// without one the stream stays open.
    stream_terminal: Mutex<Option<(tonic::Code, String)>>,
    subscription_disabled: Mutex<bool>,
    /// Subscriptions are accepted and never answered.
    subscription_unanswered: Mutex<bool>,
    /// `block_from` of every subscription request, in order.
    subscription_requests: Arc<Mutex<Vec<u32>>>,
    /// `block_num` of every `GetBlockByNumber` request, in order.
    block_requests: Arc<Mutex<Vec<u32>>>,
}

impl ScriptedChain {
    fn tip(&self) -> Option<u32> {
        self.blocks
            .lock()
            .expect("chain lock")
            .last_key_value()
            .map(|(number, _)| *number)
    }
}

/// A committed block numbered `block_num` whose body updates `updates`.
pub fn block(
    block_num: u32,
    updates: Vec<blockchain::BlockAccountUpdate>,
) -> blockchain::SignedBlock {
    blockchain::SignedBlock {
        header: Some(blockchain::BlockHeader {
            block_num: Some(blockchain::BlockNumber { block_num }),
            ..Default::default()
        }),
        body: Some(blockchain::BlockBody {
            updated_accounts: updates,
            ..Default::default()
        }),
        signatures: Vec::new(),
    }
}

/// One `updated_accounts` entry: `account_id` ended the block at
/// `commitment`, with its public patch when `public`.
pub fn account_update(
    account_id: AccountId,
    commitment: [u8; 32],
    public: bool,
) -> blockchain::BlockAccountUpdate {
    blockchain::BlockAccountUpdate {
        account_id: Some(crate::proto_account_id(&account_id)),
        final_state_commitment: Some(primitives::Word {
            encoded: commitment.to_vec(),
        }),
        details: Some(account::AccountUpdateDetails {
            update: Some(if public {
                account::account_update_details::Update::Public(account::AccountPatch::default())
            } else {
                account::account_update_details::Update::Private(account::PrivateAccountUpdate {})
            }),
        }),
    }
}

/// A stream over a channel the scripted subscription task feeds.
struct ChannelStream(
    tokio::sync::mpsc::Receiver<Result<rpc::BlockSubscriptionResponse, tonic::Status>>,
);

impl Stream for ChannelStream {
    type Item = Result<rpc::BlockSubscriptionResponse, tonic::Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
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
            chain: Arc::new(ScriptedChain::default()),
        }
    }

    /// A node with no scripted failures.
    pub fn healthy() -> Self {
        Self::failing(
            0,
            || tonic::Status::internal("unused"),
            Arc::new(AtomicU32::new(0)),
        )
    }

    /// Serves `blocks` as the committed chain: `GetBlockByNumber` answers
    /// them by number (an absent block as no block), the latest block
    /// header is the highest of them (unless [`Self::with_chain_tip`] set
    /// one), and `BlockSubscription { block_from }` replays those from
    /// `block_from` on, in order.
    pub fn with_blocks(self, blocks: Vec<blockchain::SignedBlock>) -> Self {
        let mut chain = self.chain.blocks.lock().expect("chain lock");
        for block in blocks {
            let number = block
                .header
                .as_ref()
                .and_then(|header| header.block_num)
                .map(|number| number.block_num)
                .expect("a scripted block has a number");
            chain.insert(number, block);
        }
        drop(chain);
        self
    }

    /// After a subscription replayed the committed blocks, it commits
    /// `blocks` one every `interval` (each becomes readable and the tip
    /// advances as it does) and streams each one.
    pub fn with_live_blocks(
        self,
        blocks: Vec<blockchain::SignedBlock>,
        interval: Duration,
    ) -> Self {
        *self.chain.live_blocks.lock().expect("chain lock") = blocks;
        *self.chain.live_interval.lock().expect("chain lock") = interval;
        self
    }

    /// Every subscription ends with `code` once its blocks are sent.
    pub fn with_stream_terminal(self, code: tonic::Code, message: &str) -> Self {
        *self.chain.stream_terminal.lock().expect("chain lock") = Some((code, message.to_string()));
        self
    }

    /// `BlockSubscription` answers `UNIMPLEMENTED`, as a node that does not
    /// serve the stream would.
    pub fn with_block_subscription_disabled(self) -> Self {
        *self.chain.subscription_disabled.lock().expect("chain lock") = true;
        self
    }

    /// `BlockSubscription` is accepted and never answered (no response
    /// headers), as a stuck node or proxy would; the connection itself
    /// stays healthy.
    pub fn with_unanswered_block_subscription(self) -> Self {
        *self
            .chain
            .subscription_unanswered
            .lock()
            .expect("chain lock") = true;
        self
    }

    /// The `block_from` of every subscription request, in order.
    pub fn subscription_requests(&self) -> Arc<Mutex<Vec<u32>>> {
        self.chain.subscription_requests.clone()
    }

    /// The `block_num` of every `GetBlockByNumber` request, in order.
    pub fn block_requests(&self) -> Arc<Mutex<Vec<u32>>> {
        self.chain.block_requests.clone()
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
        request: tonic::Request<rpc::BlockSubscriptionRequest>,
    ) -> std::result::Result<tonic::Response<Self::BlockSubscriptionStream>, tonic::Status> {
        let block_from = request.into_inner().block_from;
        self.chain
            .subscription_requests
            .lock()
            .expect("request log lock")
            .push(block_from);
        if *self.chain.subscription_disabled.lock().expect("chain lock") {
            return Err(tonic::Status::unimplemented("scripted node"));
        }
        if *self
            .chain
            .subscription_unanswered
            .lock()
            .expect("chain lock")
        {
            std::future::pending::<()>().await;
        }
        let chain = self.chain.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let replay: Vec<blockchain::SignedBlock> = chain
                .blocks
                .lock()
                .expect("chain lock")
                .range(block_from..)
                .map(|(_, block)| block.clone())
                .collect();
            for block in replay {
                let tip = chain.tip().unwrap_or(0);
                let item = rpc::BlockSubscriptionResponse {
                    block: Some(block),
                    committed_chain_tip: tip,
                    protocol_config: None,
                };
                if sender.send(Ok(item)).await.is_err() {
                    return;
                }
            }
            let live = chain.live_blocks.lock().expect("chain lock").clone();
            let interval = *chain.live_interval.lock().expect("chain lock");
            for block in live {
                tokio::time::sleep(interval).await;
                let number = block
                    .header
                    .as_ref()
                    .and_then(|header| header.block_num)
                    .map(|number| number.block_num)
                    .expect("a scripted block has a number");
                chain
                    .blocks
                    .lock()
                    .expect("chain lock")
                    .insert(number, block.clone());
                let item = rpc::BlockSubscriptionResponse {
                    block: Some(block),
                    committed_chain_tip: number,
                    protocol_config: None,
                };
                if sender.send(Ok(item)).await.is_err() {
                    return;
                }
            }
            let terminal = chain.stream_terminal.lock().expect("chain lock").clone();
            match terminal {
                Some((code, message)) => {
                    let _ = sender.send(Err(tonic::Status::new(code, message))).await;
                }
                // An open stream on an idle chain: hold the channel until
                // the subscriber goes away.
                None => sender.closed().await,
            }
        });
        Ok(tonic::Response::new(Box::pin(ChannelStream(receiver))))
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

    async fn is_invitation_code_valid(
        &self,
        _: tonic::Request<rpc::IsInvitationCodeValidRequest>,
    ) -> std::result::Result<tonic::Response<rpc::IsInvitationCodeValidResponse>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("scripted node"))
    }

    async fn get_block_by_number(
        &self,
        request: tonic::Request<rpc::GetBlockByNumberRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetBlockByNumberResponse>, tonic::Status> {
        let block_num = request.into_inner().block_num;
        self.chain
            .block_requests
            .lock()
            .expect("request log lock")
            .push(block_num);
        self.scripted_failure().await?;
        let block = self
            .chain
            .blocks
            .lock()
            .expect("chain lock")
            .get(&block_num)
            .cloned();
        Ok(tonic::Response::new(rpc::GetBlockByNumberResponse {
            block,
            proof: None,
        }))
    }

    async fn get_block_header_by_number(
        &self,
        _: tonic::Request<rpc::GetBlockHeaderByNumberRequest>,
    ) -> std::result::Result<tonic::Response<rpc::GetBlockHeaderByNumberResponse>, tonic::Status>
    {
        let Some(tip) = self.chain_tip.or_else(|| self.chain.tip()) else {
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
