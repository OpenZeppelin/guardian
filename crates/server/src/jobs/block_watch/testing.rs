//! Scripted block sources for tests: a chain any opened source serves
//! from, with per-open behaviours (refuse, fail after some blocks, stall),
//! and a log of every open. No node, no network.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Notify;

use super::source::{
    AccountUpdate, BlockSource, BlockSourceError, BlockSourceFactory, ObservedBlock, Transport,
};

/// A block numbered `block_num` updating `updates` (`(account, commitment)`).
pub fn block(block_num: u32, updates: &[(&str, &str)]) -> ObservedBlock {
    ObservedBlock {
        block_num,
        committed_chain_tip: block_num,
        updates: updates
            .iter()
            .map(|(account_id, commitment)| AccountUpdate {
                account_id: (*account_id).to_string(),
                commitment: (*commitment).to_string(),
                is_public: true,
            })
            .collect(),
        undecodable_updates: 0,
    }
}

/// Committed blocks, shared by every source the factory opens.
#[derive(Default)]
pub struct ScriptedChain {
    blocks: Mutex<BTreeMap<u32, ObservedBlock>>,
    added: Notify,
    /// Blocks from this one on are not served though the tip counts them,
    /// as behind a load balancer whose replicas disagree: the tip comes
    /// from one ahead, the blocks from one lagging.
    lagging_from: Mutex<Option<u32>>,
}

impl ScriptedChain {
    /// Commit `block`; open sources waiting at the tip receive it.
    pub fn push(&self, block: ObservedBlock) {
        self.blocks.lock().unwrap().insert(block.block_num, block);
        self.added.notify_waiters();
    }

    /// Commit empty blocks `from..=to`.
    pub fn push_empty(&self, from: u32, to: u32) {
        for number in from..=to {
            self.push(block(number, &[]));
        }
    }

    /// Forget the blocks below `block_num`, as a node with bounded
    /// retention would.
    pub fn prune_below(&self, block_num: u32) {
        self.blocks
            .lock()
            .unwrap()
            .retain(|number, _| *number >= block_num);
    }

    pub fn tip(&self) -> Option<u32> {
        self.blocks.lock().unwrap().keys().next_back().copied()
    }

    /// Serve no block from `block_num` on (`None`: serve them all again),
    /// the tip still counting them.
    pub fn lag_from(&self, block_num: Option<u32>) {
        *self.lagging_from.lock().unwrap() = block_num;
        self.added.notify_waiters();
    }

    fn lagging(&self, block_num: u32) -> bool {
        self.lagging_from
            .lock()
            .unwrap()
            .is_some_and(|from| block_num >= from)
    }

    fn get(&self, block_num: u32) -> Option<ObservedBlock> {
        if self.lagging(block_num) {
            return None;
        }
        self.blocks.lock().unwrap().get(&block_num).cloned()
    }

    fn first(&self) -> Option<u32> {
        self.blocks.lock().unwrap().keys().next().copied()
    }
}

/// How the source of one open behaves.
#[derive(Clone, Debug)]
pub enum Open {
    /// The open itself fails.
    Refuse(BlockSourceError),
    /// Serve up to `blocks` blocks (all of them when `None`), following the
    /// chain as it grows, then end as `then` says.
    Serve { blocks: Option<usize>, then: End },
    /// The open never returns, as against a node that takes the call and
    /// never answers it.
    Hang,
}

#[derive(Clone, Debug)]
pub enum End {
    /// The source fails with this error.
    Fail(BlockSourceError),
    /// The source never yields again, though the chain may move on.
    Stall,
}

impl Open {
    /// Follow the chain indefinitely.
    pub fn follow() -> Self {
        Self::Serve {
            blocks: None,
            then: End::Stall,
        }
    }

    /// Serve `blocks` blocks, then fail with `error`.
    pub fn then_fail(blocks: usize, error: BlockSourceError) -> Self {
        Self::Serve {
            blocks: Some(blocks),
            then: End::Fail(error),
        }
    }

    /// Serve `blocks` blocks, then go silent.
    pub fn then_stall(blocks: usize) -> Self {
        Self::Serve {
            blocks: Some(blocks),
            then: End::Stall,
        }
    }
}

/// A gRPC status error, as a node would send it.
pub fn status(code: tonic::Code) -> BlockSourceError {
    BlockSourceError::Status {
        code,
        message: "scripted".to_string(),
    }
}

struct ScriptedSource {
    chain: Arc<ScriptedChain>,
    transport: Transport,
    next: u32,
    remaining: Option<usize>,
    then: End,
}

#[async_trait]
impl BlockSource for ScriptedSource {
    async fn next(&mut self) -> Result<ObservedBlock, BlockSourceError> {
        loop {
            if self.remaining == Some(0) {
                match &self.then {
                    End::Fail(error) => return Err(error.clone()),
                    End::Stall => std::future::pending::<()>().await,
                }
            }
            let added = self.chain.added.notified();
            if let Some(block) = self.chain.get(self.next) {
                self.next += 1;
                self.remaining = self.remaining.map(|left| left - 1);
                return Ok(block);
            }
            // Below the oldest retained block: the node no longer serves it.
            // Its stream ends with INTERNAL (it cannot load the block);
            // a poll sees no block. A poll past a lagging replica's last
            // block, below the tip, sees none either.
            let lagging = self.transport == Transport::Poll
                && self.chain.lagging(self.next)
                && self.chain.tip().is_some_and(|tip| self.next <= tip);
            if lagging || self.chain.first().is_some_and(|first| self.next < first) {
                return Err(match self.transport {
                    Transport::Stream => status(tonic::Code::Internal),
                    Transport::Poll => BlockSourceError::Unavailable {
                        block_num: self.next,
                    },
                });
            }

            added.await;
        }
    }

