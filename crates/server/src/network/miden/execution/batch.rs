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
//! - A transaction without the time to wait for a batch, a group that cannot become a batch (one
//!   member, no chain view, a batch that does not assemble, prove or finish in its members' time,
//!   the batch prover busy) is sent one transaction at a time, as without batching.
//! - A batch the node refused definitely was never admitted, so none of its transactions is in
//!   flight. Each is then sent alone and judged on its own, so a transaction the node refuses
//!   does not cost the other accounts their proposals.
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
use super::chain::build_chain_view_from;
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
    /// kernel will use every core, so a batch that finds the prover busy is sent one transaction at
    /// a time instead of queueing behind another while its members' send windows run down. The
    /// proof itself holds the permit, so a proof its members stopped waiting for still counts.
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
    #[error("fewer than two transactions can share a batch")]
    TooFew,
    #[error("another batch is being proved")]
    ProverBusy,
    #[error("the batch does not assemble: {0}")]
    Assembly(String),
    #[error("the batch could not be proved: {0}")]
    Proving(String),
    #[error("the batch was not built within the {0:?} its members could spare")]
    OutOfTime(Duration),
}

/// A batch proved from some of a group's members.
struct Proved {
    proposed: ProposedBatch,
    proven: ProvenBatch,
    /// Index-aligned with the group: whether the member is in the batch.
    included: Vec<bool>,
    proved_in: Duration,
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
    /// A member joins only with time to wait out a whole window and still have the least time a
    /// send needs ([`SendWindow::MIN_SEND`]); one handed over with less is sent at once, as without
    /// batching, rather than held back from a send it can still make.
    pub(super) async fn submit(
        self: &Arc<Self>,
        genesis: &BlockHeader,
        member: Member,
    ) -> SubmissionOutcome {
        let can_wait = member.left(Instant::now()) >= self.settings.window + SendWindow::MIN_SEND;
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

    /// Sends the batch `generation` once its window ends, or at once when the process starts
    /// shutting down, unless it already left full.
    fn close_after_window(self: &Arc<Self>, generation: u64) {
        let batcher = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(batcher.settings.window) => {}
                _ = batcher.shutdown.cancelled() => {}
            }
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
            if let Some(open) = open {
                batcher.send(open).await;
            }
        });
    }

    /// Builds and sends the batch of the members that still have [`SendWindow::MIN_SEND`] left,
    /// sending alone every member it does not hold. A member already short of that is sent alone
    /// at once, while the others' batch is built rather than after it.
    async fn send(&self, open: OpenBatch) {
        let OpenBatch {
            genesis, waiting, ..
        } = open;
        let now = Instant::now();
        let (group, mut late): (Vec<Waiting>, Vec<Waiting>) = waiting
            .into_iter()
            .partition(|waiting| waiting.member.left(now) >= SendWindow::MIN_SEND);
        if group.len() < 2 {
            late.extend(group);
            return self.send_each_alone(late).await;
        }
        tokio::join!(
            self.send_each_alone(late),
            self.build_and_send(&genesis, group, now)
        );
    }

    /// Builds the batch of `group`, whose members all had at least [`SendWindow::MIN_SEND`] left
    /// at `now`, within the time they can spare, and sends it, sending alone every member the
    /// batch does not hold.
    async fn build_and_send(&self, genesis: &BlockHeader, group: Vec<Waiting>, now: Instant) {
        // Whatever the build takes comes out of every member's send window, so it may take only
        // what leaves each member the least time a send needs.
        let budget = group
            .iter()
            .map(|waiting| waiting.member.left(now))
            .min()
            .unwrap_or_default()
            .saturating_sub(SendWindow::MIN_SEND);
        let started = std::time::Instant::now();
        let proved = tokio::time::timeout(budget, self.prove(genesis, &group))
            .await
            .unwrap_or(Err((NotBuilt::OutOfTime(budget), group.len())));
        match proved {
            Ok(proved) => {
                let built_in = started.elapsed();
                let mut members = Vec::with_capacity(group.len());
                let mut alone = Vec::new();
                for (waiting, included) in group.into_iter().zip(proved.included.iter().copied()) {
                    if included {
                        members.push(waiting);
                    } else {
                        alone.push(waiting);
                    }
                }
                tokio::join!(
                    self.send_built(proved, members, built_in),
                    self.send_each_alone(alone)
                );
            }
            Err((reason, eligible)) => {
                if !matches!(reason, NotBuilt::TooFew) {
                    let outcome = match reason {
                        NotBuilt::ProverBusy => BatchOutcome::ProverBusy,
                        _ => BatchOutcome::NotBuilt,
                    };
                    record_batch(outcome, eligible);
                    tracing::warn!(
                        transactions = eligible,
                        accounts = ?accounts(&group),
                        %reason,
                        "Guardian did not send these transactions as a batch; sending each alone"
                    );
                }
                self.send_each_alone(group).await;
            }
        }
    }

    /// Reads the chain view at the tip, keeps the members with expiration headroom there, and
    /// assembles and proves their batch. On failure, returns why and how many members the batch
    /// would have held.
    async fn prove(
        &self,
        genesis: &BlockHeader,
        group: &[Waiting],
    ) -> Result<Proved, (NotBuilt, usize)> {
        let tracked: BTreeSet<BlockNumber> = group
            .iter()
            .map(|waiting| waiting.member.proven.ref_block_num())
            .collect();
        let view = build_chain_view_from(self.rpc.as_ref(), genesis, &tracked)
            .await
            .map_err(|error| (NotBuilt::ChainView(error.to_string()), group.len()))?;
        let tip = view.reference_block();
        let included: Vec<bool> = group
            .iter()
            .map(|waiting| waiting.member.has_headroom_at(tip))
            .collect();
        let eligible = included.iter().filter(|included| **included).count();
        if eligible < group.len() {
            tracing::info!(
                tip = tip.as_u32(),
                headroom_blocks = MIN_EXPIRATION_HEADROOM_BLOCKS,
                excluded = group.len() - eligible,
                "transactions too close to their expiration to share a batch are sent alone"
            );
        }
        if eligible < 2 {
            return Err((NotBuilt::TooFew, eligible));
        }
        let Ok(permit) = self.proving.clone().try_acquire_owned() else {
            return Err((NotBuilt::ProverBusy, eligible));
        };
        let transactions: Vec<Arc<ProvenTransaction>> = group
            .iter()
            .zip(&included)
            .filter(|(_, included)| **included)
            .map(|(waiting, _)| waiting.member.proven.clone())
            .collect();
        let header = view.reference_header().clone();
        let blockchain = view.blockchain().clone();
        #[cfg(test)]
        let before_proving = self.before_proving.get().cloned();
        let proving = std::time::Instant::now();
        let (proposed, proven) = tokio::task::spawn_blocking(move || {
            // A started blocking task runs to the end even when the build runs out of time and
            // nobody waits for it any more, so the proof keeps the permit until it ends rather
            // than letting another batch prove beside it.
            let _permit = permit;
            #[cfg(test)]
            if let Some(before_proving) = before_proving {
                before_proving();
            }
            prove_batch(transactions, header, blockchain)
        })
        .await
        .map_err(|error| NotBuilt::Proving(format!("the proving task failed: {error}")))
        .and_then(|proved| proved)
        .map_err(|reason| (reason, eligible))?;
        let proved_in = proving.elapsed();
        record_batch_proving(proved_in);
        Ok(Proved {
            proposed,
            proven,
            included,
            proved_in,
        })
    }

    /// Sends a built batch with the time its members have in common, and hands each member its
    /// outcome. A batch the node refused is sent again one transaction at a time.
    async fn send_built(&self, proved: Proved, members: Vec<Waiting>, built_in: Duration) {
        let Proved {
            proposed,
            proven,
            proved_in,
            ..
        } = proved;
        let now = Instant::now();
        let common = members
            .iter()
            .map(|waiting| waiting.member.left(now))
            .min()
            .unwrap_or_default();
        let Some(deadline) = SendWindow::deadline_after_boundary(common) else {
            record_batch(BatchOutcome::NotBuilt, members.len());
            tracing::warn!(
                batch_id = %proven.id(),
                accounts = ?accounts(&members),
                "a member's send window closed while the batch was built; sending each alone"
            );
            return self.send_each_alone(members).await;
        };
        let sealed: Vec<SealedTransactionInputs> = members
            .iter()
            .map(|waiting| waiting.member.sealed.clone())
            .collect();
        let one_key = sealed
            .windows(2)
            .all(|pair| pair[0].key_id() == pair[1].key_id());
        let sending = std::time::Instant::now();
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
        let shared = match answer {
            BatchAnswer::Accepted => Some((BatchOutcome::Accepted, SubmissionOutcome::Accepted)),
            BatchAnswer::Unknown { reason } => {
                Some((BatchOutcome::Unknown, SubmissionOutcome::Unknown { reason }))
            }
            // Every member sealed against the key the node called stale, so each alone would be
            // refused the same way. Members sealed against different keys are sent alone: only
            // some of them carry the stale sealing.
            BatchAnswer::StaleKey { reason } if one_key => Some((
                BatchOutcome::NotProcessed,
                SubmissionOutcome::NotProcessed { reason },
            )),
            BatchAnswer::StaleKey { reason } | BatchAnswer::Refused { reason } => {
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
                None
            }
        };
        let Some((recorded, outcome)) = shared else {
            return self.send_each_alone(members).await;
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
    use miden_protocol::utils::serde::Serializable;
    use miden_testing::utils::create_p2any_note;
    use miden_testing::{Auth, MockChain, MockChainBuilder};
    use miden_tx::LocalTransactionProver;
    use tokio::sync::oneshot;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::{
        BatchSettings, Batcher, MIN_EXPIRATION_HEADROOM_BLOCKS, Member, OpenBatch, Waiting,
    };
    use crate::services::execute_proposal::SendWindow;
    use crate::services::execute_proposal::SubmissionOutcome;

    /// The wire value of the one IES scheme the client accepts; the client keeps its enum private.
    const X25519_XCHACHA20_POLY1305: u32 = 1;
    /// Long enough that building the fixture's members (each proved locally) leaves every one
    /// time to wait for a batch.
    const SEND_WINDOW: Duration = Duration::from_secs(120);

    struct Fixture {
        rpc: Arc<MockRpcApi>,
        wallets: Vec<Account>,
        /// A note no block holds, so a transaction consuming it does so unauthenticated.
        loose_note: Note,
        genesis: BlockHeader,
        key: TransactionEncryptionKey,
        /// Another key the same validator attests, as after a key rotation.
        other_key: TransactionEncryptionKey,
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
        let other_key = attested_key(&validator, genesis.commitment(), 8)
            .verify(genesis.commitment(), tip.validator_config())
            .unwrap();
        Fixture {
            rpc,
            wallets,
            loose_note,
            genesis,
            key,
            other_key,
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
            self.member_sealed_with(index, note, &self.key).await
        }

        async fn member_sealed_with(
            &self,
            index: usize,
            note: Option<Note>,
            key: &TransactionEncryptionKey,
        ) -> Member {
            let mut transaction = self.chain.build_transaction(self.wallets[index].id());
            if let Some(note) = note {
                transaction = transaction.unauthenticated_input_note(note);
            }
            let executed = transaction.build().unwrap().execute().await.unwrap();
            let proven = LocalTransactionProver::default()
                .prove(executed.tx_inputs().clone())
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
                self.rpc.clone(),
                self.rpc.clone(),
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

    #[tokio::test]
    async fn a_batch_sealed_against_a_stale_key_is_not_processed_and_not_resent() {
        let f = fixture(2).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        f.rpc.fail_next_call(
            RpcEndpoint::SubmitProvenBatch,
            refused(GrpcError::FailedPrecondition),
        );
        let batcher = f.batcher(2, Duration::from_secs(5));
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert!(
            matches!(first, SubmissionOutcome::NotProcessed { .. }),
            "{first:?}"
        );
        assert!(
            matches!(second, SubmissionOutcome::NotProcessed { .. }),
            "{second:?}"
        );
        assert!(
            f.commit_block().await.is_empty(),
            "each transaction alone carries the same stale sealing"
        );
    }

    #[tokio::test]
    async fn a_shutdown_sends_a_collecting_batch_at_once() {
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
        assert_eq!(
            f.rpc.submitted_batch_sealed_inputs().len(),
            1,
            "the two waiting transactions still left as one batch"
        );
        assert_eq!(f.commit_block().await.len(), 2);

        let c = f.member(0).await;
        assert_eq!(
            batcher.submit(&f.genesis, c).await,
            SubmissionOutcome::Accepted,
            "after the shutdown a transaction is sent at once"
        );
        assert_eq!(f.rpc.submitted_batch_sealed_inputs().len(), 1);
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
    async fn a_stale_key_refusal_of_members_sealed_against_different_keys_sends_each_alone() {
        let f = fixture(2).await;
        let a = f.member(0).await;
        let b = f.member_sealed_with(1, None, &f.other_key).await;
        assert_ne!(a.sealed.key_id(), b.sealed.key_id());
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
        assert_eq!(
            f.commit_block().await.len(),
            2,
            "only one of them may carry the stale sealing, so each went alone"
        );
    }

    #[tokio::test]
    async fn a_transaction_without_time_to_wait_for_a_batch_is_sent_at_once() {
        let f = fixture(2).await;
        let window = Duration::from_secs(3);
        let mut short = f.member(0).await;
        short.send_by = Instant::now() + window + SendWindow::MIN_SEND - Duration::from_secs(1);
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

    /// On a paused clock, members that join with exactly a window and the least send time left
    /// have nothing to spare once the window ends, so the batch they would share is not built and
    /// each is sent alone with the send time it still has.
    #[tokio::test(start_paused = true)]
    async fn a_batch_not_built_within_its_members_time_sends_each_alone() {
        let f = fixture(2).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let window = Duration::from_millis(100);
        let send_by = Instant::now() + window + SendWindow::MIN_SEND;
        (a.send_by, b.send_by) = (send_by, send_by);
        let expected: BTreeSet<_> = [final_commitment(&a), final_commitment(&b)].into();
        let batcher = f.batcher(4, window);
        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted)
        );
        assert!(
            f.rpc.submitted_batch_sealed_inputs().is_empty(),
            "the late batch was never sent"
        );
        let landed: BTreeSet<_> = f.commit_block().await.into_iter().collect();
        assert_eq!(landed, expected);
    }

    /// A member left with less than the least send time when its window ends (a late timer) is
    /// sent at once, not after the others' batch is built, which may take up to their own spare
    /// time and so outlast what it has left.
    #[tokio::test]
    async fn a_member_short_of_send_time_is_sent_while_the_others_batch_is_built() {
        let f = fixture(3).await;
        let (a, b) = (f.member(0).await, f.member(1).await);
        let mut late = f.member(2).await;
        late.send_by = Instant::now() + SendWindow::MIN_SEND - Duration::from_secs(5);
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
    /// the proof keeps the prover until it ends, so the next group is sent alone as well rather
    /// than proved beside it.
    #[tokio::test]
    async fn a_batch_proof_its_members_stopped_waiting_for_keeps_the_prover_until_it_ends() {
        let f = fixture(4).await;
        let (mut a, mut b) = (f.member(0).await, f.member(1).await);
        let (c, d) = (f.member(2).await, f.member(3).await);
        let expected: BTreeSet<_> = [&a, &b, &c, &d].map(final_commitment).into();
        let window = Duration::from_millis(100);
        let batcher = f.batcher(4, window);
        let held = hold_proofs(&batcher);
        // Two seconds to spare for the build once the window ends; the held proof outlasts them.
        let send_by = Instant::now() + window + SendWindow::MIN_SEND + Duration::from_secs(2);
        (a.send_by, b.send_by) = (send_by, send_by);

        let (first, second) =
            tokio::join!(batcher.submit(&f.genesis, a), batcher.submit(&f.genesis, b));
        assert_eq!(
            (first, second),
            (SubmissionOutcome::Accepted, SubmissionOutcome::Accepted),
            "sent alone once the build ran out of time"
        );
        assert_eq!(held.started(), 1, "their batch proof started and is held");

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
}
