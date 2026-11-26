//! Batched submission of Guardian executions (#239).
//!
//! With `GUARDIAN_EXECUTION_MAX_TXS_PER_BATCH` above one, an attempt past its no-retry boundary
//! hands its proven transaction here instead of sending it alone. Transactions of different
//! accounts that arrive within `GUARDIAN_EXECUTION_BATCH_WINDOW_MS` of the first are proved
//! together as one batch and sent through the node's `SubmitProvenTxBatch`, which admits or
//! refuses the batch as a whole and commits it in one block.
//!
//! A batch is only another way to deliver transactions the worker already committed to: each
//! member is exactly the transaction its boundary commit recorded, sent only after its worker's
//! fenced send claim, its outcome is still read per account from the chain, and nothing is proved
//! or sent that an attempt did not hand over. Reconciliation never sends, so the wait between a
//! claim and the batch send (one window and the build, inside the member's send window and far
//! inside its lease) gives no other owner a transaction to send.
//!
//! Every send a member makes stays inside the time its attempt handed over (`Member::send_by`),
//! and each of the two sends it may make gets at least what a single send needs to cross the
//! boundary ([`SendWindow::MIN_SEND`]):
//!
//! - A transaction joins a batch only with the window plus [`AFTER_WINDOW`] left: at least
//!   [`MIN_BUILD`] for the build, [`MIN_BATCH_SEND`] for the batch send, and [`RESEND_RESERVE`]
//!   for its own send should the node refuse the batch. With less it is sent alone at once.
//! - The build ends while every member still has the batch send and its resend left; a member the
//!   batch cannot hold is sent alone as soon as that is known, not after the others' build.
//! - The batch send keeps back [`RESEND_RESERVE`] of every member's time, so a refusal, however
//!   late, leaves each member that long to be sent alone. A send that has not been answered by
//!   then is cut off and its outcome is unknown, as a single send's would be.
//!
//! What becomes of each member:
//!
//! - A transaction without the time to wait for a batch, a group that cannot become a batch (one
//!   member, no chain view, a batch that does not assemble, prove or finish in its members' time,
//!   the batch prover busy for all of its build time, a planned stop) is sent one transaction at a
//!   time, as without batching.
//! - A batch the node refused definitely was never admitted, so none of its transactions is in
//!   flight. Each is then sent alone and judged on its own answer, whatever the batch was refused
//!   for, so a transaction the node refuses does not cost the other accounts their proposals.
//!   The resend has less time than an immediate send would have had, though never less than
//!   [`RESEND_RESERVE`]: a node slower than that to answer it leaves its outcome unknown.
//! - A batch whose outcome is unknown may have been admitted, so its transactions are left for the
//!   chain to settle, as an unknown single send is.
//!
//! The node reverts an admitted batch as a whole when its earliest transaction expiration passes,
//! or when a transaction it depends on is reverted. A transaction therefore joins a batch only
//! with [`MIN_EXPIRATION_HEADROOM_BLOCKS`] blocks left before it expires, and never when it
//! consumes an unauthenticated note, which ties it to a transaction still in flight. Every account
//! appears at most once in a batch, as Guardian holds at most one execution per account.
//!
//! The batch kernel in the pinned protocol is still a skeleton (`miden_tx_batch::BatchVerifier`):
//! its proof binds nothing, takes milliseconds, and the node verifies each transaction's proof and
//! re-executes each transaction anyway. Batching therefore buys no fee or speed today; it exists so
//! that Guardian already builds batches when the real kernel and batch fees arrive upstream
//! (protocol #1122, #1706), behind the same calls.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use miden_client::rpc::NodeRpcClient;
use miden_client::rpc::encryption::SealedTransactionInputs;
use miden_protocol::MIN_PROOF_SECURITY_LEVEL;
use miden_protocol::account::AccountId;
use miden_protocol::batch::{ProposedBatch, ProvenBatch};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::transaction::{PartialBlockchain, ProvenTransaction};
use miden_tx_batch::{BatchExecutor, LocalBatchProver};
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::attempt::{BatchAnswer, send_batch, send_transaction};
use super::chain::{ChainView, build_chain_view_from};
use crate::metrics::execution::{BatchOutcome, record_batch, record_batch_proving};
use crate::services::execute_proposal::{SendWindow, SubmissionOutcome};

/// Blocks a transaction must have left before its expiration, counted from the tip a batch is
/// built at, to join the batch. The node reverts a whole batch once its earliest expiration
/// passes, so a member close to its own would take the others with it. The mempool admits only
/// transactions expiring more than 2 blocks past its tip (node 0.18 `expiration_slack`), and a user
/// batch is normally committed in the next block; this leaves a few blocks more. It has to stay
/// small: a fee faucet with a transfer policy (devnet's) caps every fee-paying transaction at its
/// reference block plus 20, and proving uses several of those blocks before the batch is built.
pub(super) const MIN_EXPIRATION_HEADROOM_BLOCKS: u32 = 8;

/// The least time a batch's build (the chain view read, assembly and proof) is given once its
/// window ends. On the pinned protocol a build takes well under a second (about 0.3 s on devnet,
/// most of it the chain view read); this leaves room for a slow read.
pub(super) const MIN_BUILD: Duration = Duration::from_secs(5);

/// The least time the batch send waits for the node: the least a single send is given to cross
/// the boundary.
pub(super) const MIN_BATCH_SEND: Duration = SendWindow::MIN_SEND;

/// Kept back from the batch send for each member's own send, should the node refuse the batch:
/// again the least a single send is given to cross the boundary.
pub(super) const RESEND_RESERVE: Duration = SendWindow::MIN_SEND;

/// What a member must have left of its send window once its batch's window ends to be built into
/// the batch.
pub(super) const AFTER_WINDOW: Duration = MIN_BUILD
    .saturating_add(MIN_BATCH_SEND)
    .saturating_add(RESEND_RESERVE);

/// How the batcher groups transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BatchSettings {
    pub max_transactions: usize,
    pub window: Duration,
}

impl From<&crate::config::execution::BatchConfig> for BatchSettings {
    fn from(config: &crate::config::execution::BatchConfig) -> Self {
        Self {
            max_transactions: config.max_transactions as usize,
            window: config.window,
        }
    }
}

/// A transaction an attempt handed over after its boundary commit and send claim.
pub(super) struct Member {
    pub proven: Arc<ProvenTransaction>,
    pub sealed: SealedTransactionInputs,
    /// When the attempt's send window closes; no send of this transaction starts after it.
    pub send_by: Instant,
}

impl Member {
    fn account_id(&self) -> AccountId {
        self.proven.account_id()
    }

    /// What is left of the send window at `now`.
    fn left(&self, now: Instant) -> Duration {
        self.send_by.saturating_duration_since(now)
    }

    /// A transaction consuming an unauthenticated note depends on whichever transaction creates
    /// that note, possibly one still in flight whose revert would revert the whole batch.
    fn may_join_a_batch(&self) -> bool {
        self.proven
            .input_notes()
            .iter()
            .all(|note| note.is_authenticated())
    }

    fn has_headroom_at(&self, tip: BlockNumber) -> bool {
        u64::from(self.proven.expiration_block_num().as_u32())
            >= u64::from(tip.as_u32()) + u64::from(MIN_EXPIRATION_HEADROOM_BLOCKS)
    }

    /// Sends this transaction alone with whatever is left of its send window. A transaction whose
    /// window has closed is not sent: it can no longer be accepted, and the chain settles it as an
    /// expired transaction, as it does an attempt that lost its claim to send.
    async fn send_alone(self, rpc: &dyn NodeRpcClient) -> SubmissionOutcome {
        match SendWindow::deadline_after_boundary(self.left(Instant::now())) {
            Some(deadline) => send_transaction(rpc, &self.proven, self.sealed, deadline).await,
            None => SubmissionOutcome::Unknown {
                reason: "the send window closed while the transaction waited for its batch; it \
                         was not sent"
                    .to_string(),
            },
        }
    }
}

