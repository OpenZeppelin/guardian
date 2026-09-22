//! The per-account candidate chain (issue #17).
//!
//! Candidates are queued in strict commitment order: each queued delta
//! builds on the post-state of the one before it, and the first builds
//! on the canonical state. The *tail* of the chain is the state a new
//! delta or proposal must build on. Because promotion of the oldest
//! candidate only moves the canonical base forward along the chain, the
//! tail commitment is invariant under promotion — a proposal pinned to
//! the tail stays viable while the chain drains.
//!
//! The tail's state JSON is never persisted: it is reconstructed on
//! demand by replaying the queued payloads on the canonical state, one
//! delta application per queued candidate, bounded by
//! [`CanonicalizationConfig::max_pending_candidates_per_account`].
//!
//! [`CanonicalizationConfig::max_pending_candidates_per_account`]:
//!     crate::canonicalization::CanonicalizationConfig::max_pending_candidates_per_account

use std::sync::Arc;

use serde_json::Value;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{ChainPosition, QueuedCandidate, StorageBackend, classify_chain_position};

/// The account's configured candidate queue depth. Optimistic mode has
/// no candidates at all, so the value is immaterial there; `1` keeps the
/// admission arithmetic well-defined.
pub fn max_pending_candidates(state: &AppState) -> usize {
    state
        .canonicalization
        .as_ref()
        .map(|config| config.max_pending_candidates_per_account)
        .unwrap_or(1)
        .max(1)
}

/// The state a new delta or proposal must build on: the post-state of
/// the newest queued candidate, or the canonical state itself when the
/// queue is empty.
#[derive(Debug, Clone)]
pub struct ChainTail {
    pub commitment: String,
    pub state_json: Value,
    /// Nonce of the newest queued candidate; `None` when the queue is
    /// empty (the canonical state is the tail).
    pub nonce: Option<u64>,
}

/// An account's queued candidates, nonce-ascending, validated as an
/// unbroken chain from the canonical commitment.
#[derive(Debug, Clone)]
pub struct CandidateChain {
    candidates: Vec<DeltaObject>,
}

