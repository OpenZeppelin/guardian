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
use crate::network::{AuthBinding, ReconstructError};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{
    ChainPosition, QueuedCandidate, StorageBackend, classify_chain_position,
    first_unchained_candidate,
};

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

        let queue: Vec<QueuedCandidate> = candidates.iter().map(QueuedCandidate::of).collect();
        if let Some(index) = first_unchained_candidate(stored_commitment, &queue) {
            let candidate = &candidates[index];
            tracing::info!(
                account_id = %account_id,
                nonce = candidate.nonce,
                prev_commitment = %candidate.prev_commitment,
                stored_commitment = %stored_commitment,
                "Candidate queue does not chain from the canonical state at this nonce \
                 (a predecessor was parked or discarded and awaits the worker's sweep); \
                 refusing the submission"
            );
            return Err(GuardianError::ConflictPendingDelta);
        }

        Ok(Self { candidates })
    }

    /// Load the chain for an admission decision. The caller's state read
    /// and the queue read are separate, so a promotion landing between
    /// them leaves the two disagreeing, in one of two ways:
    ///
    /// - candidates remain behind the promoted one: they no longer chain
    ///   from the state read, and the chain looks broken;
    /// - the promoted candidate was the last one: the queue is empty,
    ///   which agrees with any state read, so the stale base goes
    ///   unnoticed and the submission is judged against a commitment the
    ///   account has already left.
    ///
    /// Rather than refuse the first or misjudge the second, the stored
    /// commitment is read again when the verdict depends on it, and on a
    /// change the state is re-read and replaces `current_state`, so the
    /// caller validates against one consistent base. The empty case
    /// matters when there is no `base` (a proposal, pinned to whatever
    /// tail this returns) or when the submission's `base` is not the
    /// state read (a delta that would otherwise be refused as unrelated,
    /// with a stale commitment to resync to). A chain that is still
    /// broken against an unchanged state is refused as before.
    pub async fn load_for_admission(
        storage: &dyn StorageBackend,
        account_id: &str,
        current_state: &mut StateObject,
        base: Option<&str>,
    ) -> Result<Self> {
        let first = Self::load(storage, account_id, &current_state.commitment).await;
        let recheck = match &first {
            Err(GuardianError::ConflictPendingDelta) => true,
            Ok(chain) => {
                chain.is_empty() && base.is_none_or(|base| base != current_state.commitment)
            }
            Err(_) => false,
        };
        if !recheck {
            return first;
        }
        let stored_commitment = storage
            .pull_state_commitment(account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to re-read account state: {e}"))
            })?;
        if stored_commitment == current_state.commitment {
            return first;
        }
        let fresh = storage.pull_state(account_id).await.map_err(|e| {
            GuardianError::StorageError(format!("Failed to re-read account state: {e}"))
        })?;
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
    /// computation — so a payload that no longer applies, or a replay
    /// that lands elsewhere, is an internal inconsistency, never a client
    /// error. It can only come from a server whose delta application
    /// changed while the candidates were queued (an upgrade); it is
    /// logged as an error and refused as the pending conflict it is from
    /// the client's side: nothing can be admitted behind the queue until
    /// the worker drains it, so the client waits and retries, as for a
    /// full queue. A replay task that never completed (panic, runtime
    /// shutdown) stays a server fault.
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
                    let applied = client.apply_delta(&state_json, &payload)?;
                    state_json = applied.state_json;
                    commitment = applied.commitment;
                }
                Ok((state_json, commitment))
            })
            .await
            .map_err(|e| match e {
                ReconstructError::Operation(message) => {
                    tracing::error!(
                        account_id = %current_state.account_id,
                        tail_nonce,
                        error = %message,
                        "Failed to replay the candidate queue onto the canonical state; \
                         refusing submissions until the queue drains"
                    );
                    GuardianError::ConflictPendingDelta
                }
                // The replay never ran to completion: a server fault, not
                // a property of the queue.
                task => GuardianError::from(task),
            })?;

        if commitment != expected_commitment {
            tracing::error!(
                account_id = %current_state.account_id,
                tail_nonce,
                stored = %expected_commitment,
                replayed = %commitment,
                "Candidate queue replay does not reproduce the stored tail commitment; \
                 refusing submissions until the queue drains"
            );
            return Err(GuardianError::ConflictPendingDelta);
        }

        Ok(ChainTail {
            commitment,
            state_json,
            nonce: Some(tail_nonce),
        })
    }
}