/// A member waiting for its batch, and where its outcome goes.
struct Waiting {
    member: Member,
    reply: oneshot::Sender<SubmissionOutcome>,
}

impl Waiting {
    fn answer(self, outcome: SubmissionOutcome) {
        deliver(self.reply, outcome);
    }
}

/// Hands an attempt its outcome. The attempt stopped waiting only if its worker was dropped, and
/// then nobody reads it.
fn deliver(reply: oneshot::Sender<SubmissionOutcome>, outcome: SubmissionOutcome) {
    let _ = reply.send(outcome);
}

/// The batch currently collecting members. `generation` tells its window timer apart from the
/// timer of a batch that already left.
struct OpenBatch {
    generation: u64,
    genesis: BlockHeader,
    waiting: Vec<Waiting>,
}

impl OpenBatch {
    fn holds(&self, account_id: AccountId) -> bool {
        self.waiting
            .iter()
            .any(|waiting| waiting.member.account_id() == account_id)
    }
}

/// Where a member handed to the batcher ended up.
enum Joined {
    /// In the open batch, which is still collecting.
    Waiting,
    /// It filled the open batch, which is to be sent now.
    Full(Box<OpenBatch>),
    /// It opened a batch, whose window starts now.
    Opened(u64),
    /// The open batch already holds a transaction of its account, so it goes alone.
    SameAccount(Waiting),
}

#[derive(Default)]
struct Collecting {
    open: Option<OpenBatch>,
    next_generation: u64,
}

/// Groups the transactions attempts hand over into batches and sends them.
pub(crate) struct Batcher {
    settings: BatchSettings,
    rpc: Arc<dyn NodeRpcClient>,
    submission_rpc: Arc<dyn NodeRpcClient>,
    shutdown: CancellationToken,
    collecting: Mutex<Collecting>,
    /// One batch proof at a time per process. Proving runs on the blocking pool and the real
    /// kernel will use every core, so a batch that finds the prover busy waits for it, but only
    /// within its build time; one still waiting when that runs out is sent one transaction at a
    /// time. The proof itself holds the permit, so a proof its members stopped waiting for still
    /// counts.
    proving: Arc<tokio::sync::Semaphore>,
    /// Runs on the blocking pool when a batch proof starts, before it proves.
    #[cfg(test)]
    before_proving: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>,
}

/// Why a group of transactions was not sent as a batch.
#[derive(Debug, thiserror::Error)]
enum NotBuilt {
    #[error("the chain view for the batch could not be read: {0}")]
    ChainView(String),
    #[error("another batch was being proved for all of this batch's build time")]
    ProverBusy,
    #[error("the batch does not assemble: {0}")]
    Assembly(String),
    #[error("the batch could not be proved: {0}")]
    Proving(String),
    #[error("the batch was not built within the {0:?} its members could spare")]
    OutOfTime(Duration),
    #[error(
        "the batch was built, but its members no longer have the time for a batch send and their \
         own send after a refusal"
    )]
    NoTimeToSend,
    #[error("the process is stopping")]
    Stopping,
}

/// The least of the send time `waiting` have left at `now`.
fn least_left(waiting: &[Waiting], now: Instant) -> Duration {
    waiting
        .iter()
        .map(|waiting| waiting.member.left(now))
        .min()
        .unwrap_or_default()
}