    fn transport(&self) -> Transport {
        self.transport
    }
}

/// Opens [`ScriptedSource`]s over one [`ScriptedChain`].
#[derive(Default)]
pub struct ScriptedBlockSourceFactory {
    pub chain: Arc<ScriptedChain>,
    streams: Mutex<VecDeque<Open>>,
    polls: Mutex<VecDeque<Open>>,
    /// Answers `chain_tip` instead of the chain's tip while set.
    tip_override: Mutex<Option<Result<u32, BlockSourceError>>>,
    /// The genesis commitment `genesis` reports.
    genesis: Mutex<Option<String>>,
    /// Answers `genesis` gives first, in order, before `genesis`.
    genesis_reads: Mutex<VecDeque<Option<String>>>,
    opens: Mutex<Vec<(Transport, u32, tokio::time::Instant)>>,
}

impl ScriptedBlockSourceFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the next stream opens, in order; once they are used up a
    /// stream follows the chain.
    pub fn with_streams(self, opens: Vec<Open>) -> Self {
        self.streams.lock().unwrap().extend(opens);
        self
    }

    /// Script the next poll opens, in order; once they are used up a poll
    /// source follows the chain.
    pub fn with_polls(self, opens: Vec<Open>) -> Self {
        self.polls.lock().unwrap().extend(opens);
        self
    }

    pub fn set_tip(&self, tip: Result<u32, BlockSourceError>) {
        *self.tip_override.lock().unwrap() = Some(tip);
    }

    /// Report `genesis` as the chain's genesis commitment.
    pub fn set_genesis(&self, genesis: Option<&str>) {
        *self.genesis.lock().unwrap() = genesis.map(str::to_string);
    }

    /// Answer the next `genesis` reads with `reads`, in order.
    pub fn with_genesis_reads(self, reads: &[Option<&str>]) -> Self {
        self.genesis_reads
            .lock()
            .unwrap()
            .extend(reads.iter().map(|read| read.map(str::to_string)));
        self
    }

    /// Replace the chain with a new one (a reset): blocks `1..=tip`, and
    /// `genesis` as its genesis commitment.
    pub fn reset_chain(&self, tip: u32, genesis: Option<&str>) {
        self.chain.blocks.lock().unwrap().clear();
        self.chain.push_empty(1, tip);
        self.set_genesis(genesis);
    }

    /// Every open, `(transport, from)`, in order.
    pub fn opens(&self) -> Vec<(Transport, u32)> {
        self.opens
            .lock()
            .unwrap()
            .iter()
            .map(|(transport, from, _)| (*transport, *from))
            .collect()
    }

    /// When each open happened, in order.
    pub fn open_times(&self) -> Vec<tokio::time::Instant> {
        self.opens
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, at)| *at)
            .collect()
    }

    async fn open_scripted(
        &self,
        transport: Transport,
        from: u32,
    ) -> Result<Box<dyn BlockSource>, BlockSourceError> {
        self.opens
            .lock()
            .unwrap()
            .push((transport, from, tokio::time::Instant::now()));
        let script = match transport {
            Transport::Stream => self.streams.lock().unwrap().pop_front(),
            Transport::Poll => self.polls.lock().unwrap().pop_front(),
        }
        .unwrap_or_else(Open::follow);
        // Like the node, a subscription starting more than 100 blocks past
        // the tip is refused.
        if transport == Transport::Stream
            && self
                .chain
                .tip()
                .is_some_and(|tip| from > tip.saturating_add(100))
        {
            return Err(status(tonic::Code::OutOfRange));
        }
        match script {
            Open::Hang => std::future::pending().await,
            Open::Refuse(error) => Err(error),
            Open::Serve { blocks, then } => Ok(Box::new(ScriptedSource {
                chain: self.chain.clone(),
                transport,
                next: from,
                remaining: blocks,
                then,
            })),
        }
    }
}

#[async_trait]
impl BlockSourceFactory for ScriptedBlockSourceFactory {
    async fn open_stream(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError> {
        self.open_scripted(Transport::Stream, from).await
    }

    async fn open_poll(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError> {
        self.open_scripted(Transport::Poll, from).await
    }

    async fn chain_tip(&self) -> Result<u32, BlockSourceError> {
        if let Some(tip) = self.tip_override.lock().unwrap().clone() {
            return tip;
        }
        self.chain
            .tip()
            .ok_or_else(|| BlockSourceError::Transport("empty scripted chain".to_string()))
    }

    async fn genesis(&self) -> Result<Option<String>, BlockSourceError> {
        if let Some(read) = self.genesis_reads.lock().unwrap().pop_front() {
            return Ok(read);
        }
        Ok(self.genesis.lock().unwrap().clone())
    }

    async fn serves_block(&self, block_num: u32) -> Result<bool, BlockSourceError> {
        Ok(self.chain.get(block_num).is_some())
    }
}
