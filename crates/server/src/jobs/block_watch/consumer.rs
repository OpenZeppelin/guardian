//! The block watch consumer: reads committed blocks in sequence from the
//! cursor on, and for every guarded account a block moved to a state this
//! server does not hold, records an observation for the release detectors.
//! No chain RPC is made per account here: one membership query and one
//! stored-commitment read per block, then the cursor. The detectors run
//! later, paced, in the release sweep's visit loop.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, sleep_until, timeout_at};
use tokio_util::sync::CancellationToken;

use super::auto::{FailureKind, TransportPolicy, backoff};
use super::source::{BlockSource, BlockSourceError, BlockSourceFactory, ObservedBlock, Transport};
use super::{BlockWatchHandle, BlockWatchSignals};
use crate::coordination::{CursorWrite, NewObservation};
use crate::metrics::labels::BlockWatchUpdateOutcome;
use crate::metrics::names;
use crate::state::AppState;
use crate::storage::LeaseFence;

/// Why the consumer returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The lease holder is shutting the consumer down.
    Cancelled,
    /// A cursor write was refused: this replica no longer holds the
    /// release sweep lease.
    LostLease,
}

/// What processing one block did, for the logs and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockSummary {
    pub updates: usize,
    /// Updates of accounts this server guards (sweepable or with a
    /// candidate in flight).
    pub guarded: usize,
    pub observed: usize,
}

#[derive(Debug)]
enum ProcessError {
    LostLease,
    Store(String),
}

/// The node refuses a subscription that starts more than this many blocks
/// past its tip; a cursor that far ahead belongs to another chain (a reset)
/// or to a node far behind.
pub const MAX_CURSOR_LEAD_BLOCKS: u32 = 100;

/// What a stall timeout found when it read the tip.
enum Stall {
    /// The chain moved past the cursor: the source is stuck.
    Stalled,
    /// No new block: an idle chain.
    Idle,
    /// The tip is far below the cursor: the chain was reset, or the node is
    /// far behind; the start position is evaluated again.
    ChainBehind,
}

pub struct Consumer {
    state: AppState,
    handle: BlockWatchHandle,
    signals: Arc<BlockWatchSignals>,
    fence: LeaseFence,
    /// Whether this consumer should request a rotation even with a cursor
    /// to resume from: the store's observations did not survive the
    /// process (filesystem backend) and this is its first start.
    request_rotation_on_start: bool,
    /// The genesis commitment of the chain the cursor counts, as the node
    /// reported it at start.
    genesis: std::sync::Mutex<Option<String>>,
}

impl Consumer {
    pub fn new(
        state: AppState,
        handle: BlockWatchHandle,
        signals: Arc<BlockWatchSignals>,
        fence: LeaseFence,
        request_rotation_on_start: bool,
    ) -> Self {
        Self {
            state,
            handle,
            signals,
            fence,
            request_rotation_on_start,
            genesis: std::sync::Mutex::new(None),
        }
    }