impl Batcher {
    pub(super) fn new(
        settings: BatchSettings,
        rpc: Arc<dyn NodeRpcClient>,
        submission_rpc: Arc<dyn NodeRpcClient>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            rpc,
            submission_rpc,
            shutdown,
            collecting: Mutex::new(Collecting::default()),
            proving: Arc::new(tokio::sync::Semaphore::new(1)),
            #[cfg(test)]
            before_proving: std::sync::OnceLock::new(),
        })
    }

    /// The same batcher sending through `submission_rpc`.
    pub(super) fn sending_through(&self, submission_rpc: Arc<dyn NodeRpcClient>) -> Arc<Self> {
        Self::new(
            self.settings,
            self.rpc.clone(),
            submission_rpc,
            self.shutdown.clone(),
        )
    }

    /// Sends `member` in the next batch, or alone when it cannot join one, and returns what the
    /// node said about it. `genesis` is the chain's genesis header, which the batch's chain view
    /// is seeded from.
    ///
    /// A member joins only with time to wait out a whole window and still have [`AFTER_WINDOW`]
    /// for the build, the batch send and its own send after a refusal; one handed over with less is
    /// sent at once, as without batching, rather than held back from a send it can still make.
    pub(super) async fn submit(
        self: &Arc<Self>,
        genesis: &BlockHeader,
        member: Member,
    ) -> SubmissionOutcome {
        let can_wait = member.left(Instant::now()) >= self.settings.window + AFTER_WINDOW;
        if !can_wait || !member.may_join_a_batch() || self.shutdown.is_cancelled() {
            return member.send_alone(self.submission_rpc.as_ref()).await;
        }
        let (reply, answer) = oneshot::channel();
        match self.join(genesis, Waiting { member, reply }) {
            Joined::Waiting => {}
            Joined::Full(full) => {
                let batcher = self.clone();
                tokio::spawn(async move { batcher.send(*full).await });
            }
            Joined::Opened(generation) => self.close_after_window(generation),
            Joined::SameAccount(Waiting { member, .. }) => {
                return member.send_alone(self.submission_rpc.as_ref()).await;
            }
        }
        answer.await.unwrap_or_else(|_| SubmissionOutcome::Unknown {
            reason: "the batch task ended without answering; the transaction may have been sent"
                .to_string(),
        })
    }

    /// Adds `waiting` to the batch collecting now, or opens one with it.
    fn join(&self, genesis: &BlockHeader, waiting: Waiting) -> Joined {
        let mut collecting = self
            .collecting
            .lock()
            .expect("the batch collector lock is never poisoned");
        match collecting.open.as_mut() {
            Some(open) if open.holds(waiting.member.account_id()) => Joined::SameAccount(waiting),
            Some(open) => {
                open.waiting.push(waiting);
                if open.waiting.len() >= self.settings.max_transactions {
                    Joined::Full(Box::new(collecting.open.take().expect("the batch is open")))
                } else {
                    Joined::Waiting
                }
            }
            None => {
                let generation = collecting.next_generation;
                collecting.next_generation += 1;
                collecting.open = Some(OpenBatch {
                    generation,
                    genesis: genesis.clone(),
                    waiting: vec![waiting],
                });
                Joined::Opened(generation)
            }
        }
    }

    /// Sends the batch `generation` once its window ends, unless it already left full. When the
    /// process starts stopping first, its transactions are sent alone at once: a planned stop
    /// gives sends a few seconds, which a build could use up before anything is sent.
    fn close_after_window(self: &Arc<Self>, generation: u64) {
        let batcher = self.clone();
        tokio::spawn(async move {
            let stopping = tokio::select! {
                _ = tokio::time::sleep(batcher.settings.window) => false,
                _ = batcher.shutdown.cancelled() => true,
            };
            let open = {
                let mut collecting = batcher
                    .collecting
                    .lock()
                    .expect("the batch collector lock is never poisoned");
                match &collecting.open {
                    Some(open) if open.generation == generation => collecting.open.take(),
                    _ => None,
                }
            };
            match open {
                Some(open) if stopping => {
                    tracing::info!(
                        accounts = ?accounts(&open.waiting),
                        "the process is stopping; sending each transaction collected for a batch \
                         alone"
                    );
                    batcher.send_each_alone(open.waiting).await;
                }
                Some(open) => batcher.send(open).await,
                None => {}
            }
        });
    }

    /// Builds and sends the batch of the members that still have [`AFTER_WINDOW`] left, sending
    /// alone every member it does not hold. A member already short of that is sent alone at once,
    /// while the others' batch is built rather than after it.
    async fn send(&self, open: OpenBatch) {
        let OpenBatch {
            genesis, waiting, ..
        } = open;
        let now = Instant::now();
        let (group, mut late): (Vec<Waiting>, Vec<Waiting>) = waiting
            .into_iter()
            .partition(|waiting| waiting.member.left(now) >= AFTER_WINDOW);
        if group.len() < 2 {
            late.extend(group);
            return self.send_each_alone(late).await;
        }
        tokio::join!(
            self.send_each_alone(late),
            self.build_and_send(&genesis, group, now)
        );
    }

    /// Builds the batch of `group`, whose members all had at least [`AFTER_WINDOW`] left at `now`,
    /// and sends it. The build may take only what leaves every member the batch send and its own
    /// send after a refusal. It first takes the batch prover, then reads the tip, so a member's
    /// expiration headroom is measured just before the proof rather than before a wait for another
    /// batch's proof. A member too close to its expiration there is sent alone as soon as the tip
    /// is read, while the others' batch is proved.
    async fn build_and_send(&self, genesis: &BlockHeader, group: Vec<Waiting>, now: Instant) {
        let spare =
            least_left(&group, now).saturating_sub(MIN_BATCH_SEND.saturating_add(RESEND_RESERVE));
        let build_by = now + spare;
        let started = Instant::now();
        // A burst of executions fills several batches at once; the next one waits for the prover
        // rather than giving up on batching while its members still have build time.
        let permit = match self
            .within(build_by, spare, self.proving.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(closed)) => {
                let reason = NotBuilt::Proving(format!("the batch prover is closed: {closed}"));
                return self.not_built(reason, group).await;
            }
            Err(NotBuilt::OutOfTime(_)) => {
                return self.not_built(NotBuilt::ProverBusy, group).await;
            }
            Err(reason) => return self.not_built(reason, group).await,
        };
        let tracked: BTreeSet<BlockNumber> = group
            .iter()
            .map(|waiting| waiting.member.proven.ref_block_num())
            .collect();
        let view = match self
            .within(
                build_by,
                spare,
                build_chain_view_from(self.rpc.as_ref(), genesis, &tracked),
            )
            .await
        {
            Ok(Ok(view)) => view,
            Ok(Err(error)) => {
                return self
                    .not_built(NotBuilt::ChainView(error.to_string()), group)
                    .await;
            }
            Err(reason) => return self.not_built(reason, group).await,
        };
        let tip = view.reference_block();
        let (eligible, short): (Vec<Waiting>, Vec<Waiting>) = group
            .into_iter()
            .partition(|waiting| waiting.member.has_headroom_at(tip));
        if !short.is_empty() {
            tracing::info!(
                tip = tip.as_u32(),
                headroom_blocks = MIN_EXPIRATION_HEADROOM_BLOCKS,
                accounts = ?accounts(&short),
                "transactions too close to their expiration to share a batch are sent alone"
            );
        }
        if eligible.len() < 2 {
            let mut alone = short;
            alone.extend(eligible);
            return self.send_each_alone(alone).await;
        }
        tokio::join!(
            self.send_each_alone(short),
            self.prove_and_send(&view, eligible, permit, build_by, spare, started)
        );
    }

    /// Assembles and proves the batch of `group` at the chain view's tip by `build_by`, holding
    /// the batch prover's `permit`, and sends it.
    async fn prove_and_send(
        &self,
        view: &ChainView,
        group: Vec<Waiting>,
        permit: tokio::sync::OwnedSemaphorePermit,
        build_by: Instant,
        spare: Duration,
        started: Instant,
    ) {
        let transactions: Vec<Arc<ProvenTransaction>> = group
            .iter()
            .map(|waiting| waiting.member.proven.clone())
            .collect();
        let header = view.reference_header().clone();
        let blockchain = view.blockchain().clone();
        #[cfg(test)]
        let before_proving = self.before_proving.get().cloned();
        let proving = tokio::task::spawn_blocking(move || {
            // A started blocking task runs to the end even when the build runs out of time or the
            // process stops and nobody waits for it any more, so the proof keeps the permit until
            // it ends rather than letting another batch prove beside it.
            let _permit = permit;
            #[cfg(test)]
            if let Some(before_proving) = before_proving {
                before_proving();
            }
            let proving = std::time::Instant::now();
            prove_batch(transactions, header, blockchain).map(|proved| (proved, proving.elapsed()))
        });
        let ((proposed, proven), proved_in) = match self.within(build_by, spare, proving).await {
            Ok(Ok(Ok(proved))) => proved,
            Ok(Ok(Err(reason))) => return self.not_built(reason, group).await,
            Ok(Err(error)) => {
                let reason = NotBuilt::Proving(format!("the proving task failed: {error}"));
                return self.not_built(reason, group).await;
            }
            Err(reason) => return self.not_built(reason, group).await,
        };
        record_batch_proving(proved_in);
        self.send_built(proposed, proven, group, started.elapsed(), proved_in)
            .await;
    }

    /// Runs one step of a build that must end by `build_by`, `spare` after the window ended, and
    /// gives it up when the process starts stopping.
    async fn within<F: Future>(
        &self,
        build_by: Instant,
        spare: Duration,
        step: F,
    ) -> Result<F::Output, NotBuilt> {
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => Err(NotBuilt::Stopping),
            output = tokio::time::timeout_at(build_by, step) => {
                output.map_err(|_| NotBuilt::OutOfTime(spare))
            }
        }
    }

    /// Sends alone each member of a group that did not become a batch, saying why. A planned
    /// stop is not counted: it is no failure, and a stop during collection is not counted either.
    async fn not_built(&self, reason: NotBuilt, group: Vec<Waiting>) {
        if matches!(reason, NotBuilt::Stopping) {
            tracing::info!(
                transactions = group.len(),
                accounts = ?accounts(&group),
                "the process is stopping; sending each transaction of the batch being built alone"
            );
            return self.send_each_alone(group).await;
        }
        let outcome = match reason {
            NotBuilt::ProverBusy => BatchOutcome::ProverBusy,
            _ => BatchOutcome::NotBuilt,
        };
        record_batch(outcome, group.len());
        tracing::warn!(
            transactions = group.len(),
            accounts = ?accounts(&group),
            %reason,
            "Guardian did not send these transactions as a batch; sending each alone"
        );
        self.send_each_alone(group).await;
    }

    /// Sends a built batch and hands each member its outcome. The send keeps back
    /// [`RESEND_RESERVE`] of the time its members have in common, so that a batch the node refuses
    /// can still be sent one transaction at a time, each with at least that long.
    async fn send_built(
        &self,
        proposed: ProposedBatch,
        proven: ProvenBatch,
        members: Vec<Waiting>,
        built_in: Duration,
        proved_in: Duration,
    ) {
        // `build_by` leaves the batch send its time, but a finished proof is still taken when the
        // task resumes after it (a paused VM, a starved runtime). A batch send with less than its
        // time would likely end unanswered, and then no member is sent again, so each is sent
        // alone instead with the time it still has.
        let wait = least_left(&members, Instant::now()).saturating_sub(RESEND_RESERVE);
        if wait < MIN_BATCH_SEND {
            return self.not_built(NotBuilt::NoTimeToSend, members).await;
        }
        let deadline = wait.min(SendWindow::MAX_SEND);
        let sealed: Vec<SealedTransactionInputs> = members
            .iter()
            .map(|waiting| waiting.member.sealed.clone())
            .collect();
        let sending = Instant::now();
        let answer = send_batch(
            self.submission_rpc.as_ref(),
            &proven,
            &proposed,
            sealed,
            deadline,
        )
        .await;
        let send_ms = sending.elapsed().as_millis();
        let transactions: Vec<String> = members
            .iter()
            .map(|waiting| waiting.member.proven.id().to_hex())
            .collect();
        let (recorded, outcome) = match answer {
            BatchAnswer::Accepted => (BatchOutcome::Accepted, SubmissionOutcome::Accepted),
            BatchAnswer::Unknown { reason } => {
                (BatchOutcome::Unknown, SubmissionOutcome::Unknown { reason })
            }
            // Whatever the node refused the batch for (one member's transaction, the batch's size,
            // a stale encryption key, a rate limit), each member's own send decides its outcome,
            // as without batching. A member sealed against a stale key is refused again alone and
            // keeps its proposal.
            BatchAnswer::Refused { reason } => {
                record_batch(BatchOutcome::Refused, members.len());
                tracing::warn!(
                    batch_id = %proven.id(),
                    accounts = ?accounts(&members),
                    ?transactions,
                    %reason,
                    send_ms,
                    "the node refused the batch, so none of its transactions is in flight; \
                     sending each alone"
                );
                return self.send_each_alone(members).await;
            }
        };
        record_batch(recorded, members.len());
        let reason = match &outcome {
            SubmissionOutcome::Unknown { reason }
            | SubmissionOutcome::NotProcessed { reason }
            | SubmissionOutcome::Rejected { reason } => reason.as_str(),
            SubmissionOutcome::Accepted => "",
        };
        tracing::info!(
            batch_id = %proven.id(),
            reference_block = proven.reference_block_num().as_u32(),
            expiration_block = proven.batch_expiration_block_num().as_u32(),
            accounts = ?accounts(&members),
            ?transactions,
            outcome = recorded.as_str(),
            reason,
            build_ms = built_in.as_millis(),
            prove_ms = proved_in.as_millis(),
            send_ms,
            "Guardian execution batch submitted"
        );
        for waiting in members {
            waiting.answer(outcome.clone());
        }
    }

    async fn send_each_alone(&self, waiting: Vec<Waiting>) {
        let rpc = self.submission_rpc.as_ref();
        futures::future::join_all(waiting.into_iter().map(
            |Waiting { member, reply }| async move {
                deliver(reply, member.send_alone(rpc).await);
            },
        ))
        .await;
    }
}

