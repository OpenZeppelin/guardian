//! The stream transport: the node's `BlockSubscription`, read by a task of
//! its own so the node's per-subscriber buffer (32 blocks, 10 s send
//! timeout, then the subscriber is cut off for lag and its address refused
//! for 10 minutes) is drained whatever the consumer is doing. The task
//! decodes each block and queues it locally; only when the local queue is
//! full too does the node's buffer start to fill.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use miden_rpc_client::MidenRpcClient;

use super::observed_block;
use crate::jobs::block_watch::{BlockSource, BlockSourceError, ObservedBlock, Transport};

/// Blocks decoded but not yet taken by the consumer: about three minutes
/// of chain at 3 s blocks, so a consumer that briefly stalls (a slow
/// store) never backs the stream up to the node.
pub const LOCAL_QUEUE_BLOCKS: usize = 64;

pub struct StreamSource {
    receiver: mpsc::Receiver<Result<ObservedBlock, BlockSourceError>>,
    task: JoinHandle<()>,
    expected: u32,
}

impl StreamSource {
    /// Subscribe from `from` over a dedicated channel (no request deadline,
    /// HTTP/2 keep-alive).
    pub async fn open(endpoint: &str, from: u32) -> Result<Self, BlockSourceError> {
        let started = std::time::Instant::now();
        let subscription = async {
            let mut client = MidenRpcClient::connect_streaming(endpoint).await?;
            let stream = client.subscribe_blocks(from).await?;
            Ok::<_, miden_rpc_client::RpcClientError>((client, stream))
        }
        .await;
        crate::network::miden::record_rpc("block_subscription", started, subscription.is_ok());
        let (client, stream) = subscription?;
        Ok(Self::spawn(client, stream, from))
    }

    fn spawn(
        client: miden_rpc_client::StreamingClient,
        mut stream: tonic::Streaming<miden_rpc_client::rpc::BlockSubscriptionResponse>,
        from: u32,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(LOCAL_QUEUE_BLOCKS);
        let task = tokio::spawn(async move {
            // The channel lives as long as the stream it carries.
            let _client = client;
            loop {
                let item = match stream.message().await {
                    Ok(Some(item)) => match item.block.as_ref() {
                        Some(block) => observed_block(block, item.committed_chain_tip),
                        None => Err(BlockSourceError::Malformed(
                            "subscription item without a block".to_string(),
                        )),
                    },
                    Ok(None) => Err(BlockSourceError::Ended),
                    Err(status) => Err(status.into()),
                };
                let last = item.is_err();
                // A send fails only once the consumer dropped the source.
                if sender.send(item).await.is_err() || last {
                    return;
                }
            }
        });
        Self {
            receiver,
            task,
            expected: from,
        }
    }
}

impl Drop for StreamSource {
    /// Dropping the source cancels the subscription.
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[async_trait]
impl BlockSource for StreamSource {
    async fn next(&mut self) -> Result<ObservedBlock, BlockSourceError> {
        let block = self
            .receiver
            .recv()
            .await
            .unwrap_or(Err(BlockSourceError::Ended))?;
        if block.block_num != self.expected {
            return Err(BlockSourceError::OutOfSequence {
                expected: self.expected,
                got: block.block_num,
            });
        }
        self.expected = self.expected.saturating_add(1);
        Ok(block)
    }

    fn transport(&self) -> Transport {
        Transport::Stream
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use std::time::Duration;

    use super::*;
    use miden_protocol::account::AccountId;
    use miden_rpc_client::test_node::{self, ScriptedNode};

    fn account() -> AccountId {
        AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap()
    }

    #[tokio::test]
    async fn the_stream_replays_then_follows_the_chain() {
        let node = ScriptedNode::healthy()
            .with_blocks(vec![
                test_node::block(10, vec![]),
                test_node::block(
                    11,
                    vec![test_node::account_update(account(), [5; 32], true)],
                ),
            ])
            .with_live_blocks(
                vec![test_node::block(12, vec![])],
                Duration::from_millis(20),
            );
        let requests = node.subscription_requests();
        let endpoint = test_node::serve(node).await;
        let mut source = StreamSource::open(&endpoint, 11).await.expect("subscribe");
        assert_eq!(source.transport(), Transport::Stream);

        let first = source.next().await.expect("block 11");
        assert_eq!(first.block_num, 11);
        assert_eq!(first.updates.len(), 1);
        assert_eq!(first.updates[0].account_id, account().to_hex());
        assert_eq!(first.committed_chain_tip, 11);
        let live = source.next().await.expect("block 12");
        assert_eq!((live.block_num, live.committed_chain_tip), (12, 12));
        assert_eq!(requests.lock().unwrap().clone(), vec![11]);
        // Nothing more on an idle chain, and waiting is cancel safe.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), source.next())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_closing_status_ends_the_source_with_that_status() {
        let node = ScriptedNode::healthy()
            .with_blocks(vec![test_node::block(1, vec![])])
            .with_stream_terminal(tonic::Code::ResourceExhausted, "lagging");
        let endpoint = test_node::serve(node).await;
        let mut source = StreamSource::open(&endpoint, 1).await.expect("subscribe");
        source.next().await.expect("block 1");
        let error = source.next().await.expect_err("terminal");
        assert_eq!(error.code(), Some(tonic::Code::ResourceExhausted));
        assert_eq!(error.reason(), "status_resource_exhausted");
    }

    #[tokio::test]
    async fn a_refused_subscription_is_a_status_error() {
        let node = ScriptedNode::healthy()
            .with_blocks(vec![test_node::block(1, vec![])])
            .with_block_subscription_disabled();
        let endpoint = test_node::serve(node).await;
        let error = StreamSource::open(&endpoint, 1)
            .await
            .err()
            .expect("refused");
        assert_eq!(error.code(), Some(tonic::Code::Unimplemented));
    }

    #[tokio::test]
    async fn a_block_out_of_sequence_ends_the_source() {
        // The node serves 5 and 7 only: after 5 the next block is 7.
        let node = ScriptedNode::healthy().with_blocks(vec![
            test_node::block(5, vec![]),
            test_node::block(7, vec![]),
        ]);
        let endpoint = test_node::serve(node).await;
        let mut source = StreamSource::open(&endpoint, 5).await.expect("subscribe");
        source.next().await.expect("block 5");
        assert_eq!(
            source.next().await.expect_err("gap"),
            BlockSourceError::OutOfSequence {
                expected: 6,
                got: 7
            }
        );
    }

    #[tokio::test]
    async fn a_malformed_block_ends_the_source() {
        let mut broken = test_node::block(3, vec![]);
        broken.body = None;
        let node = ScriptedNode::healthy().with_blocks(vec![broken]);
        let endpoint = test_node::serve(node).await;
        let mut source = StreamSource::open(&endpoint, 3).await.expect("subscribe");
        assert!(matches!(
            source.next().await,
            Err(BlockSourceError::Malformed(_))
        ));
    }
}