    fn genesis(&self) -> Option<String> {
        self.genesis
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn factory(&self) -> &Arc<dyn BlockSourceFactory> {
        &self.handle.factory
    }

    /// Consume blocks until cancelled or the lease is lost.
    pub async fn run(self, cancel: CancellationToken) -> Stopped {
        let stopped = self.consume(&cancel).await;
        self.handle.status.clear();
        set_transport_gauge(None);
        // Only the holder's feed has a lag; a replica that stepped down
        // must not keep reporting its last one.
        metrics::gauge!(names::BLOCK_WATCH_LAG_BLOCKS).set(0.0);
        stopped
    }

    async fn consume(&self, cancel: &CancellationToken) -> Stopped {
        let config = &self.handle.config;
        let mut policy = TransportPolicy::new(config);
        let Some(mut next) = self.start(cancel).await else {
            return Stopped::Cancelled;
        };
        if let Ok(pending) = self.handle.store.count_pending_observations().await {
            metrics::gauge!(names::BLOCK_WATCH_OBSERVATIONS_PENDING).set(pending as f64);
        }
        let mut source: Option<Box<dyn BlockSource>> = None;
        let mut open_at = Instant::now();
        let mut previous_transport: Option<Transport> = None;
        let mut first_open = true;

        loop {
            if source.is_none() {
                if !sleep_or_cancel(open_at, cancel).await {
                    return Stopped::Cancelled;
                }
                // A reopen follows a failure, which may be the node coming
                // back on a reset chain.
                if !std::mem::take(&mut first_open) && self.chain_was_reset().await {
                    match self.start(cancel).await {
                        Some(from) => next = from,
                        None => return Stopped::Cancelled,
                    }
                }
                let transport = policy.transport(Instant::now());
                match self.factory().open(transport, next).await {
                    Ok(opened) => {
                        self.attached(transport, next, previous_transport);
                        previous_transport = Some(transport);
                        source = Some(opened);
                    }
                    Err(BlockSourceError::Unavailable { block_num }) => {
                        next = self.jump_past_unavailable(block_num, cancel).await;
                        continue;
                    }
                    // The cursor is too far past the node's tip.
                    Err(error) if error.code() == Some(tonic::Code::OutOfRange) => {
                        metrics::counter!(
                            names::BLOCK_WATCH_RECONNECTS_TOTAL,
                            names::LABEL_REASON => error.reason()
                        )
                        .increment(1);
                        tracing::warn!(
                            event = "block_watch_reconnect",
                            reason = error.reason(),
                            from_block = next,
                            error = %error,
                            "The node refused to stream from the cursor; re-evaluating the \
                             start position"
                        );
                        match self.start(cancel).await {
                            Some(from) => next = from,
                            None => return Stopped::Cancelled,
                        }
                        open_at = Instant::now() + jittered(backoff(1));
                        continue;
                    }
                    Err(error) => {
                        if self.next_block_is_gone(&error, next).await {
                            next = self.jump_past_unavailable(next, cancel).await;
                            continue;
                        }
                        open_at = self.failed(&mut policy, transport, &error, next);
                        continue;
                    }
                }
            }
            let Some(current) = source.as_mut() else {
                continue;
            };
            let transport = current.transport();

            // Polling in auto mode: hand back to the stream once its retry
            // is due.
            let stream_retry_at = match transport {
                Transport::Poll => policy.stream_retry_at(),
                Transport::Stream => None,
            };
            if stream_retry_at.is_some_and(|at| Instant::now() >= at) {
                tracing::info!(
                    event = "block_watch_transport",
                    from = Transport::Poll.as_str(),
                    to = Transport::Stream.as_str(),
                    reason = "stream_retry_due",
                    "Block watch retrying the stream"
                );
                source = None;
                open_at = Instant::now();
                continue;
            }

            let stall_at = Instant::now() + config.stall();
            let deadline = stream_retry_at.map_or(stall_at, |at| at.min(stall_at));
            let received = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Stopped::Cancelled,
                received = timeout_at(deadline, current.next()) => received,
            };
            match received {
                Ok(Ok(block)) => {
                    policy.on_block(transport);
                    match self.process_until_done(&block, transport, cancel).await {
                        Ok(()) => next = block.block_num.saturating_add(1),
                        Err(stopped) => return stopped,
                    }
                }
                Ok(Err(BlockSourceError::Unavailable { block_num })) => {
                    source = None;
                    next = self.jump_past_unavailable(block_num, cancel).await;
                }
                Ok(Err(error)) => {
                    source = None;
                    if self.next_block_is_gone(&error, next).await {
                        next = self.jump_past_unavailable(next, cancel).await;
                        continue;
                    }
                    open_at = self.failed(&mut policy, transport, &error, next);
                }
                Err(_timed_out) => {
                    if stream_retry_at.is_some_and(|at| Instant::now() >= at) {
                        continue;
                    }
                    let stall = self.stalled(next).await;
                    if let Stall::ChainBehind = stall {
                        source = None;
                        match self.start(cancel).await {
                            Some(from) => next = from,
                            None => return Stopped::Cancelled,
                        }
                        continue;
                    }
                    if let Stall::Stalled = stall {
                        source = None;
                        let stall = BlockSourceError::Transport("stall".to_string());
                        let decision =
                            policy.on_failure(transport, FailureKind::Other, Instant::now());
                        metrics::counter!(
                            names::BLOCK_WATCH_RECONNECTS_TOTAL,
                            names::LABEL_REASON => "stall"
                        )
                        .increment(1);
                        open_at = self.reopen_at(transport, &stall, decision, next, "stall");
                    }
                }
            }
        }
    }