/// Assembles the batch at the chain view's reference block and proves it. CPU-bound.
///
/// `ProposedBatch::new` verifies every transaction's proof again and checks the batch the node
/// will check: reference blocks under the batch's, at most one update per account in order, no
/// note consumed twice, every expiration after the reference block. The precompile proof is
/// skipped, as the node does: the batch kernel cannot verify it yet and the prover discards it,
/// while `ProposedBatch::new` has already bound each transaction's precompile witness to its proof.
fn prove_batch(
    transactions: Vec<Arc<ProvenTransaction>>,
    reference: BlockHeader,
    blockchain: PartialBlockchain,
) -> Result<(ProposedBatch, ProvenBatch), NotBuilt> {
    let proposed = ProposedBatch::new(
        transactions,
        reference,
        blockchain,
        BTreeMap::new(),
        MIN_PROOF_SECURITY_LEVEL,
    )
    .map_err(|error| NotBuilt::Assembly(error.to_string()))?;
    let executed = BatchExecutor::new()
        .execute(proposed.clone())
        .map_err(|error| NotBuilt::Proving(error.to_string()))?;
    let proven = LocalBatchProver::default()
        .skip_precompile_proof_generation(true)
        .prove(executed)
        .map_err(|error| NotBuilt::Proving(error.to_string()))?;
    Ok((proposed, proven))
}

fn accounts(waiting: &[Waiting]) -> Vec<String> {
    waiting
        .iter()
        .map(|waiting| waiting.member.account_id().to_hex())
        .collect()
}

#[cfg(test)]
mod outcome_tests {
    use super::BatchSettings;
    use crate::config::execution::BatchConfig;

    #[test]
    fn settings_follow_the_configuration() {
        let settings = BatchSettings::from(&BatchConfig {
            max_transactions: 4,
            window: std::time::Duration::from_millis(250),
        });
        assert_eq!(settings.max_transactions, 4);
        assert_eq!(settings.window, std::time::Duration::from_millis(250));
    }

    /// An attempt hands over at most the longest a send is given, so a transaction handed over
    /// with all of it must be able to wait out the longest window and keep the build, the batch
    /// send and its own send after a refusal.
    #[test]
    fn the_longest_window_and_its_reserves_fit_in_the_longest_send() {
        use crate::config::execution::MAX_EXECUTION_BATCH_WINDOW_MS;
        use crate::services::execute_proposal::SendWindow;

        let window = std::time::Duration::from_millis(u64::from(MAX_EXECUTION_BATCH_WINDOW_MS));
        assert!(window + super::AFTER_WINDOW <= SendWindow::MAX_SEND);
        assert_eq!(super::MIN_BATCH_SEND, SendWindow::MIN_SEND);
        assert_eq!(super::RESEND_RESERVE, SendWindow::MIN_SEND);
    }
}

