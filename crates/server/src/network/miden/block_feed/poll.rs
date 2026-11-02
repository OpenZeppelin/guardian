//! The polling transport: the fallback when the stream is unavailable. It
//! reads the chain tip, then each block up to it with `GetBlockByNumber`,
//! back to back while behind and one tip read per `poll_interval` once
//! caught up. One attempt per call: the feed is the retry loop.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use miden_rpc_client::{MidenRpcClient, RpcReadMode};

use super::observed_block;
use crate::jobs::block_watch::{BlockSource, BlockSourceError, ObservedBlock, Transport};

pub struct PollSource {
    client: Arc<MidenRpcClient>,
    next: u32,
    poll_interval: Duration,
    /// The tip as last read.
    tip: Option<u32>,
    last_tip_read: Option<Instant>,
}

impl PollSource {
    pub fn new(client: Arc<MidenRpcClient>, from: u32, poll_interval: Duration) -> Self {
        Self {
            client,
            next: from,
            poll_interval,
            tip: None,
            last_tip_read: None,
        }
    }
}

#[async_trait]
impl BlockSource for PollSource {
    async fn next(&mut self) -> Result<ObservedBlock, BlockSourceError> {
        loop {
            if let Some(tip) = self.tip.filter(|tip| *tip >= self.next) {
                let started = std::time::Instant::now();
                let block = self
                    .client
                    .get_block(self.next, RpcReadMode::SingleAttempt)
                    .await;
                crate::network::miden::record_rpc("get_block", started, block.is_ok());
                let Some(block) = block? else {
                    // At or below the tip the node has the block, unless it
                    // no longer retains it.
                    return Err(BlockSourceError::Unavailable {
                        block_num: self.next,
                    });
                };
                let observed = observed_block(&block, tip)?;
                if observed.block_num != self.next {
                    return Err(BlockSourceError::OutOfSequence {
                        expected: self.next,
                        got: observed.block_num,
                    });
                }
                self.next = self.next.saturating_add(1);
                return Ok(observed);
            }
            // Caught up: read the tip again, at most once per interval.
            if let Some(last) = self.last_tip_read {
                tokio::time::sleep_until(last + self.poll_interval).await;
            }
            let started = std::time::Instant::now();
            let tip = self.client.get_chain_tip(RpcReadMode::SingleAttempt).await;
            crate::network::miden::record_rpc("get_chain_tip", started, tip.is_ok());
            self.last_tip_read = Some(Instant::now());
            self.tip = Some(tip?);
        }
    }

    fn transport(&self) -> Transport {
        Transport::Poll
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_rpc_client::test_node::{self, ScriptedNode};

    async fn client(node: ScriptedNode) -> Arc<MidenRpcClient> {
        let endpoint = test_node::serve(node).await;
        Arc::new(MidenRpcClient::connect(endpoint).await.expect("connect"))
    }

    #[tokio::test]
    async fn polling_reads_each_block_to_the_tip_then_waits_for_the_next() {
        let node = ScriptedNode::healthy()
            .with_blocks((1..=4).map(|n| test_node::block(n, vec![])).collect());
        let requests = node.block_requests();
        let mut source = PollSource::new(client(node).await, 2, Duration::from_millis(50));
        assert_eq!(source.transport(), Transport::Poll);
        for expected in 2..=4 {
            let block = source.next().await.expect("block");
            assert_eq!(block.block_num, expected);
            assert_eq!(block.committed_chain_tip, 4);
        }
        assert_eq!(requests.lock().unwrap().clone(), vec![2, 3, 4]);
        // Caught up on an idle chain: it keeps reading the tip, no block.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), source.next())
                .await
                .is_err()
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            3,
            "no block past the tip is asked for"
        );
    }

    #[tokio::test]
    async fn a_block_the_node_does_not_serve_below_its_tip_is_unavailable() {
        // Blocks 1 and 3 only: 2 is gone.
        let node = ScriptedNode::healthy().with_blocks(vec![
            test_node::block(1, vec![]),
            test_node::block(3, vec![]),
        ]);
        let mut source = PollSource::new(client(node).await, 2, Duration::from_millis(10));
        assert_eq!(
            source.next().await.expect_err("missing"),
            BlockSourceError::Unavailable { block_num: 2 }
        );
    }

    #[tokio::test]
    async fn a_failing_node_ends_the_source_with_its_status() {
        let node = ScriptedNode::failing(
            1,
            || tonic::Status::unavailable("node restarting"),
            Arc::new(std::sync::atomic::AtomicU32::new(0)),
        )
        .with_blocks(vec![test_node::block(1, vec![])]);
        let mut source = PollSource::new(client(node).await, 1, Duration::from_millis(10));
        let error = source.next().await.expect_err("tip read fails");
        assert_eq!(error.code(), Some(tonic::Code::Unavailable));
    }
}