    /// The block to start from (cursor policy, plan D5), retried until the
    /// cursor and the tip can be read. `None` when cancelled first.
    async fn start(&self, cancel: &CancellationToken) -> Option<u32> {
        let mut failures = 0;
        loop {
            match self.start_position().await {
                Ok(from) => return Some(from),
                Err(error) => {
                    failures += 1;
                    let wait = backoff(failures);
                    tracing::warn!(
                        event = "block_watch_start_failed",
                        error = %error,
                        retry_in_ms = wait.as_millis() as u64,
                        "Block watch could not read its cursor or the chain tip; retrying"
                    );
                    if !sleep_or_cancel(Instant::now() + wait, cancel).await {
                        return None;
                    }
                }
            }
        }
    }

    async fn start_position(&self) -> Result<u32, String> {
        let config = &self.handle.config;
        let cursor = self
            .handle
            .store
            .cursor(self.handle.network)
            .await
            .map_err(|e| e.to_string())?;
        let tip = self
            .factory()
            .chain_tip()
            .await
            .map_err(|e| e.to_string())?;
        let genesis = self.factory().genesis().await.map_err(|e| e.to_string())?;
        *self
            .genesis
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = genesis.clone();
        self.handle.status.set_chain_tip(tip);
        if let Some(cursor) = &cursor {
            self.handle.status.set_last_block(cursor.last_block);
        }
        // A cursor counted on another chain (the network was reset), or one
        // the node refuses to stream from, starts over at the tip.
        let reset = cursor.as_ref().and_then(|cursor| {
            if let (Some(recorded), Some(reported)) = (&cursor.genesis, &genesis)
                && recorded != reported
            {
                return Some((cursor.last_block, "genesis_changed"));
            }
            (cursor.last_block > tip.saturating_add(MAX_CURSOR_LEAD_BLOCKS))
                .then_some((cursor.last_block, "cursor_ahead_of_tip"))
        });
        if let Some((last_block, reason)) = reset {
            tracing::warn!(
                event = "block_watch_chain_reset",
                cursor = last_block,
                tip,
                reason,
                "Block watch's cursor does not belong to the node's chain: starting at the \
                 chain tip and requesting one release sweep rotation"
            );
            match self
                .handle
                .store
                .reset_cursor(
                    self.handle.network,
                    tip.saturating_sub(1),
                    genesis.as_deref(),
                    &self.fence,
                )
                .await
                .map_err(|e| e.to_string())?
            {
                CursorWrite::StaleLease => {
                    return Err("the release sweep lease moved; cursor not reset".to_string());
                }
                CursorWrite::Advanced | CursorWrite::NotForward => {}
            }
            self.handle.status.set_last_block(tip.saturating_sub(1));
            self.signals.request_rotation();
            return Ok(tip);
        }
        let cursor = cursor.map(|cursor| cursor.last_block);
        let from = match cursor {
            None => {
                tracing::info!(
                    event = "block_watch_cursor_init",
                    start_block = tip,
                    "Block watch has no cursor yet: starting at the chain tip and \
                     requesting one release sweep rotation"
                );
                self.signals.request_rotation();
                tip
            }
            Some(cursor) if tip.saturating_sub(cursor) > config.max_catchup_blocks => {
                tracing::warn!(
                    event = "block_watch_cursor_jump",
                    cursor,
                    tip,
                    gap = tip - cursor,
                    max_catchup_blocks = config.max_catchup_blocks,
                    "Block watch is too far behind to replay: starting at the chain tip and \
                     requesting one release sweep rotation to cover the skipped blocks"
                );
                self.signals.request_rotation();
                tip
            }
            Some(cursor) => {
                if self.request_rotation_on_start {
                    tracing::info!(
                        event = "block_watch_observations_lost",
                        cursor,
                        "Observations do not survive a restart on this backend; requesting \
                         one release sweep rotation"
                    );
                    self.signals.request_rotation();
                }
                cursor.saturating_add(1)
            }
        };
        Ok(from)
    }