impl CandidateChain {
    /// Load the account's candidates and validate that they chain from
    /// `stored_commitment`. A broken chain — a candidate whose base is
    /// neither the canonical state nor its predecessor's post-state —
    /// means a predecessor was parked, discarded, or abandoned and the
    /// worker has not yet swept the orphaned successors; nothing can be
    /// admitted against it, so the caller sees `ConflictPendingDelta`
    /// until the next full pass cleans the queue.
    pub async fn load(
        storage: &dyn StorageBackend,
        account_id: &str,
        stored_commitment: &str,
    ) -> Result<Self> {
        let candidates = storage
            .pull_candidate_deltas(account_id)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to load candidate queue"
                );
                GuardianError::StorageError(format!("Failed to load candidate queue: {e}"))
            })?;

        let mut running = stored_commitment;
        for candidate in &candidates {
            let chained = candidate.prev_commitment == running;
            let Some(next) = candidate.new_commitment.as_deref().filter(|_| chained) else {
                tracing::info!(
                    account_id = %account_id,
                    nonce = candidate.nonce,
                    prev_commitment = %candidate.prev_commitment,
                    chain_commitment = %running,
                    "Candidate queue does not chain from the canonical state at this nonce \
                     (a predecessor was parked or discarded and awaits the worker's sweep); \
                     refusing the submission"
                );
                return Err(GuardianError::ConflictPendingDelta);
            };
            running = next;
        }

        Ok(Self { candidates })
    }

    /// Load the chain for an admission decision. The caller's state read
    /// and the queue read are separate, so a promotion landing between
    /// them makes a healthy chain look broken (the head has become
    /// canonical and the stored state moved along the chain); rather
    /// than refuse such a race, the chain is re-validated once against a
    /// fresh state read, which then replaces `current_state` so the
    /// caller validates against one consistent base. A chain that is
    /// still broken against an unchanged state is refused as before.
    pub async fn load_for_admission(
        storage: &dyn StorageBackend,
        account_id: &str,
        current_state: &mut StateObject,
    ) -> Result<Self> {
        match Self::load(storage, account_id, &current_state.commitment).await {
            Err(GuardianError::ConflictPendingDelta) => {}
            outcome => return outcome,
        }
        let fresh = storage.pull_state(account_id).await.map_err(|e| {
            GuardianError::StorageError(format!("Failed to re-read account state: {e}"))
        })?;
        if fresh.commitment == current_state.commitment {
            return Err(GuardianError::ConflictPendingDelta);
        }
        tracing::debug!(
            account_id = %account_id,
            "Canonical state moved during admission; re-validating the candidate queue"
        );
        *current_state = fresh;
        Self::load(storage, account_id, &current_state.commitment).await
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn candidates(&self) -> &[DeltaObject] {
        &self.candidates
    }

    /// Nonce of the newest queued candidate, if any.
    pub fn tail_nonce(&self) -> Option<u64> {
        self.candidates.last().map(|candidate| candidate.nonce)
    }

    /// The commitment a new submission must build on.
    pub fn tail_commitment<'a>(&'a self, stored_commitment: &'a str) -> &'a str {
        self.candidates
            .last()
            .and_then(|candidate| candidate.new_commitment.as_deref())
            .unwrap_or(stored_commitment)
    }

    /// Classify a submission's base against the chain — see
    /// [`ChainPosition`].
    pub fn position(&self, stored_commitment: &str, prev_commitment: &str) -> ChainPosition {
        let queue: Vec<QueuedCandidate> = self.candidates.iter().map(QueuedCandidate::of).collect();
        classify_chain_position(stored_commitment, &queue, prev_commitment)
    }

    /// Materialize the chain tail: the canonical state when the queue is
    /// empty, otherwise the canonical state with every queued payload
    /// applied in nonce order. The replay runs on the shared
    /// reconstruction pool (the same CPU gate `push_delta` already
    /// takes). The replayed commitment must reproduce the stored tail
    /// commitment — the queued rows were written from exactly this
    /// computation — so a mismatch is an internal inconsistency, never a
    /// client error.
    pub async fn reconstruct_tail(
        &self,
        state: &AppState,
        current_state: &StateObject,
    ) -> Result<ChainTail> {
        let Some(tail) = self.candidates.last() else {
            return Ok(ChainTail {
                commitment: current_state.commitment.clone(),
                state_json: current_state.state_json.clone(),
                nonce: None,
            });
        };
        let expected_commitment = self.tail_commitment(&current_state.commitment).to_string();
        let tail_nonce = tail.nonce;

        let client = state.network_client.clone();
        let base_json = current_state.state_json.clone();
        let base_commitment = current_state.commitment.clone();
        let payloads: Vec<Arc<Value>> = self
            .candidates
            .iter()
            .map(|candidate| Arc::new(candidate.delta_payload.clone()))
            .collect();
        let (state_json, commitment) = crate::network::reconstructor()
            .run(move || {
                let mut state_json = base_json;
                let mut commitment = base_commitment;
                for payload in payloads {
                    (state_json, commitment) = client.apply_delta(&state_json, &payload)?;
                }
                Ok((state_json, commitment))
            })
            .await
            .map_err(|e| {
                // A queued payload that no longer applies is corruption
                // of the server's own queue, not a malformed client
                // request: surface it as a storage fault, not 400.
                tracing::error!(
                    account_id = %current_state.account_id,
                    tail_nonce,
                    error = %GuardianError::from(e),
                    "Failed to replay the candidate queue onto the canonical state"
                );
                GuardianError::StorageError(
                    "candidate queue could not be replayed onto the canonical state".to_string(),
                )
            })?;

        if commitment != expected_commitment {
            tracing::error!(
                account_id = %current_state.account_id,
                tail_nonce,
                stored = %expected_commitment,
                replayed = %commitment,
                "Candidate queue replay does not reproduce the stored tail commitment"
            );
            return Err(GuardianError::StorageError(
                "candidate queue replay does not match the stored tail commitment".to_string(),
            ));
        }

        Ok(ChainTail {
            commitment,
            state_json,
            nonce: Some(tail_nonce),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};

    fn candidate(nonce: u64, prev: &str, new: Option<&str>) -> DeltaObject {
        DeltaObject {
            account_id: "0xacc".to_string(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: new.map(str::to_string),
            delta_payload: serde_json::json!({"nonce": nonce}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2026-01-01T00:00:00Z".to_string()),
            metadata: None,
        }
    }

    fn stored_state() -> StateObject {
        StateObject {
            account_id: "0xacc".to_string(),
            commitment: "0xbase".to_string(),
            state_json: serde_json::json!({"step": 0}),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    #[tokio::test]
    async fn empty_queue_tail_is_the_canonical_state() {
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![]));
        let chain = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect("empty queue loads");
        assert!(chain.is_empty());
        assert_eq!(chain.tail_commitment("0xbase"), "0xbase");
        assert_eq!(chain.tail_nonce(), None);
        assert_eq!(chain.position("0xbase", "0xbase"), ChainPosition::Tail);
        assert_eq!(chain.position("0xbase", "0xelse"), ChainPosition::Unrelated);

        let state = create_test_app_state_with_mocks(
            Arc::new(storage),
            Arc::new(MockNetworkClient::new()),
            Arc::new(MockMetadataStore::new()),
        );
        let tail = chain
            .reconstruct_tail(&state, &stored_state())
            .await
            .expect("no replay needed");
        assert_eq!(tail.commitment, "0xbase");
        assert_eq!(tail.state_json, serde_json::json!({"step": 0}));
        assert_eq!(tail.nonce, None);
    }

    #[tokio::test]
    async fn chained_queue_replays_to_the_stored_tail_commitment() {
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![
            candidate(1, "0xbase", Some("0xc1")),
            candidate(2, "0xc1", Some("0xc2")),
        ]));
        let chain = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect("chained queue loads");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain.tail_commitment("0xbase"), "0xc2");
        assert_eq!(chain.tail_nonce(), Some(2));
        assert_eq!(chain.position("0xbase", "0xc2"), ChainPosition::Tail);
        assert_eq!(chain.position("0xbase", "0xc1"), ChainPosition::Competing);
        assert_eq!(chain.position("0xbase", "0xbase"), ChainPosition::Competing);
        assert_eq!(chain.position("0xbase", "0xzzz"), ChainPosition::Unrelated);

        // The mock pops responses LIFO: queue the second application first.
        let network = MockNetworkClient::new()
            .with_apply_delta(Ok((serde_json::json!({"step": 2}), "0xc2".to_string())))
            .with_apply_delta(Ok((serde_json::json!({"step": 1}), "0xc1".to_string())));
        let state = create_test_app_state_with_mocks(
            Arc::new(storage),
            Arc::new(network),
            Arc::new(MockMetadataStore::new()),
        );
        let tail = chain
            .reconstruct_tail(&state, &stored_state())
            .await
            .expect("replay reproduces the tail");
        assert_eq!(tail.commitment, "0xc2");
        assert_eq!(tail.state_json, serde_json::json!({"step": 2}));
        assert_eq!(tail.nonce, Some(2));
    }

    #[tokio::test]
    async fn replay_that_disagrees_with_the_stored_tail_is_a_storage_fault() {
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![candidate(
            1,
            "0xbase",
            Some("0xc1"),
        )]));
        let chain = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect("queue loads");
        let network = MockNetworkClient::new()
            .with_apply_delta(Ok((serde_json::json!({}), "0xnot_c1".to_string())));
        let state = create_test_app_state_with_mocks(
            Arc::new(storage),
            Arc::new(network),
            Arc::new(MockMetadataStore::new()),
        );
        let err = chain
            .reconstruct_tail(&state, &stored_state())
            .await
            .expect_err("mismatched replay is refused");
        assert!(matches!(err, GuardianError::StorageError(_)), "{err:?}");
    }

    #[tokio::test]
    async fn broken_chain_refuses_admission_as_pending_conflict() {
        // An orphan: nonce 2 chains from a commitment that is neither
        // the base nor nonce 1's post-state.
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![
            candidate(1, "0xbase", Some("0xc1")),
            candidate(2, "0xstale", Some("0xc2")),
        ]));
        let err = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect_err("broken chain is refused");
        assert!(
            matches!(err, GuardianError::ConflictPendingDelta),
            "{err:?}"
        );

        // A candidate without a stored post-state cannot be chained from.
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![
            candidate(1, "0xbase", None),
            candidate(2, "0xc1", Some("0xc2")),
        ]));
        let err = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect_err("unchainable candidate is refused");
        assert!(
            matches!(err, GuardianError::ConflictPendingDelta),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn admission_load_tolerates_a_promotion_between_the_two_reads() {
        // The caller read the state at 0xbase; the worker then promoted
        // nonce 1 (state → 0xc1) before the queue read returned [nonce 2].
        // Reads pop LIFO: the fresh state read comes first, then the
        // reload of the queue.
        let storage = MockStorageBackend::new()
            .with_pull_candidate_deltas(Ok(vec![candidate(2, "0xc1", Some("0xc2"))]))
            .with_pull_candidate_deltas(Ok(vec![candidate(2, "0xc1", Some("0xc2"))]))
            .with_pull_state(Ok(StateObject {
                commitment: "0xc1".to_string(),
                state_json: serde_json::json!({"step": 1}),
                ..stored_state()
            }));
        let mut current_state = stored_state();
        let chain = CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state)
            .await
            .expect("the race is tolerated");
        assert_eq!(current_state.commitment, "0xc1");
        assert_eq!(chain.tail_commitment(&current_state.commitment), "0xc2");

        // Unchanged state: the queue really is broken.
        let storage = MockStorageBackend::new()
            .with_pull_candidate_deltas(Ok(vec![candidate(2, "0xgone", Some("0xc2"))]))
            .with_pull_state(Ok(stored_state()));
        let mut current_state = stored_state();
        let err = CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state)
            .await
            .expect_err("a genuinely broken queue is refused");
        assert!(
            matches!(err, GuardianError::ConflictPendingDelta),
            "{err:?}"
        );
        assert_eq!(current_state.commitment, "0xbase");
    }

    #[tokio::test]
    async fn queue_read_failure_is_a_storage_error() {
        let storage =
            MockStorageBackend::new().with_pull_candidate_deltas(Err("disk on fire".to_string()));
        let err = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect_err("read failure surfaces");
        assert!(matches!(err, GuardianError::StorageError(_)), "{err:?}");
    }

    #[test]
    fn depth_defaults_to_one_in_optimistic_mode() {
        let state = create_test_app_state_with_mocks(
            Arc::new(MockStorageBackend::new()),
            Arc::new(MockNetworkClient::new()),
            Arc::new(MockMetadataStore::new()),
        );
        assert!(state.canonicalization.is_none());
        assert_eq!(max_pending_candidates(&state), 1);

        let mut state = state;
        state.canonicalization = Some(
            crate::canonicalization::CanonicalizationConfig::default()
                .with_max_pending_candidates_per_account(3),
        );
        assert_eq!(max_pending_candidates(&state), 3);
    }
}
