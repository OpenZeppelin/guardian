//! The state a new delta or proposal must build on (issue #517).
//!
//! With `GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT` above 1, a submission
//! must build on the queue tail, the post-state of the newest queued
//! candidate, which GUARDIAN never stores (`candidate_chain`). `/state` serves
//! the canonical state, so without this read only the device that pushed the
//! newest candidate could build the next transaction. The tail is replayed on
//! every read, one delta application per queued candidate on the
//! reconstruction pool, which is why it is a separate read rather than what
//! `/state` serves.

use guardian_shared::session_request::SessionOperation;
use serde::Serialize;

use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::candidate_chain::{self, CandidateChain, ChainTail};
use crate::services::get_canonical_nonce::state_read_error;
use crate::services::resolve_account_for_read;
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone)]
pub struct GetStateTailParams {
    pub account_id: String,
    pub credentials: Credentials,
}

/// A queued candidate the tail replays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct QueuedCandidate {
    /// The candidate's nonce: the account nonce its transaction leaves the
    /// account at.
    pub nonce: u64,
    /// The state it builds on: the canonical state's commitment for the
    /// oldest candidate, the previous candidate's `new_commitment` after it.
    pub prev_commitment: String,
    /// The state it leaves the account at.
    pub new_commitment: String,
}

/// The queue tail and the candidates that lead to it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StateTail {
    /// The tail: the canonical state with every queued candidate applied,
    /// shaped as `GET /state` serves a state. With nothing queued it is the
    /// canonical state. `updated_at` is when the newest candidate was queued.
    pub state: StateObject,
    /// The queued candidates, oldest first. The first builds on the
    /// canonical state, each later one on the one before it, and the last
    /// one's `new_commitment` is the tail's commitment. Empty when nothing is
    /// queued.
    pub queue: Vec<QueuedCandidate>,
    /// Whether the queue takes a delta or proposal built on this tail now.
    /// False while the queue is full (at the default depth of 1, whenever
    /// anything is queued), while the tail changes the account's signers or
    /// guardian key, or while the newest candidate was queued by a server
    /// before 0.19.0 under a label other than its nonce: a submission on the
    /// tail then gets `409 conflict_pending_delta`, so wait for the queue to
    /// drain and read again. True says only that the queue would take it at
    /// this read: the tail can move before the push, and the account's own
    /// status (paused, a Guardian execution in progress) and the proposal
    /// limits still apply.
    pub accepts_submissions: bool,
}