    /// The node no longer serves `block_num`: start again at the tip and
    /// let one rotation cover what is skipped.
    async fn jump_past_unavailable(&self, block_num: u32, cancel: &CancellationToken) -> u32 {
        let mut failures = 0;
        loop {
            match self.factory().chain_tip().await {
                Ok(tip) => {
                    let from = tip.max(block_num.saturating_add(1));
                    tracing::warn!(
                        event = "block_watch_cursor_jump",
                        cursor = block_num.saturating_sub(1),
                        tip,
                        gap = from - block_num,
                        reason = "block_unavailable",
                        "The node no longer serves the next block: starting at the chain tip \
                         and requesting one release sweep rotation to cover the skipped blocks"
                    );
                    metrics::counter!(
                        names::BLOCK_WATCH_RECONNECTS_TOTAL,
                        names::LABEL_REASON => "unavailable_block"
                    )
                    .increment(1);
                    self.signals.request_rotation();
                    return from;
                }
                Err(error) => {
                    failures += 1;
                    let wait = backoff(failures);
                    tracing::warn!(error = %error, "Failed to read the chain tip; retrying");
                    if !sleep_or_cancel(Instant::now() + wait, cancel).await {
                        return block_num;
                    }
                }
            }
        }
    }

    /// No block for `stall_seconds`: a stall when the tip is past the
    /// cursor, an idle chain when it is not, a reset (or a node far behind)
    /// when it is far below the cursor.
    async fn stalled(&self, next: u32) -> Stall {
        match self.factory().chain_tip().await {
            Ok(tip) if next > tip.saturating_add(MAX_CURSOR_LEAD_BLOCKS).saturating_add(1) => {
                tracing::warn!(
                    event = "block_watch_stall",
                    last_block = next.saturating_sub(1),
                    chain_tip = tip,
                    "The chain tip is far below the cursor; re-evaluating the start position"
                );
                Stall::ChainBehind
            }
            Ok(tip) => {
                self.handle.status.set_chain_tip(tip);
                if tip >= next {
                    tracing::warn!(
                        event = "block_watch_stall",
                        last_block = next.saturating_sub(1),
                        chain_tip = tip,
                        idle_seconds = self.handle.config.stall_seconds,
                        "No block within the stall timeout while the chain moved on; \
                         reopening the block source"
                    );
                    metrics::gauge!(names::BLOCK_WATCH_LAG_BLOCKS)
                        .set(f64::from(tip.saturating_sub(next.saturating_sub(1))));
                    Stall::Stalled
                } else {
                    Stall::Idle
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "No block within the stall timeout and the chain tip could not be read; \
                     waiting on the open source"
                );
                Stall::Idle
            }
        }
    }

    /// Whether the node now reports another genesis than the chain the
    /// cursor counts. An unreadable genesis is no evidence of a reset.
    async fn chain_was_reset(&self) -> bool {
        let Some(known) = self.genesis() else {
            return false;
        };
        match self.factory().genesis().await {
            Ok(Some(reported)) if reported != known => {
                tracing::warn!(
                    event = "block_watch_chain_reset",
                    known_genesis = %known,
                    reported_genesis = %reported,
                    "The node reports another genesis than the chain block watch was \
                     following"
                );
                true
            }
            _ => false,
        }
    }

    /// A stream that fails with `INTERNAL` may be a node that cannot load
    /// the next block (it no longer has it): asked directly, a block at or
    /// below the tip the node does not serve cannot be replayed.
    async fn next_block_is_gone(&self, error: &BlockSourceError, next: u32) -> bool {
        if error.code() != Some(tonic::Code::Internal) {
            return false;
        }
        let Ok(tip) = self.factory().chain_tip().await else {
            return false;
        };
        tip >= next && matches!(self.factory().serves_block(next).await, Ok(false))
    }