/// Real transactions of real accounts on a mock chain, proved locally and sealed against a key the
/// chain's validator attests, sent through the mock node.
#[cfg(all(test, feature = "e2e"))]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use miden_client::rpc::encryption::{
        AttestedTransactionEncryptionKey, TransactionEncryptionKey, ValidatorAttestation,
        attestation_commitment, seal_transaction_inputs,
    };
    use miden_client::rpc::{GrpcError, NodeRpcClient, RpcEndpoint, RpcError};
    use miden_client::testing::mock::MockRpcApi;
    use miden_protocol::Word;
    use miden_protocol::account::{Account, AccountId};
    use miden_protocol::block::{BlockHeader, BlockNumber};
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
    use miden_protocol::note::{Note, NoteType};
    use miden_protocol::transaction::TransactionWitness;
    use miden_protocol::utils::serde::Serializable;
    use miden_testing::utils::create_p2any_note;
    use miden_testing::{Auth, MockChain, MockChainBuilder};
    use miden_tx::LocalTransactionProver;
    use tokio::sync::oneshot;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use miden_standards::code_builder::CodeBuilder;

    use super::{
        AFTER_WINDOW, BatchSettings, Batcher, MIN_BATCH_SEND, MIN_BUILD,
        MIN_EXPIRATION_HEADROOM_BLOCKS, Member, OpenBatch, RESEND_RESERVE, Waiting,
        build_chain_view_from, prove_batch,
    };
    use crate::network::miden::execution::test_node::TestNode;
    use crate::services::execute_proposal::SendWindow;
    use crate::services::execute_proposal::SubmissionOutcome;

    /// The wire value of the one IES scheme the client accepts; the client keeps its enum private.
    const X25519_XCHACHA20_POLY1305: u32 = 1;
    /// Long enough that building the fixture's members (each proved locally) leaves every one
    /// time to wait for a batch.
    const SEND_WINDOW: Duration = Duration::from_secs(120);

    struct Fixture {
        rpc: Arc<MockRpcApi>,
        /// The node the batcher reads and sends through: the same mock, with scripted answer
        /// times.
        node: Arc<TestNode>,
        wallets: Vec<Account>,
        /// A note no block holds, so a transaction consuming it does so unauthenticated.
        loose_note: Note,
        genesis: BlockHeader,
        key: TransactionEncryptionKey,
        /// The chain as the wallets' transactions see it: the state they execute against.
        chain: MockChain,
    }

    async fn fixture(wallets: usize) -> Fixture {
        let validator = SigningKey::new();
        let mut builder = MockChainBuilder::new().validator_signing_keys(vec![validator.clone()]);
        let wallets: Vec<Account> = (0..wallets)
            .map(|_| builder.add_existing_wallet(Auth::IncrNonce).unwrap())
            .collect();
        let loose_note =
            create_p2any_note(wallets[0].id(), NoteType::Public, [], builder.rng_mut());
        let mut chain = builder.build().unwrap();
        chain.prove_until_block(BlockNumber::from(3)).unwrap();
        let rpc = Arc::new(MockRpcApi::new(chain.clone()));
        let (genesis, _) = rpc
            .get_block_header_by_number(Some(BlockNumber::GENESIS), false)
            .await
            .unwrap();
        let (tip, _) = rpc.get_block_header_by_number(None, false).await.unwrap();
        let key = attested_key(&validator, genesis.commitment(), 7)
            .verify(genesis.commitment(), tip.validator_config())
            .unwrap();
        Fixture {
            node: Arc::new(TestNode::new(rpc.clone())),
            rpc,
            wallets,
            loose_note,
            genesis,
            key,
            chain,
        }
    }

    fn attested_key(
        validator: &SigningKey,
        genesis: Word,
        key_id: u8,
    ) -> AttestedTransactionEncryptionKey {
        let public_key = KeyExchangeKey::with_rng(&mut rand::rng())
            .public_key()
            .to_bytes();
        let key_id = vec![key_id];
        let commitment = attestation_commitment(
            X25519_XCHACHA20_POLY1305,
            &key_id,
            genesis,
            &public_key,
            None,
        );
        AttestedTransactionEncryptionKey {
            scheme: X25519_XCHACHA20_POLY1305,
            key_id,
            public_key,
            attestations: vec![ValidatorAttestation {
                validator_key: validator.public_key(),
                signature: validator.sign(commitment),
            }],
            next_key: None,
        }
    }

    impl Fixture {
        /// Wallet `index`'s next transaction, proved and sealed as an attempt hands it over.
        async fn member(&self, index: usize) -> Member {
            self.member_consuming(index, None).await
        }

        async fn member_consuming(&self, index: usize, note: Option<Note>) -> Member {
            self.member_built(index, note, None).await
        }

        /// Wallet `index`'s next transaction, expiring `blocks` after its reference block.
        async fn member_expiring_in(&self, index: usize, blocks: u16) -> Member {
            let script = CodeBuilder::default()
                .compile_tx_script(format!(
                    "
                    use miden::protocol::tx

                    @transaction_script
                    pub proc main
                        push.{blocks}
                        exec.tx::update_expiration_block_delta
                    end
                    "
                ))
                .unwrap();
            self.member_built(index, None, Some(script)).await
        }

        async fn member_built(
            &self,
            index: usize,
            note: Option<Note>,
            script: Option<miden_protocol::transaction::TransactionScript>,
        ) -> Member {
            let key = &self.key;
            let mut transaction = self.chain.build_transaction(self.wallets[index].id());
            if let Some(note) = note {
                transaction = transaction.unauthenticated_input_note(note);
            }
            if let Some(script) = script {
                transaction = transaction.tx_script(script);
            }
            let executed = transaction.build().unwrap().execute().await.unwrap();
            let proven = LocalTransactionProver::default()
                .prove(TransactionWitness::from(executed.clone()))
                .unwrap();
            let sealed =
                seal_transaction_inputs(&mut rand::rng(), key, proven.id(), executed.tx_inputs())
                    .unwrap();
            Member {
                proven: Arc::new(proven),
                sealed,
                send_by: Instant::now() + SEND_WINDOW,
            }
        }

        fn batcher(&self, max_transactions: usize, window: Duration) -> Arc<Batcher> {
            self.batcher_stopping_on(max_transactions, window, CancellationToken::new())
        }

        fn batcher_stopping_on(
            &self,
            max_transactions: usize,
            window: Duration,
            shutdown: CancellationToken,
        ) -> Arc<Batcher> {
            Batcher::new(
                BatchSettings {
                    max_transactions,
                    window,
                },
                self.node.clone(),
                self.node.clone(),
                shutdown,
            )
        }

        /// Commits the next block and returns the accounts it updated, with their new commitments.
        async fn commit_block(&self) -> Vec<(AccountId, Word)> {
            self.rpc.prove_block();
            let (tip, _) = self
                .rpc
                .get_block_header_by_number(None, false)
                .await
                .unwrap();
            let (block, _) = self
                .rpc
                .get_block_by_number(tip.block_num(), false)
                .await
                .unwrap();
            block
                .body()
                .updated_accounts()
                .iter()
                .map(|update| (update.account_id(), update.final_state_commitment()))
                .collect()
        }
    }

    fn final_commitment(member: &Member) -> (AccountId, Word) {
        (
            member.proven.account_id(),
            member.proven.account_update().final_state_commitment(),
        )
    }

    fn refused(kind: GrpcError) -> RpcError {
        RpcError::RequestError {
            endpoint: RpcEndpoint::SubmitProvenBatch,
            error_kind: kind,
            endpoint_error: None,
            source: None,
        }
    }

    /// Holds every batch proof of a batcher at its start, on the blocking pool, until released.
    #[derive(Default)]
    struct ProofHold {
        started: AtomicUsize,
        released: Mutex<bool>,
        release: Condvar,
    }

    impl ProofHold {
        fn hold(&self) {
            self.started.fetch_add(1, Ordering::SeqCst);
            let released = self.released.lock().unwrap();
            // Bounded, so a failed test never leaves the runtime waiting on a held proof.
            let _ = self
                .release
                .wait_timeout_while(released, Duration::from_secs(30), |released| !*released)
                .unwrap();
        }

        fn started(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.release.notify_all();
        }
    }

    /// Releases the held proofs when dropped, so a failed assertion does not leave one held.
    struct HeldProofs(Arc<ProofHold>);

    impl std::ops::Deref for HeldProofs {
        type Target = ProofHold;
        fn deref(&self) -> &ProofHold {
            &self.0
        }
    }

    impl Drop for HeldProofs {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    fn hold_proofs(batcher: &Batcher) -> HeldProofs {
        let hold = Arc::new(ProofHold::default());
        let held = hold.clone();
        assert!(
            batcher
                .before_proving
                .set(Arc::new(move || held.hold()))
                .is_ok()
        );
        HeldProofs(hold)
    }

    /// Opens a batch holding `members` as their window ends, with the receivers of their outcomes.
    fn open_batch(
        genesis: &BlockHeader,
        members: Vec<Member>,
    ) -> (OpenBatch, Vec<oneshot::Receiver<SubmissionOutcome>>) {
        let (waiting, answers) = members
            .into_iter()
            .map(|member| {
                let (reply, answer) = oneshot::channel();
                (Waiting { member, reply }, answer)
            })
            .unzip();
        (
            OpenBatch {
                generation: 0,
                genesis: genesis.clone(),
                waiting,
            },
            answers,
        )
    }

    #[tokio::test]
    async fn transactions_of_two_accounts_within_the_window_land_together_as_one_batch() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let expected: BTreeSet<_> = [final_commitment(&a), final_commitment(&b)].into();
        let batcher = f.batcher(4, Duration::from_millis(300));

        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(first, SubmissionOutcome::Accepted);
        assert_eq!(second, SubmissionOutcome::Accepted);
        let batches = f.rpc.submitted_batch_sealed_inputs();
        assert_eq!(batches.len(), 1, "one batch was sent");
        assert_eq!(
            batches[0].len(),
            2,
            "carrying both transactions' sealed inputs"
        );

        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected, "both accounts moved in the same block");
    }

    #[tokio::test]
    async fn a_full_batch_leaves_without_waiting_for_its_window() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let batcher = f.batcher(2, Duration::from_secs(5));
        let started = std::time::Instant::now();
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "sent at the cap, not at the end of the window: {:?}",
            started.elapsed()
        );
        assert_eq!(f.rpc.submitted_batch_sealed_inputs().len(), 1);
    }

    #[tokio::test]
    async fn a_transaction_alone_in_its_window_is_sent_by_itself() {
        let f = fixture(1).await;
        let a = f.member(0).await;
        let expected = final_commitment(&a);
        let batcher = f.batcher(4, Duration::from_millis(100));
        assert_eq!(
            batcher.submit(&f.genesis, a).await,
            SubmissionOutcome::Accepted
        );
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "no batch of one is built"
        );
        assert_eq!(f.commit_block().await, vec![expected]);
    }

    #[tokio::test]
    async fn a_refused_batch_sends_each_transaction_alone() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let expected: BTreeSet<_> = [final_commitment(&a), final_commitment(&b)].into();
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::InvalidArgument),
        );
        let batcher = f.batcher(2, Duration::from_secs(5));

        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert_eq!(
            f.rpc.submitted_batch_sealed_inputs().len(),
            1,
            "the batch was sent once and refused"
        );
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(
            landed, expected,
            "each transaction then went alone and landed"
        );
    }

    #[tokio::test]
    async fn a_batch_the_node_does_not_implement_falls_back_to_single_sends() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::Unimplemented),
        );
        let batcher = f.batcher(2, Duration::from_secs(5));
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert_eq!(f.commit_block().await.len(), 2);
    }

    #[tokio::test]
    async fn an_unanswered_batch_is_left_for_the_chain_to_settle() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::Unavailable),
        );
        let batcher = f.batcher(2, Duration::from_secs(5));
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert!(
            matches!(first, SubmissionOutcome::Unknown { .. }),
            "{first:?}"
        );
        assert!(
            matches!(second, SubmissionOutcome::Unknown { .. }),
            "{second:?}"
        );
        assert!(
            f.commit_block().await.is_empty(),
            "a batch that may have been admitted is never followed by single sends"
        );
    }

    /// The client tells a stale encryption key apart only by the gRPC code, so a batch refused
    /// with it is refused like any other: each member's own send decides its outcome. A member
    /// whose sealing really is stale is refused again alone and keeps its proposal.
    #[tokio::test]
    async fn a_batch_refused_for_a_stale_key_sends_each_transaction_alone() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        assert_eq!(
            a.sealed.key_id(),
            b.sealed.key_id(),
            "the usual case: one key"
        );
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::FailedPrecondition),
        );
        let batcher = f.batcher(2, Duration::from_secs(5));
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert_eq!(f.rpc.submitted_batch_sealed_inputs().len(), 1);
        assert_eq!(f.node.single_sends().len(), 2, "each was sent alone");
        assert_eq!(f.commit_block().await.len(), 2);
    }

    /// A planned stop gives sends a few seconds, so transactions still collecting are sent alone
    /// at once rather than built into a batch first.
    #[tokio::test]
    async fn a_shutdown_sends_each_collecting_transaction_alone_at_once() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let shutdown = CancellationToken::new();
        let batcher = f.batcher_stopping_on(4, Duration::from_secs(5), shutdown.clone());
        let started = std::time::Instant::now();
        let stop = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            shutdown.cancel();
        };
        let (first, second, ()) = tokio::join!(
            batcher.submit(&f.genesis, a),
            batcher.submit(&f.genesis, b),
            stop
        );
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "no batch was built on the way out"
        );
        assert_eq!(f.node.single_sends().len(), 2);
        assert_eq!(f.commit_block().await.len(), 2);

        let c = f.member(0).await;
        assert_eq!(
            batcher.submit(&f.genesis, c).await,
            SubmissionOutcome::Accepted,
            "after the shutdown a transaction is sent at once"
        );
        assert!(f.rpc.submitted_batch_sealed_inputs().is_empty());
    }

    /// A stop that comes while a batch is being built ends the build: its members are sent alone
    /// at once instead of after a proof or chain read that could outlast the stop's grace.
    #[tokio::test]
    async fn a_shutdown_during_a_build_sends_its_members_alone_at_once() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let expected: BTreeSet<_> = [&a, &b].map(final_commitment).into();
        let shutdown = CancellationToken::new();
        let batcher = f.batcher_stopping_on(4, Duration::from_millis(100), shutdown.clone());
        let held = hold_proofs(&batcher);
        let (open, answers) = open_batch(&f.genesis, vec![a, b]);
        let sending = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(open).await }
        });
        while held.started() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        shutdown.cancel();
        for answer in answers {
            let outcome = tokio::time::timeout(Duration::from_secs(8), answer)
                .await
                .expect("the members waited for the held proof")
                .unwrap();
            assert_eq!(outcome, SubmissionOutcome::Accepted);
        }
        assert!(f.rpc.submitted_batch_sealed_inputs().is_empty());
        held.release();
        sending.await.unwrap();
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// The second transaction of an account is sent at once rather than joining the batch its
    /// account already waits in, where the batch would not assemble.
    #[tokio::test]
    async fn two_transactions_of_one_account_never_share_a_batch() {
        let f = fixture(1).await;
        let (a, again) = (f.member(0).await, f.member(0).await);
        let window = Duration::from_secs(3);
        let batcher = f.batcher(4, window);
        let started = std::time::Instant::now();
        let timed = |outcome: SubmissionOutcome| (outcome, started.elapsed());
        let ((first, first_at), (second, second_at)) = tokio::join!(
            async { timed(batcher.submit(&f.genesis, a).await) },
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                timed(batcher.submit(&f.genesis, again).await)
            }
        );
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert!(second_at < window, "sent alone at once: {second_at:?}");
        assert!(
            first_at >= window,
            "the first waited out its window: {first_at:?}"
        );
        assert!(f.rpc.submitted_batch_sealed_inputs().is_empty());
    }

    #[tokio::test]
    async fn a_transaction_consuming_an_unauthenticated_note_is_sent_alone() {
        let f = fixture(2).await;
        let loose = f.member_consuming(0, Some(f.loose_note.clone())).await;
        assert!(!loose.may_join_a_batch());
        let b = f.member(1).await;
        assert!(b.may_join_a_batch());
        let batcher = f.batcher(2, Duration::from_millis(300));
        let (first, second) = tokio::join!(
            batcher.submit(&f.genesis, loose),
            batcher.submit(&f.genesis, b)
        );
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "the other transaction was left alone in its window"
        );
    }

    #[tokio::test]
    async fn a_transaction_near_its_expiration_keeps_out_of_batches() {
        let f = fixture(1).await;
        let a = f.member(0).await;
        let expires = a.proven.expiration_block_num().as_u32();
        let within = BlockNumber::from(expires - MIN_EXPIRATION_HEADROOM_BLOCKS);
        let too_close = BlockNumber::from(expires - MIN_EXPIRATION_HEADROOM_BLOCKS + 1);
        assert!(a.has_headroom_at(within));
        assert!(!a.has_headroom_at(too_close));
    }

    #[tokio::test]
    async fn a_closed_send_window_is_never_sent_into() {
        let f = fixture(1).await;
        let mut a = f.member(0).await;
        a.send_by = Instant::now();
        let outcome = a.send_alone(f.rpc.as_ref()).await;
        assert!(
            matches!(outcome, SubmissionOutcome::Unknown { .. }),
            "{outcome:?}"
        );
        assert!(f.commit_block().await.is_empty(), "nothing was sent");
    }

    #[tokio::test]
    async fn a_transaction_without_time_to_wait_for_a_batch_is_sent_at_once() {
        let f = fixture(2).await;
        let window = Duration::from_secs(3);
        let mut short = f.member(0).await;
        // A second short of the window, the build, the batch send and its own send after a
        // refusal: it would not have the time it needs, so it does not wait at all.
        short.send_by = Instant::now() + window + AFTER_WINDOW - Duration::from_secs(1);
        let other = f.member(1).await;
        let batcher = f.batcher(4, window);
        let started = std::time::Instant::now();
        let ((first, first_at), (second, second_at)) = tokio::join!(
            async {
                let outcome = batcher.submit(&f.genesis, short).await;
                (outcome, started.elapsed())
            },
            async {
                let outcome = batcher.submit(&f.genesis, other).await;
                (outcome, started.elapsed())
            }
        );
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert!(first_at < window, "sent without waiting: {first_at:?}");
        assert!(second_at >= window, "the other waited alone: {second_at:?}");
        assert!(f.rpc.submitted_batch_sealed_inputs().is_empty());
    }

    /// Whether two members joining with the least wait allowed (the window plus
    /// [`AFTER_WINDOW`]) are sent as one batch when reading the chain view for their batch takes
    /// `read`, on a paused clock.
    async fn batched_after_a_chain_read_of(read: Duration) -> bool {
        let f = fixture(2).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let window = Duration::from_millis(100);
        let send_by = Instant::now() + window + AFTER_WINDOW;
        (a.send_by, b.send_by) = (send_by, send_by);
        f.node.answer_chain_syncs_after(read);
        let batcher = f.batcher(4, window);
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert_eq!(f.commit_block().await.len(), 2);
        let batches = f.rpc.submitted_batch_sealed_inputs().len();
        let alone = f.node.single_sends().len();
        assert!(
            (batches, alone) == (1, 0) || (batches, alone) == (0, 2),
            "{batches} batches, {alone} sent alone"
        );
        batches == 1
    }

    /// Members that join with the least wait allowed still have [`MIN_BUILD`] for their build
    /// once the window ends, and no more: a build a second shorter is sent as a batch, one a
    /// second longer sends each member alone with the time it still has.
    #[tokio::test(start_paused = true)]
    async fn members_joining_with_the_least_wait_have_exactly_their_build_time() {
        assert!(batched_after_a_chain_read_of(MIN_BUILD - Duration::from_secs(1)).await);
        assert!(!batched_after_a_chain_read_of(MIN_BUILD + Duration::from_secs(1)).await);
    }

    /// A refusal that arrives just before the batch send's deadline still leaves every member
    /// [`RESEND_RESERVE`] for its own send. Members carry the longest send time an attempt hands
    /// over ([`SendWindow::MAX_SEND`]).
    #[tokio::test(start_paused = true)]
    async fn a_late_batch_refusal_still_leaves_each_member_its_resend_time() {
        let f = fixture(2).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let window = Duration::from_millis(100);
        let send_by = Instant::now() + SendWindow::MAX_SEND;
        (a.send_by, b.send_by) = (send_by, send_by);
        let expected: BTreeSet<_> = [&a, &b].map(final_commitment).into();
        // The batch send may wait the members' time less their resend: 60 s - 0.1 s - 15 s.
        let batch_deadline = SendWindow::MAX_SEND - window - RESEND_RESERVE;
        f.node
            .answer_batches_after(batch_deadline - Duration::from_secs(1));
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::InvalidArgument),
        );
        let batcher = f.batcher(4, window);
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        let sends = f.node.single_sends();
        assert_eq!(sends.len(), 2, "each was sent alone after the refusal");
        for sent in sends {
            let left = send_by.saturating_duration_since(sent);
            assert!(left >= RESEND_RESERVE, "sent alone with only {left:?} left");
        }
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// The batch send is cut off where its members' resend time begins. An answer that has not
    /// come by then is unknown, like a single send's, and is never followed by single sends.
    #[tokio::test(start_paused = true)]
    async fn the_batch_send_never_waits_into_its_members_resend_time() {
        let f = fixture(2).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let window = Duration::from_millis(100);
        let started = Instant::now();
        let send_by = started + SendWindow::MAX_SEND;
        (a.send_by, b.send_by) = (send_by, send_by);
        // Refused, but only after the resend time began.
        f.node
            .answer_batches_after(SendWindow::MAX_SEND - window - RESEND_RESERVE / 2);
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::InvalidArgument),
        );
        let batcher = f.batcher(4, window);
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert!(
            matches!(first, SubmissionOutcome::Unknown { .. }),
            "{first:?}"
        );
        assert!(
            matches!(second, SubmissionOutcome::Unknown { .. }),
            "{second:?}"
        );
        assert!(f.node.single_sends().is_empty(), "never sent again");
        let answered = Instant::now();
        assert!(
            answered <= send_by - RESEND_RESERVE,
            "the batch send waited {:?} into the resend time",
            answered.saturating_duration_since(send_by - RESEND_RESERVE)
        );
    }

    /// A member too close to its expiration at the tip the batch is built at is sent alone as soon
    /// as the tip is read, not after the others' batch is proved.
    #[tokio::test]
    async fn a_member_near_its_expiration_is_sent_while_the_others_batch_is_proved() {
        let f = fixture(3).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let near = f.member_expiring_in(2, 4).await;
        let (tip, _) = f.rpc.get_block_header_by_number(None, false).await.unwrap();
        assert!(!near.has_headroom_at(tip.block_num()));
        let expected: BTreeSet<_> = [&a, &b, &near].map(final_commitment).into();
        let batcher = f.batcher(4, Duration::from_millis(100));
        let held = hold_proofs(&batcher);
        let (open, answers) = open_batch(&f.genesis, vec![a, b, near]);
        let [first, second, near_answer]: [_; 3] = answers.try_into().unwrap();
        let sending = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(open).await }
        });

        let near_outcome = tokio::time::timeout(Duration::from_secs(8), near_answer)
            .await
            .expect("the member near its expiration waited for the others' batch proof")
            .unwrap();
        assert_eq!(near_outcome, SubmissionOutcome::Accepted);
        let proving = async {
            while held.started() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(8), proving)
            .await
            .expect("the others' batch proof started");
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "answered while the others' proof is held"
        );

        held.release();
        sending.await.unwrap();
        assert_eq!(first.await.unwrap(), SubmissionOutcome::Accepted);
        assert_eq!(second.await.unwrap(), SubmissionOutcome::Accepted);
        let batches = f.rpc.submitted_batch_sealed_inputs();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2, "the other two left as one batch");
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// A member left with less than [`AFTER_WINDOW`] when its window ends (a late timer) is sent
    /// at once, not after the others' batch is built, which may take up to their own spare time and
    /// so outlast what it has left.
    #[tokio::test]
    async fn a_member_short_of_send_time_is_sent_while_the_others_batch_is_built() {
        let f = fixture(3).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let mut late = f.member(2).await;
        late.send_by = Instant::now() + AFTER_WINDOW - Duration::from_secs(1);
        let expected: BTreeSet<_> = [&a, &b, &late].map(final_commitment).into();
        let batcher = f.batcher(4, Duration::from_millis(100));
        let held = hold_proofs(&batcher);
        let (open, answers) = open_batch(&f.genesis, vec![a, b, late]);
        let [first, second, late_answer]: [_; 3] = answers.try_into().unwrap();
        let sending = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(open).await }
        });

        let late_outcome = tokio::time::timeout(Duration::from_secs(8), late_answer)
            .await
            .expect("the late member waited for the others' batch proof")
            .unwrap();
        assert_eq!(late_outcome, SubmissionOutcome::Accepted);
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "answered while the others' batch was still being built"
        );

        held.release();
        sending.await.unwrap();
        assert_eq!(first.await.unwrap(), SubmissionOutcome::Accepted);
        assert_eq!(second.await.unwrap(), SubmissionOutcome::Accepted);
        assert_eq!(held.started(), 1);
        let batches = f.rpc.submitted_batch_sealed_inputs();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2, "the others still left as one batch");
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// A batch proof outlasting its build's time is not cancelled: its members are sent alone, and
    /// the proof keeps the prover until it ends, so the next group waits for it only through its
    /// own build time and is then sent alone, never proved beside it.
    #[tokio::test]
    async fn a_batch_proof_its_members_stopped_waiting_for_keeps_the_prover_until_it_ends() {
        let f = fixture(4).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let (mut c, mut d) = (f.member(2).await, f.member(3).await);
        let expected: BTreeSet<_> = [&a, &b, &c, &d].map(final_commitment).into();
        let window = Duration::from_millis(100);
        let batcher = f.batcher(4, window);
        let held = hold_proofs(&batcher);
        // About the least build time once the window ends (a second more, so the members still
        // join after the time building them takes); the held proof outlasts it.
        let send_by = Instant::now() + window + AFTER_WINDOW + Duration::from_secs(1);
        (a.send_by, b.send_by) = (send_by, send_by);

        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted),
            "sent alone once the build ran out of time"
        );
        assert_eq!(held.started(), 1, "their batch proof started and is held");

        let send_by = Instant::now() + window + AFTER_WINDOW + Duration::from_secs(1);
        (c.send_by, d.send_by) = (send_by, send_by);
        let (third, fourth) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(batcher.submit(&f.genesis, c), batcher.submit(&f.genesis, d))
        })
        .await
        .expect("the next group was proved beside the held proof");
        assert_eq!(
            (third, fourth),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert_eq!(
            held.started(),
            1,
            "no second proof started beside the first"
        );
        assert!(f.rpc.submitted_batch_sealed_inputs().is_empty());
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected, "every transaction went alone and landed");

        held.release();
        let freed = async {
            while batcher.proving.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), freed)
            .await
            .expect("the prover is free once the held proof ends");
    }

    /// A burst fills several batches at once. The next batch waits for the prover within its
    /// build time instead of sending its members alone at once.
    #[tokio::test]
    async fn a_batch_finding_the_prover_busy_waits_for_it_within_its_build_time() {
        let f = fixture(4).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let (c, d) = (f.member(2).await, f.member(3).await);
        let expected: BTreeSet<_> = [&a, &b, &c, &d].map(final_commitment).into();
        let batcher = f.batcher(4, Duration::from_millis(100));
        let held = hold_proofs(&batcher);
        let (first, first_answers) = open_batch(&f.genesis, vec![a, b]);
        let (second, second_answers) = open_batch(&f.genesis, vec![c, d]);
        let one = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(first).await }
        });
        let proving = async {
            while held.started() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(8), proving)
            .await
            .expect("the first batch proof started");
        let two = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(second).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            f.node.single_sends().is_empty(),
            "the second batch waits for the prover rather than sending alone"
        );
        assert_eq!(held.started(), 1, "and does not prove beside the first");

        held.release();
        one.await.unwrap();
        two.await.unwrap();
        for answer in first_answers.into_iter().chain(second_answers) {
            assert_eq!(answer.await.unwrap(), SubmissionOutcome::Accepted);
        }
        assert_eq!(held.started(), 2);
        assert_eq!(
            f.rpc.submitted_batch_sealed_inputs().len(),
            2,
            "two batches"
        );
        assert!(f.node.single_sends().is_empty());
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// A finished proof taken after the task resumed late (a paused VM, a starved runtime) leaves
    /// the batch send less than its time. Each member is then sent alone with the time it still
    /// has, rather than in a batch send likely to end unanswered and never be sent again.
    #[tokio::test]
    async fn a_batch_built_too_late_for_its_send_sends_each_member_alone() {
        let f = fixture(2).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let expected: BTreeSet<_> = [&a, &b].map(final_commitment).into();
        let tracked: BTreeSet<BlockNumber> =
            [a.proven.ref_block_num(), b.proven.ref_block_num()].into();
        let view = build_chain_view_from(f.rpc.as_ref(), &f.genesis, &tracked)
            .await
            .unwrap();
        let (proposed, proven) = prove_batch(
            vec![a.proven.clone(), b.proven.clone()],
            view.reference_header().clone(),
            view.blockchain().clone(),
        )
        .unwrap();
        // A second short of the batch send's time beyond the resend reserve.
        let send_by = Instant::now() + RESEND_RESERVE + MIN_BATCH_SEND - Duration::from_secs(1);
        (a.send_by, b.send_by) = (send_by, send_by);
        let batcher = f.batcher(4, Duration::from_millis(100));
        let (open, answers) = open_batch(&f.genesis, vec![a, b]);
        batcher
            .send_built(
                proposed,
                proven,
                open.waiting,
                Duration::ZERO,
                Duration::ZERO,
            )
            .await;
        for answer in answers {
            assert_eq!(answer.await.unwrap(), SubmissionOutcome::Accepted);
        }
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "no batch was sent"
        );
        assert_eq!(f.node.single_sends().len(), 2, "each was sent alone");
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// A batch waiting for the prover measures its members' expiration headroom once it has the
    /// prover, at the tip it is then built at, not at a tip read before the wait: a member that
    /// lost its headroom while the chain moved on is sent alone.
    #[tokio::test]
    async fn expiration_headroom_is_measured_after_waiting_for_the_prover() {
        let f = fixture(5).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let (c, d) = (f.member(2).await, f.member(3).await);
        let tip = |blocks: u32| BlockNumber::from(3 + blocks);
        // Enough headroom at the tip the members were proved at, not two blocks later.
        let near = f
            .member_expiring_in(4, (MIN_EXPIRATION_HEADROOM_BLOCKS + 1) as u16)
            .await;
        assert!(near.has_headroom_at(tip(0)) && !near.has_headroom_at(tip(2)));
        let expected: BTreeSet<_> = [&a, &b, &c, &d, &near].map(final_commitment).into();
        let batcher = f.batcher(4, Duration::from_millis(100));
        let held = hold_proofs(&batcher);
        let (first, first_answers) = open_batch(&f.genesis, vec![a, b]);
        let (second, second_answers) = open_batch(&f.genesis, vec![c, d, near]);
        let one = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(first).await }
        });
        let proving = async {
            while held.started() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(8), proving)
            .await
            .expect("the first batch proof started");
        let two = tokio::spawn({
            let batcher = batcher.clone();
            async move { batcher.send(second).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        // The chain moves on while the second batch waits for the prover.
        f.rpc.prove_block();
        f.rpc.prove_block();

        held.release();
        one.await.unwrap();
        two.await.unwrap();
        for answer in first_answers.into_iter().chain(second_answers) {
            assert_eq!(answer.await.unwrap(), SubmissionOutcome::Accepted);
        }
        let batches = f.rpc.submitted_batch_sealed_inputs();
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[1].len(),
            2,
            "the member short of headroom left the batch"
        );
        assert_eq!(f.node.single_sends().len(), 1, "and was sent alone");
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }
}