/// Refuse to chain behind a candidate that changes who may act on the
/// account (issue #17): the signer set or the guardian key its post-state
/// binds differs from the canonical state's. Requests are authorized
/// against the canonical metadata until a candidate promotes, so while a
/// signer-changing candidate is queued a signer it removes can still push
/// and propose and a signer it adds is still refused; and a successor this
/// server acknowledges behind a queued guardian switch is signed by a key
/// the post-switch account does not accept, so it can never land. Nothing
/// is admitted behind such a candidate until it promotes (which syncs the
/// cosigner set) or leaves the queue: `409 conflict_pending_delta`, as for
/// a full queue. Judged on the replayed tail, not on a proposal label: a
/// direct push changes signers without any proposal type. Both bindings
/// are read in one task on the reconstruction pool, as the replay that
/// produced the tail was. An empty queue has nothing to compare, the tail
/// being the canonical state itself.
///
/// This rule is judged in the request path only, unlike the chain-position
/// rules the storage gate re-evaluates under the account lock. That is
/// safe: a successor can only name a tail that exists once its candidate
/// is admitted, and a candidate admitted between this check and the lock
/// moves the tail, so the in-lock position check refuses the successor as
/// competing.
pub async fn ensure_tail_keeps_auth(
    state: &AppState,
    current_state: &StateObject,
    tail: &ChainTail,
) -> Result<()> {
    let Some(tail_nonce) = tail.nonce else {
        return Ok(());
    };
    let client = state.network_client.clone();
    let canonical_json = current_state.state_json.clone();
    let tail_json = tail.state_json.clone();
    let bindings: Option<(AuthBinding, AuthBinding)> = crate::network::reconstructor()
        .run(move || {
            let canonical = client.account_auth_binding(&canonical_json)?;
            let tail = client.account_auth_binding(&tail_json)?;
            Ok(canonical.zip(tail))
        })
        .await
        .map_err(|error| match error {
            // Both states decode for the replay that produced the tail, so
            // a read that fails is an internal fault, never a property of
            // the submission.
            ReconstructError::Operation(message) => {
                tracing::error!(
                    account_id = %current_state.account_id,
                    tail_nonce,
                    error = %message,
                    "Failed to read the auth binding of the canonical state or the queue tail"
                );
                GuardianError::StorageError(format!(
                    "Failed to read the auth binding of the canonical state or the queue tail: {message}"
                ))
            }
            task => GuardianError::from(task),
        })?;
    let Some((canonical, tail_binding)) = bindings else {
        // A network without the notion: nothing to compare.
        return Ok(());
    };
    if tail_binding != canonical {
        tracing::info!(
            account_id = %current_state.account_id,
            tail_nonce,
            signers_changed = tail_binding.signers != canonical.signers,
            guardian_changed = tail_binding.guardian != canonical.guardian,
            "The newest queued candidate changes who may act on the account; \
             refusing to chain behind it until it promotes"
        );
        return Err(GuardianError::ConflictPendingDelta);
    }
    Ok(())
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
            nonce: None,
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
    async fn replay_that_disagrees_with_the_stored_tail_is_a_pending_conflict() {
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
        assert!(
            matches!(err, GuardianError::ConflictPendingDelta),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn payload_that_no_longer_applies_is_a_pending_conflict() {
        // An upgrade changed delta application while a candidate was
        // queued: the replay fails. The client cannot fix that, so it is
        // told to wait for the queue to drain, not handed a 500.
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![candidate(
            1,
            "0xbase",
            Some("0xc1"),
        )]));
        let chain = CandidateChain::load(&storage, "0xacc", "0xbase")
            .await
            .expect("queue loads");
        let network =
            MockNetworkClient::new().with_apply_delta(Err("unknown field `fee`".to_string()));
        let state = create_test_app_state_with_mocks(
            Arc::new(storage),
            Arc::new(network),
            Arc::new(MockMetadataStore::new()),
        );
        let err = chain
            .reconstruct_tail(&state, &stored_state())
            .await
            .expect_err("an unreplayable queue is refused");
        assert!(
            matches!(err, GuardianError::ConflictPendingDelta),
            "{err:?}"
        );
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
        let chain =
            CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state, Some("0xc2"))
                .await
                .expect("the race is tolerated");
        assert_eq!(current_state.commitment, "0xc1");
        assert_eq!(chain.tail_commitment(&current_state.commitment), "0xc2");

        // Unchanged state: the queue really is broken.
        let storage = MockStorageBackend::new()
            .with_pull_candidate_deltas(Ok(vec![candidate(2, "0xgone", Some("0xc2"))]))
            .with_pull_state(Ok(stored_state()));
        let mut current_state = stored_state();
        let err =
            CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state, Some("0xc2"))
                .await
                .expect_err("a genuinely broken queue is refused");
        assert!(
            matches!(err, GuardianError::ConflictPendingDelta),
            "{err:?}"
        );
        assert_eq!(current_state.commitment, "0xbase");
    }

    fn state_at(commitment: &str, step: u64) -> StateObject {
        StateObject {
            commitment: commitment.to_string(),
            state_json: serde_json::json!({ "step": step }),
            ..stored_state()
        }
    }

    #[tokio::test]
    async fn admission_load_sees_the_last_candidate_promoted_between_the_two_reads() {
        // The caller read the state at 0xbase; the worker then promoted
        // the only queued candidate (state → 0xc1), so the queue read
        // comes back empty. An empty queue agrees with any state read:
        // without a second look, a delta built on 0xc1 (the real tail)
        // would be judged against the stale 0xbase and refused as
        // unrelated. Reads pop LIFO: the commitment re-read peeks at the
        // fresh state, the full re-read takes it.
        let storage = MockStorageBackend::new()
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_state(Ok(state_at("0xc1", 1)));
        let mut current_state = stored_state();
        let chain =
            CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state, Some("0xc1"))
                .await
                .expect("the empty queue reloads against the fresh state");
        assert!(chain.is_empty());
        assert_eq!(current_state.commitment, "0xc1");
        assert_eq!(current_state.state_json, serde_json::json!({ "step": 1 }));
        assert_eq!(
            chain.position(&current_state.commitment, "0xc1"),
            ChainPosition::Tail
        );

        // A proposal has no base of its own: it is pinned to the tail this
        // returns, so the empty queue is re-checked for it too.
        let storage = MockStorageBackend::new()
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_state(Ok(state_at("0xc1", 1)));
        let mut current_state = stored_state();
        CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state, None)
            .await
            .expect("the empty queue reloads against the fresh state");
        assert_eq!(current_state.commitment, "0xc1");
    }

    #[tokio::test]
    async fn admission_load_keeps_a_settled_empty_queue_without_a_full_re_read() {
        // The state did not move: the commitment re-read agrees, so the
        // first verdict stands and the full state is not read again.
        let storage = MockStorageBackend::new()
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_state(Ok(stored_state()));
        let mut current_state = stored_state();
        let chain = CandidateChain::load_for_admission(
            &storage,
            "0xacc",
            &mut current_state,
            Some("0xzzz"),
        )
        .await
        .expect("an empty queue loads");
        assert!(chain.is_empty());
        assert_eq!(current_state.commitment, "0xbase");
        assert_eq!(
            storage.pull_state_responses.lock().unwrap().len(),
            1,
            "only the commitment was re-read"
        );
        assert_eq!(chain.position("0xbase", "0xzzz"), ChainPosition::Unrelated);

        // A delta on the state read needs no second look at all: it is
        // either right, or refused in-lock against the fresh commitment.
        let storage = MockStorageBackend::new().with_pull_candidate_deltas(Ok(vec![]));
        let mut current_state = stored_state();
        CandidateChain::load_for_admission(&storage, "0xacc", &mut current_state, Some("0xbase"))
            .await
            .expect("no re-read is needed");
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

    fn binding(signers: &[&str], guardian: Option<&str>) -> AuthBinding {
        AuthBinding {
            signers: signers.iter().map(|s| s.to_string()).collect(),
            guardian: guardian.map(str::to_string),
        }
    }

    /// A network whose canonical state binds `canonical` and whose queue
    /// tail binds `tail` (answers pop LIFO: the canonical state is read
    /// first).
    fn network_binding(canonical: AuthBinding, tail: AuthBinding) -> MockNetworkClient {
        MockNetworkClient::new()
            .with_account_auth_binding(Ok(Some(tail)))
            .with_account_auth_binding(Ok(Some(canonical)))
    }

    fn queued_tail() -> ChainTail {
        ChainTail {
            commitment: "0xc1".to_string(),
            state_json: serde_json::json!({"step": 1}),
            nonce: Some(1),
        }
    }

    fn app_state_with(network: MockNetworkClient) -> AppState {
        create_test_app_state_with_mocks(
            Arc::new(MockStorageBackend::new()),
            Arc::new(network),
            Arc::new(MockMetadataStore::new()),
        )
    }

    #[tokio::test]
    async fn an_empty_queue_has_no_tail_to_compare_with_the_canonical_state() {
        // The canned answers must stay unread: the tail is the canonical
        // state itself, so there is nothing to read.
        let network = network_binding(
            binding(&["0xaa"], Some("0xg")),
            binding(&["0xzz"], Some("0xg")),
        );
        let state = app_state_with(network.clone());
        let tail = ChainTail {
            commitment: "0xbase".to_string(),
            state_json: serde_json::json!({"step": 0}),
            nonce: None,
        };
        ensure_tail_keeps_auth(&state, &stored_state(), &tail)
            .await
            .expect("nothing queued, nothing to refuse");
        assert_eq!(
            network.account_auth_binding_responses.lock().unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn a_tail_that_changes_the_signer_set_is_a_pending_conflict() {
        let canonical = binding(&["0xaa", "0xbb"], Some("0xg"));
        for tail in [
            binding(&["0xaa", "0xbb", "0xcc"], Some("0xg")), // a signer added
            binding(&["0xaa"], Some("0xg")),                 // a signer removed
            binding(&["0xaa", "0xdd"], Some("0xg")),         // a signer replaced
            binding(&[], Some("0xg")),                       // the roster gone
        ] {
            let state = app_state_with(network_binding(canonical.clone(), tail.clone()));
            let err = ensure_tail_keeps_auth(&state, &stored_state(), &queued_tail())
                .await
                .expect_err("a signer change behind the tail is refused");
            assert!(
                matches!(err, GuardianError::ConflictPendingDelta),
                "{tail:?}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_tail_that_changes_the_guardian_is_a_pending_conflict() {
        let canonical = binding(&["0xaa"], Some("0xthis-server"));
        for tail in [
            binding(&["0xaa"], Some("0xnew-guardian")),
            binding(&["0xaa"], None),
        ] {
            let state = app_state_with(network_binding(canonical.clone(), tail.clone()));
            let err = ensure_tail_keeps_auth(&state, &stored_state(), &queued_tail())
                .await
                .expect_err("a queued guardian switch is refused as a base");
            assert!(
                matches!(err, GuardianError::ConflictPendingDelta),
                "{tail:?}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_tail_with_the_same_binding_keeps_the_auth() {
        // A multisig roster, and an account layout without one: both
        // compare equal to themselves.
        for same in [binding(&["0xaa", "0xbb"], Some("0xg")), binding(&[], None)] {
            let network = network_binding(same.clone(), same.clone());
            let state = app_state_with(network.clone());
            ensure_tail_keeps_auth(&state, &stored_state(), &queued_tail())
                .await
                .expect("an unchanged binding does not block the queue");
            assert!(
                network
                    .account_auth_binding_responses
                    .lock()
                    .unwrap()
                    .is_empty(),
                "both states were read"
            );
        }
    }

    #[tokio::test]
    async fn a_network_without_auth_bindings_has_nothing_to_compare() {
        // The mock's default answer, and what an EVM client would say.
        let state = app_state_with(MockNetworkClient::new());
        ensure_tail_keeps_auth(&state, &stored_state(), &queued_tail())
            .await
            .expect("no notion of a binding, no refusal");
    }

    #[tokio::test]
    async fn an_unreadable_state_is_a_server_fault() {
        let network = MockNetworkClient::new()
            .with_account_auth_binding(Err("account version is 241".to_string()));
        let state = app_state_with(network);
        let err = ensure_tail_keeps_auth(&state, &stored_state(), &queued_tail())
            .await
            .expect_err("a state the replay produced must decode");
        assert!(matches!(err, GuardianError::StorageError(_)), "{err:?}");
    }
}