    fn attached(&self, transport: Transport, from: u32, previous: Option<Transport>) {
        tracing::info!(
            event = "block_watch_attached",
            transport = transport.as_str(),
            from_block = from,
            chain_tip = self.handle.status.snapshot().chain_tip,
            "Block watch attached to the node"
        );
        if let Some(previous) = previous.filter(|previous| *previous != transport) {
            tracing::info!(
                event = "block_watch_transport",
                from = previous.as_str(),
                to = transport.as_str(),
                "Block watch changed transport"
            );
        }
        self.handle.status.set_transport(transport);
        set_transport_gauge(Some(transport));
    }

    /// A source failed to open or ended: count it, decide what to open next
    /// and when.
    fn failed(
        &self,
        policy: &mut TransportPolicy,
        transport: Transport,
        error: &BlockSourceError,
        next: u32,
    ) -> Instant {
        let decision = policy.on_failure(transport, FailureKind::of(error), Instant::now());
        metrics::counter!(
            names::BLOCK_WATCH_RECONNECTS_TOTAL,
            names::LABEL_REASON => error.reason()
        )
        .increment(1);
        self.reopen_at(transport, error, decision, next, error.reason())
    }

    fn reopen_at(
        &self,
        transport: Transport,
        error: &BlockSourceError,
        decision: super::auto::Decision,
        next: u32,
        reason: &'static str,
    ) -> Instant {
        let wait = jittered(decision.wait);
        let code = error.code().map(|code| format!("{code:?}"));
        if decision.transport != transport {
            let retry_in_seconds = policy_retry_in(decision.transport, &self.handle.config, error);
            tracing::warn!(
                event = "block_watch_transport",
                from = transport.as_str(),
                to = decision.transport.as_str(),
                reason,
                code = code.as_deref(),
                retry_in_seconds,
                error = %error,
                "Block watch switching transport"
            );
        } else {
            tracing::warn!(
                event = "block_watch_reconnect",
                transport = transport.as_str(),
                reason,
                code = code.as_deref(),
                backoff_ms = wait.as_millis() as u64,
                from_block = next,
                error = %error,
                "Block watch reopening the block source"
            );
        }
        self.handle.status.detach();
        set_transport_gauge(None);
        Instant::now() + wait
    }

    /// Process `block`, retrying storage failures (the block is not done
    /// until its cursor is written) until it succeeds, the lease is lost,
    /// or the consumer is cancelled. The source keeps buffering meanwhile.
    async fn process_until_done(
        &self,
        block: &ObservedBlock,
        transport: Transport,
        cancel: &CancellationToken,
    ) -> Result<(), Stopped> {
        let mut failures = 0;
        loop {
            match self.process(block, transport).await {
                Ok(_) => return Ok(()),
                Err(ProcessError::LostLease) => {
                    tracing::warn!(
                        event = "block_watch_stale_lease",
                        holder_id = %self.fence.holder_id,
                        fence_token = self.fence.fence_token,
                        block_num = block.block_num,
                        "Block watch cursor write refused: the release sweep lease moved to \
                         another replica; stopping"
                    );
                    return Err(Stopped::LostLease);
                }
                Err(ProcessError::Store(error)) => {
                    failures += 1;
                    let wait = backoff(failures).min(Duration::from_secs(30));
                    tracing::warn!(
                        event = "block_watch_block_failed",
                        block_num = block.block_num,
                        error = %error,
                        retry_in_ms = wait.as_millis() as u64,
                        "Block watch could not process a block; retrying it"
                    );
                    if !sleep_or_cancel(Instant::now() + wait, cancel).await {
                        return Err(Stopped::Cancelled);
                    }
                }
            }
        }
    }

