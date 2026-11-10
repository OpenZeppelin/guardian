//! Block-driven switch detection (issue #585).
//!
//! The release sweep (`jobs::release_sweep`, issue #434) finds guardian
//! switches that never reached the push path. Its rotation learns that an
//! account moved by probing every account in turn, so its cost grows with
//! the fleet and its latency is the rotation length. Block watch learns it
//! from the chain instead: the release sweep's lease holder consumes the
//! node's committed-block feed (the `BlockSubscription` stream, or
//! `GetBlockByNumber` polling when the stream is unavailable), and every
//! committed block names each account it updated and the commitment the
//! account ended at, private accounts included.
//!
//! Per block, one membership query finds the accounts this server guards,
//! one batched read compares them with their stored commitments, and only a
//! guarded account that moved to a state this server does not hold becomes
//! an **observation**. The release sweep's paced loop visits observations
//! with the existing detectors, the observed commitment standing in for the
//! head probe. A commitment in a block is never evidence of a switch by
//! itself: a release still needs the proof the detectors require (a
//! candidate's exact post-state, the stored state's own key, or a foreign
//! key confirmed in published storage).
//!
//! State a successor needs lives in the [`BlockWatchStore`]: the cursor
//! (written forward-only under the lease) and the observations. A new
//! holder resumes from the cursor and visits what its predecessor recorded.
//! The rotation stays as the safety net, and block watch asks it for one
//! walk whenever it skips blocks (a first start, a gap too large to replay,
//! a block the node no longer serves, observations lost with the process).
//!
//! [`BlockWatchStore`]: crate::coordination::BlockWatchStore

mod auto;
mod consumer;
mod source;
#[cfg(test)]
pub mod testing;
#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) use consumer::count_walk_request;
pub use consumer::{BlockSummary, Consumer, Stopped};
pub use source::{
    AccountUpdate, BlockSource, BlockSourceError, BlockSourceFactory, ObservedBlock, Transport,
};

use crate::block_watch::BlockWatchConfig;
use crate::coordination::BlockWatchStore;
use crate::network::NetworkType;

/// Everything block watch needs, carried on `AppState` while it is enabled.
#[derive(Clone)]
pub struct BlockWatchHandle {
    pub config: BlockWatchConfig,
    pub network: NetworkType,
    pub store: Arc<dyn BlockWatchStore>,
    pub factory: Arc<dyn BlockSourceFactory>,
    /// The feed as this replica sees it, for `/dashboard/info`.
    pub status: Arc<BlockWatchStatus>,
    /// The store's observations survive a restart (Postgres). Without
    /// that, a restart requests one rotation of the release sweep.
    pub durable_observations: bool,
}

/// What this replica's feed is doing. Only the lease holder has a feed;
/// everywhere else every field is `None`.
#[derive(Default)]
pub struct BlockWatchStatus {
    inner: Mutex<StatusSnapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// The transport of the open block source; `None` while none is open.
    pub transport: Option<Transport>,
    /// The last block processed by this replica's feed.
    pub last_block: Option<u32>,
    /// The node's committed tip as last seen.
    pub chain_tip: Option<u32>,
}

impl StatusSnapshot {
    pub fn lag_blocks(&self) -> Option<u32> {
        match (self.chain_tip, self.last_block) {
            (Some(tip), Some(last)) => Some(tip.saturating_sub(last)),
            _ => None,
        }
    }
}

impl BlockWatchStatus {
    fn update(&self, change: impl FnOnce(&mut StatusSnapshot)) {
        change(&mut self.inner.lock().unwrap_or_else(|p| p.into_inner()));
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        *self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn set_transport(&self, transport: Transport) {
        self.update(|s| s.transport = Some(transport));
    }

    /// No source is open (between attempts).
    pub(crate) fn detach(&self) {
        self.update(|s| s.transport = None);
    }

    /// The feed stopped on this replica (the lease moved elsewhere).
    pub(crate) fn clear(&self) {
        self.update(|s| *s = StatusSnapshot::default());
    }

    pub(crate) fn set_last_block(&self, block: u32) {
        self.update(|s| s.last_block = Some(block));
    }

    /// The latest tip read, not the highest ever seen: after a chain reset
    /// the old chain's tip would otherwise inflate the reported lag.
    pub(crate) fn set_chain_tip(&self, tip: u32) {
        self.update(|s| s.chain_tip = Some(tip));
    }
}

/// How the consumer reaches the release sweep's visit loop.
#[derive(Default)]
pub struct BlockWatchSignals {
    /// Wakes the visit loop: observations were recorded, or a rotation was
    /// requested.
    pub wake: tokio::sync::Notify,
    rotation_requested: AtomicBool,
}

impl BlockWatchSignals {
    /// Ask the release sweep for one walk of the fleet that starts no
    /// earlier than now: block watch skipped blocks the walk has to cover.
    pub fn request_rotation(&self) {
        self.rotation_requested.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Whether a walk was requested since the last call.
    pub fn take_rotation_request(&self) -> bool {
        self.rotation_requested.swap(false, Ordering::SeqCst)
    }

    pub(crate) fn observations_recorded(&self) {
        self.wake.notify_one();
    }
}