/// Read the account's queue tail, authorized like `GET /state`: by the
/// canonical state's signers, since requests are authorized against the
/// canonical metadata until a candidate promotes.
///
/// A queue that does not chain from the canonical state (a candidate left it
/// and the worker has not swept its successors yet) or whose replay does not
/// reproduce the stored tail has no tail to serve: `409
/// conflict_pending_delta`, as a submission behind it gets, until it drains.
#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn get_state_tail(state: &AppState, params: GetStateTailParams) -> Result<StateTail> {
    let resolved = resolve_account_for_read(
        state,
        &params.account_id,
        &params.credentials,
        SessionOperation::GetStateTail,
    )
    .await?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "get_state_tail".to_string(),
        });
    }

    let mut current_state = resolved
        .storage
        .pull_state(&params.account_id)
        .await
        .map_err(|error| state_read_error(&params.account_id, error))?;
    let chain = CandidateChain::load_for_read(
        resolved.storage.as_ref(),
        &params.account_id,
        &mut current_state,
    )
    .await?;
    let auth_scheme = resolved.metadata.auth.auth_scheme_label().to_string();
    if chain.is_empty() {
        // The depth is at least 1, so an empty queue has room, and its tail
        // is the canonical state itself, which changes no one's authority.
        return Ok(StateTail {
            state: StateObject {
                auth_scheme,
                ..current_state
            },
            queue: Vec::new(),
            accepts_submissions: true,
        });
    }
    let tail = chain.reconstruct_tail(state, &current_state).await?;
    let accepts_submissions = accepts_submissions(state, &chain, &current_state, &tail).await?;

    let queue = chain
        .candidates()
        .iter()
        .map(|candidate| {
            // `CandidateChain::load` refuses a queue with a candidate that has
            // no post-state, so this cannot happen; serving an empty
            // commitment for it would hide the inconsistency.
            let new_commitment = candidate.new_commitment.clone().ok_or_else(|| {
                tracing::error!(
                    account_id = %params.account_id,
                    nonce = candidate.nonce,
                    "A queued candidate has no post-state commitment"
                );
                GuardianError::StorageError(format!(
                    "queued candidate at nonce {} has no post-state commitment",
                    candidate.nonce
                ))
            })?;
            Ok(QueuedCandidate {
                nonce: candidate.nonce,
                prev_commitment: candidate.prev_commitment.clone(),
                new_commitment,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let updated_at = chain.candidates().last().map_or_else(
        || current_state.updated_at.clone(),
        |newest| newest.status.timestamp().to_string(),
    );

    Ok(StateTail {
        state: StateObject {
            account_id: current_state.account_id,
            state_json: tail.state_json,
            commitment: tail.commitment,
            nonce: tail.account_nonce,
            created_at: current_state.created_at,
            updated_at,
            auth_scheme,
        },
        queue,
        accepts_submissions,
    })
}

/// Whether a submission built on a non-empty queue's tail would be admitted
/// now, by the refusals `push_delta` and `push_delta_proposal` apply to the
/// tail itself, each a `409 conflict_pending_delta` until the queue drains: a
/// full queue, a tail queued under a label other than its nonce (a candidate
/// from a server before issue #518; a delta's nonce cannot exceed its label,
/// a proposal is refused once the tail is replayed), and a tail that changes
/// who may act on the account (`ensure_tail_keeps_auth`, whose refusal is
/// that 409; its faults stay errors).
async fn accepts_submissions(
    state: &AppState,
    chain: &CandidateChain,
    current_state: &StateObject,
    tail: &ChainTail,
) -> Result<bool> {
    if chain.len() >= candidate_chain::max_pending_candidates(state)
        || !tail.is_labelled_with_its_nonce()
    {
        return Ok(false);
    }
    match candidate_chain::ensure_tail_keeps_auth(state, current_state, tail).await {
        Ok(()) => Ok(true),
        Err(GuardianError::ConflictPendingDelta) => Ok(false),
        Err(fault) => Err(fault),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::testing::helpers::{TestSigner, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use guardian_shared::auth_request_payload::AuthRequestPayload;
    use std::sync::Arc;

    const ACCOUNT_ID: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
    const C0: &str = "0xc0";
    const C1: &str = "0xc1";
    const C2: &str = "0xc2";

    struct Setup {
        state: AppState,
        storage: MockStorageBackend,
        network: MockNetworkClient,
        signer: TestSigner,
    }

    /// An account `signer` cosigns, stored at `C0`.
    fn setup() -> Setup {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new();
        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );
        let signer = TestSigner::new();
        let _metadata = metadata.with_get(Ok(Some(AccountMetadata {
            account_id: ACCOUNT_ID.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer.commitment_hex.clone()],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })));
        Setup {
            state,
            storage,
            network,
            signer,
        }
    }

    impl Setup {
        fn with_depth(mut self, depth: usize) -> Self {
            self.state.canonicalization = Some(
                crate::canonicalization::CanonicalizationConfig::default()
                    .with_max_pending_candidates_per_account(depth),
            );
            self
        }
    }

    /// Who may act on a state, as `account_auth_binding` reads it.
    fn binding(signers: &[&str]) -> crate::network::AuthBinding {
        crate::network::AuthBinding {
            signers: signers
                .iter()
                .map(|signer| crate::metadata::auth::ChainCosigner {
                    commitment: signer.to_string(),
                    scheme: None,
                })
                .collect(),
            guardian: Some("0xguardian".to_string()),
            ..Default::default()
        }
    }

    fn stored(commitment: &str) -> StateObject {
        StateObject {
            account_id: ACCOUNT_ID.to_string(),
            commitment: commitment.to_string(),
            nonce: Some(4),
            state_json: serde_json::json!({ "data": commitment }),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn candidate(nonce: u64, prev: &str, new: &str, queued_at: &str) -> DeltaObject {
        DeltaObject {
            account_id: ACCOUNT_ID.to_string(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: Some(new.to_string()),
            delta_payload: serde_json::json!({ "data": format!("delta {nonce}") }),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate(queued_at.to_string()),
            metadata: None,
        }
    }

    /// Signed the way the HTTP layer signs `StateQuery`.
    fn params(signer: &TestSigner) -> GetStateTailParams {
        let payload = serde_json::json!({ "account_id": ACCOUNT_ID });
        let (signature, timestamp) = signer.sign_json_payload(ACCOUNT_ID, &payload);
        let bytes = serde_json::to_vec(&payload).expect("payload serializes");
        GetStateTailParams {
            account_id: ACCOUNT_ID.to_string(),
            credentials: Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
                .with_request_payload(AuthRequestPayload::from_bytes(&bytes))
                .with_request_payload_bytes(bytes),
        }
    }

    #[tokio::test]
    async fn serves_the_canonical_state_when_nothing_is_queued() {
        let setup = setup();
        let _storage = setup.storage.clone().with_pull_state(Ok(stored(C0)));

        let tail = get_state_tail(&setup.state, params(&setup.signer))
            .await
            .expect("the canonical state is the tail");

        assert_eq!(tail.state.commitment, C0);
        assert_eq!(tail.state.state_json, serde_json::json!({ "data": C0 }));
        assert_eq!(tail.state.updated_at, "2024-11-14T12:00:00Z");
        assert_eq!(tail.state.auth_scheme, "falcon");
        assert!(tail.queue.is_empty());
        assert!(
            tail.accepts_submissions,
            "an empty queue has room at any depth"
        );
    }

    #[tokio::test]
    async fn replays_every_queued_candidate_onto_the_canonical_state() {
        let setup = setup().with_depth(4);
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![
                candidate(5, C0, C1, "2024-11-14T12:01:00Z"),
                candidate(6, C1, C2, "2024-11-14T12:02:00Z"),
            ]));
        // Answers pop last-in first: the first replay step gets the last.
        let _network = setup
            .network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({ "data": C2 }), C2.to_string())))
            .with_apply_delta(Ok((serde_json::json!({ "data": C1 }), C1.to_string())));

        let tail = get_state_tail(&setup.state, params(&setup.signer))
            .await
            .expect("the queue replays to its stored tail");

        assert_eq!(tail.state.commitment, C2);
        assert_eq!(tail.state.state_json, serde_json::json!({ "data": C2 }));
        assert_eq!(tail.state.account_id, ACCOUNT_ID);
        assert_eq!(tail.state.created_at, "2024-11-14T12:00:00Z");
        assert_eq!(
            tail.state.updated_at, "2024-11-14T12:02:00Z",
            "when the newest candidate was queued"
        );
        assert_eq!(
            tail.queue,
            vec![
                QueuedCandidate {
                    nonce: 5,
                    prev_commitment: C0.to_string(),
                    new_commitment: C1.to_string(),
                },
                QueuedCandidate {
                    nonce: 6,
                    prev_commitment: C1.to_string(),
                    new_commitment: C2.to_string(),
                },
            ]
        );
        assert!(
            setup
                .network
                .apply_delta_responses
                .lock()
                .unwrap()
                .is_empty(),
            "one application per queued candidate"
        );
        assert!(tail.accepts_submissions, "two queued at depth four");
    }

    /// A full queue refuses anything built on its tail, so the tail is
    /// served with `accepts_submissions` false: at the default depth of 1
    /// whenever anything is queued, and at any depth once it is full.
    #[tokio::test]
    async fn a_full_queue_accepts_no_submissions() {
        let one = [candidate(5, C0, C1, "2024-11-14T12:01:00Z")];
        let two = [
            candidate(5, C0, C1, "2024-11-14T12:01:00Z"),
            candidate(6, C1, C2, "2024-11-14T12:02:00Z"),
        ];
        for (depth, queue) in [(1, &one[..]), (2, &two[..])] {
            let setup = setup().with_depth(depth);
            let _storage = setup
                .storage
                .clone()
                .with_pull_state(Ok(stored(C0)))
                .with_pull_deltas_after(Ok(queue.to_vec()));
            let tail_commitment = queue.last().unwrap().new_commitment.clone().unwrap();
            let mut network = setup.network.clone();
            for queued in queue.iter().rev() {
                let new = queued.new_commitment.clone().unwrap();
                network = network.with_apply_delta(Ok((serde_json::json!({ "data": new }), new)));
            }

            let tail = get_state_tail(&setup.state, params(&setup.signer))
                .await
                .unwrap_or_else(|error| panic!("depth {depth}: {error:?}"));

            assert_eq!(tail.state.commitment, tail_commitment, "depth {depth}");
            assert_eq!(tail.queue.len(), queue.len(), "depth {depth}");
            assert!(!tail.accepts_submissions, "depth {depth}");
        }
    }

    /// Nothing chains behind a candidate that changes who may act on the
    /// account until it promotes, so its tail accepts nothing even with room.
    #[tokio::test]
    async fn a_tail_that_changes_the_signers_accepts_no_submissions() {
        let setup = setup().with_depth(4);
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![candidate(5, C0, C1, "2024-11-14T12:01:00Z")]));
        // Bindings pop last-in first: the canonical state is read first.
        let _network = setup
            .network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({ "data": C1 }), C1.to_string())))
            .with_account_auth_binding(Ok(Some(binding(&["0xaa", "0xbb"]))))
            .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))));

        let tail = get_state_tail(&setup.state, params(&setup.signer))
            .await
            .expect("the tail is still served");

        assert_eq!(tail.state.commitment, C1);
        assert!(!tail.accepts_submissions);
    }

    /// The same bindings on both states: the tail takes a submission.
    #[tokio::test]
    async fn a_tail_that_keeps_the_signers_accepts_submissions() {
        let setup = setup().with_depth(4);
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![candidate(5, C0, C1, "2024-11-14T12:01:00Z")]));
        let _network = setup
            .network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({ "data": C1 }), C1.to_string())))
            .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))))
            .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))));

        let tail = get_state_tail(&setup.state, params(&setup.signer))
            .await
            .expect("the tail is served");

        assert!(tail.accepts_submissions);
    }

    /// A candidate a server before issue #518 queued under a timestamp label:
    /// no delta or proposal can carry a nonce past that label and also be
    /// the nonce it leaves the account at, so nothing is admitted behind it
    /// until it leaves the queue.
    #[tokio::test]
    async fn a_tail_queued_under_a_timestamp_label_accepts_no_submissions() {
        let setup = setup().with_depth(4);
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![candidate(
                1_790_941_600_874,
                C0,
                C1,
                "2024-11-14T12:01:00Z",
            )]));
        let _network = setup
            .network
            .clone()
            .with_applied_state(Ok(crate::network::AppliedState {
                state_json: serde_json::json!({ "data": C1 }),
                commitment: C1.to_string(),
                nonce: Some(5),
                cosigners: Vec::new(),
                guardian_commitment: None,
            }));

        let tail = get_state_tail(&setup.state, params(&setup.signer))
            .await
            .expect("the tail is still served");

        assert_eq!(tail.state.nonce, Some(5));
        assert!(!tail.accepts_submissions);
    }

    /// A binding that cannot be read is a server fault, not a refusal: the
    /// read fails rather than serve a flag it could not judge.
    #[tokio::test]
    async fn an_unreadable_auth_binding_is_a_server_error() {
        let setup = setup().with_depth(4);
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![candidate(5, C0, C1, "2024-11-14T12:01:00Z")]));
        let _network = setup
            .network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({ "data": C1 }), C1.to_string())))
            .with_account_auth_binding(Err("account version is 241".to_string()));

        let result = get_state_tail(&setup.state, params(&setup.signer)).await;

        assert!(
            matches!(result, Err(GuardianError::StorageError(_))),
            "{result:?}"
        );
    }

    /// A candidate left the queue (parked) and its successor awaits the
    /// worker's sweep: nothing chains from the canonical state.
    #[tokio::test]
    async fn a_queue_a_parked_candidate_broke_has_no_tail() {
        let setup = setup();
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![candidate(6, C1, C2, "2024-11-14T12:02:00Z")]));

        let result = get_state_tail(&setup.state, params(&setup.signer)).await;

        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
    }

    /// The oldest candidate promoted between the state read and the queue
    /// read: the queue chains from the promoted state, which is read again.
    #[tokio::test]
    async fn a_promotion_between_the_reads_is_served_from_the_promoted_state() {
        let setup = setup();
        // State reads pop last-in first: the stale read comes first.
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C1)))
            .with_pull_state(Ok(stored(C0)))
            .with_pull_state_commitment(C1)
            .with_pull_deltas_after(Ok(vec![candidate(6, C1, C2, "2024-11-14T12:02:00Z")]))
            .with_pull_deltas_after(Ok(vec![candidate(6, C1, C2, "2024-11-14T12:02:00Z")]));
        let _network = setup
            .network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({ "data": C2 }), C2.to_string())));

        let tail = get_state_tail(&setup.state, params(&setup.signer))
            .await
            .expect("served from the promoted state");

        assert_eq!(tail.state.commitment, C2);
        assert_eq!(tail.queue.len(), 1);
        assert_eq!(tail.queue[0].prev_commitment, C1);
    }

    /// The stored tail is the result of this very replay; a replay landing
    /// elsewhere is an inconsistency nothing can be built on.
    #[tokio::test]
    async fn a_replay_that_misses_the_stored_tail_has_no_tail() {
        let setup = setup();
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Ok(stored(C0)))
            .with_pull_deltas_after(Ok(vec![candidate(5, C0, C1, "2024-11-14T12:01:00Z")]));
        let _network = setup.network.clone().with_apply_delta(Ok((
            serde_json::json!({ "data": "elsewhere" }),
            "0xelsewhere".to_string(),
        )));

        let result = get_state_tail(&setup.state, params(&setup.signer)).await;

        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_state_is_not_found() {
        let setup = setup();
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Err("state not found".to_string()));

        let result = get_state_tail(&setup.state, params(&setup.signer)).await;

        assert!(
            matches!(&result, Err(GuardianError::StateNotFound(id)) if id == ACCOUNT_ID),
            "{result:?}"
        );
    }

    /// A storage fault is a retryable server error, not a 404 a client could
    /// read as an unknown account (or as a server without the route).
    #[tokio::test]
    async fn a_storage_fault_is_a_storage_error() {
        let setup = setup();
        let _storage = setup
            .storage
            .clone()
            .with_pull_state(Err("connection pool timed out".to_string()));

        let result = get_state_tail(&setup.state, params(&setup.signer)).await;

        assert!(
            matches!(result, Err(GuardianError::StorageError(_))),
            "{result:?}"
        );
    }

    /// Authorized like `GET /state`: by the canonical state's signers.
    #[tokio::test]
    async fn a_signer_outside_the_account_is_refused() {
        let setup = setup();
        let _storage = setup.storage.clone().with_pull_state(Ok(stored(C0)));

        let result = get_state_tail(&setup.state, params(&TestSigner::new())).await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(
            setup.storage.pending_pull_state_responses(),
            1,
            "refused before the state is read"
        );
    }
}