    /// One block (plan D4): sort its updates by membership, compare the
    /// guarded ones with their stored commitments, record the moves this
    /// server cannot explain, then advance the cursor.
    async fn process(
        &self,
        block: &ObservedBlock,
        transport: Transport,
    ) -> Result<BlockSummary, ProcessError> {
        let started = std::time::Instant::now();
        let mut seen = HashSet::with_capacity(block.updates.len());
        let ids: Vec<String> = block
            .updates
            .iter()
            .filter(|update| seen.insert(update.account_id.as_str()))
            .map(|update| update.account_id.clone())
            .collect();
        let membership = if ids.is_empty() {
            crate::metadata::ReleaseSweepMembership::default()
        } else {
            self.state
                .metadata
                .filter_release_sweep_ids(&ids)
                .await
                .map_err(|e| ProcessError::Store(format!("membership query: {e}")))?
        };
        let sweepable: HashSet<&str> = membership.sweepable.iter().map(String::as_str).collect();
        let pending: HashSet<&str> = membership
            .pending_candidate
            .iter()
            .map(String::as_str)
            .collect();
        let stored = if sweepable.is_empty() {
            std::collections::HashMap::new()
        } else {
            let refs: Vec<&str> = membership.sweepable.iter().map(String::as_str).collect();
            self.state
                .storage
                .pull_state_commitments_batch(&refs)
                .await
                .map_err(|e| ProcessError::Store(format!("stored commitments: {e}")))?
        };

        let mut summary = BlockSummary {
            updates: block.updates.len(),
            ..BlockSummary::default()
        };
        let mut outcomes = Vec::with_capacity(block.updates.len());
        // Updates whose commitment is not the stored one, by index into
        // `outcomes`, with the stored commitment they differ from.
        let mut moved: Vec<(usize, &str)> = Vec::new();
        for update in &block.updates {
            let id = update.account_id.as_str();
            let outcome = if pending.contains(id) {
                BlockWatchUpdateOutcome::SkippedPendingCandidate
            } else if !sweepable.contains(id) {
                BlockWatchUpdateOutcome::NotGuarded
            } else {
                match stored.get(id) {
                    Some(commitment) if commitment.eq_ignore_ascii_case(&update.commitment) => {
                        BlockWatchUpdateOutcome::AtStored
                    }
                    Some(commitment) => {
                        moved.push((outcomes.len(), commitment.as_str()));
                        BlockWatchUpdateOutcome::Observed
                    }
                    None => BlockWatchUpdateOutcome::UnfinishedConfiguration,
                }
            };
            if outcome != BlockWatchUpdateOutcome::NotGuarded {
                summary.guarded += 1;
            }
            outcomes.push(outcome);
        }

        // A move to a state this server held before (one of the account's
        // own canonical deltas) is an old transaction of its own: a block
        // replayed after downtime, or an optimistic commit ahead of the
        // chain. Only what is left is unexplained.
        let own_history = if moved.is_empty() {
            std::collections::HashSet::new()
        } else {
            let pairs: Vec<(&str, &str)> = moved
                .iter()
                .map(|(index, _)| {
                    let update = &block.updates[*index];
                    (update.account_id.as_str(), update.commitment.as_str())
                })
                .collect();
            self.state
                .storage
                .filter_canonical_post_states(&pairs)
                .await
                .map_err(|e| ProcessError::Store(format!("own history: {e}")))?
        };
        let mut observations = Vec::new();
        for (index, stored_commitment) in moved {
            let update = &block.updates[index];
            if own_history.contains(&(update.account_id.clone(), update.commitment.clone())) {
                outcomes[index] = BlockWatchUpdateOutcome::OwnHistory;
                continue;
            }
            tracing::info!(
                event = "block_watch_observation_recorded",
                account_id = %update.account_id,
                block_num = block.block_num,
                on_chain = %update.commitment,
                stored = %stored_commitment,
                "A guarded account moved on chain to a state this server does not hold; \
                 queued for the release detectors"
            );
            observations.push(NewObservation {
                account_id: update.account_id.clone(),
                block_num: block.block_num,
                on_chain_commitment: update.commitment.clone(),
            });
        }
        summary.observed = observations.len();

        if !observations.is_empty() {
            self.handle
                .store
                .record_observations(&observations, self.state.clock.now())
                .await
                .map_err(|e| ProcessError::Store(format!("record observations: {e}")))?;
        }
        match self
            .handle
            .store
            .set_cursor(
                self.handle.network,
                block.block_num,
                self.genesis().as_deref(),
                &self.fence,
            )
            .await
            .map_err(|e| ProcessError::Store(format!("cursor write: {e}")))?
        {
            CursorWrite::Advanced => {}
            CursorWrite::StaleLease => return Err(ProcessError::LostLease),
            CursorWrite::NotForward => tracing::warn!(
                event = "block_watch_cursor_not_forward",
                block_num = block.block_num,
                "The stored cursor is already at or past this block; it was not moved back"
            ),
        }

        // Counted only once the block is done, so a retried block is
        // counted once.
        for outcome in outcomes {
            metrics::counter!(
                names::BLOCK_WATCH_ACCOUNT_UPDATES_TOTAL,
                names::LABEL_OUTCOME => outcome.as_str()
            )
            .increment(1);
        }
        metrics::counter!(
            names::BLOCK_WATCH_BLOCKS_TOTAL,
            names::LABEL_TRANSPORT => transport.as_str()
        )
        .increment(1);
        metrics::gauge!(names::BLOCK_WATCH_LAST_BLOCK).set(f64::from(block.block_num));
        let lag = block.committed_chain_tip.saturating_sub(block.block_num);
        metrics::gauge!(names::BLOCK_WATCH_LAG_BLOCKS).set(f64::from(lag));
        self.handle.status.set_last_block(block.block_num);
        self.handle.status.set_chain_tip(block.committed_chain_tip);
        if !observations.is_empty() {
            self.signals.observations_recorded();
            if let Ok(pending) = self.handle.store.count_pending_observations().await {
                metrics::gauge!(names::BLOCK_WATCH_OBSERVATIONS_PENDING).set(pending as f64);
            }
        }
        let elapsed = started.elapsed();
        metrics::histogram!(names::BLOCK_WATCH_BLOCK_PROCESSING_SECONDS)
            .record(elapsed.as_secs_f64());
        tracing::debug!(
            event = "block_watch_block",
            block_num = block.block_num,
            updates = summary.updates,
            guarded = summary.guarded,
            observed = summary.observed,
            lag_blocks = lag,
            duration_ms = elapsed.as_millis() as u64,
            "Block watch processed a block"
        );
        Ok(summary)
    }
}

/// The cooldown a transport switch waits out, for the log.
fn policy_retry_in(
    to: Transport,
    config: &crate::block_watch::BlockWatchConfig,
    error: &BlockSourceError,
) -> Option<u64> {
    match (to, FailureKind::of(error)) {
        (Transport::Poll, FailureKind::Banned) => Some(config.ban_cooldown_seconds),
        (Transport::Poll, _) => Some(config.stream_retry_seconds),
        (Transport::Stream, _) => None,
    }
}

fn set_transport_gauge(active: Option<Transport>) {
    for transport in [Transport::Stream, Transport::Poll] {
        metrics::gauge!(
            names::BLOCK_WATCH_TRANSPORT,
            names::LABEL_TRANSPORT => transport.as_str()
        )
        .set(if active == Some(transport) { 1.0 } else { 0.0 });
    }
}

/// Half the wait plus a random share of the other half, so replicas that
/// lost the same node do not reconnect in lockstep.
fn jittered(wait: Duration) -> Duration {
    if wait.is_zero() {
        return wait;
    }
    let half = wait / 2;
    half + half.mul_f64(rand::random::<f64>())
}

/// Sleep until `at`; `false` when cancelled first.
async fn sleep_or_cancel(at: Instant, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = sleep_until(at) => true,
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    impl Consumer {
        /// Process one block directly, for tests of the per-block path.
        pub async fn process_for_test(
            &self,
            block: &ObservedBlock,
        ) -> Result<BlockSummary, String> {
            self.process(block, Transport::Stream)
                .await
                .map_err(|e| format!("{e:?}"))
        }

        pub async fn start_position_for_test(&self) -> Result<u32, String> {
            self.start_position().await
        }
    }
}
