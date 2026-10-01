use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::executor::{
    ExecutedTransactionInfo, ExecutionAttempt, ExecutionInput, GuardianAck, ProposalExecutor,
    ProvenTransactionInfo, SignatureSelection, SubmissionOutcome,
};
use super::{ExecutionState as ExecutionServices, RequestExecutionParams, request_execution};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::{Auth, Credentials};
use crate::services::execution_status::{
    ExecutionEnvelope, ExecutionState, current_execution, get_execution,
};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{
    CandidatePromotion, ExecutionFailure, ExecutionFailureCode, PromotableKind, PromoteWrite,
};
use crate::testing::helpers::{
    TestSigner, create_test_app_state, generate_falcon_signature_with_timestamp,
};

pub(crate) const ACCOUNT: &str = "0xa6cc46a9d729f0815cef58740803cb";
pub(crate) const BASE: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
pub(crate) const PROPOSAL: &str =
    "0x2222222222222222222222222222222222222222222222222222222222222222";
pub(crate) const NEW_COMMITMENT: &str = "mock_new_commitment";

pub(crate) fn stored_signature() -> crate::delta_object::CosignerSignature {
    crate::delta_object::CosignerSignature {
        signer_id: "0xstored".to_string(),
        signature: guardian_shared::ProposalSignature::Falcon {
            signature: "0xstored".to_string(),
        },
        timestamp: "2026-09-30T12:00:00Z".to_string(),
    }
}

fn fixture_summary() -> serde_json::Value {
    let delta: serde_json::Value =
        serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON).unwrap();
    delta["delta_payload"].clone()
}

#[derive(Clone)]
pub(crate) struct Script {
    pub(crate) required: usize,
    pub(crate) valid: usize,
    pub(crate) prepare: std::result::Result<(), ExecutionFailure>,
    pub(crate) execute: std::result::Result<(), ExecutionFailure>,
    pub(crate) prove: std::result::Result<(), ExecutionFailure>,
    pub(crate) seal: std::result::Result<(), ExecutionFailure>,
    pub(crate) submission: SubmissionOutcome,
    pub(crate) tip: std::result::Result<u32, String>,
    pub(crate) expiration_block: u32,
    pub(crate) proving_time: Duration,
    pub(crate) sealing_time: Duration,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            required: 2,
            valid: 2,
            prepare: Ok(()),
            execute: Ok(()),
            prove: Ok(()),
            seal: Ok(()),
            submission: SubmissionOutcome::Accepted,
            tip: Ok(100),
            expiration_block: 356,
            proving_time: Duration::ZERO,
            sealing_time: Duration::ZERO,
        }
    }
}

#[derive(Default)]
pub(crate) struct Calls {
    pub(crate) prepared: usize,
    pub(crate) prepared_inputs: Vec<ExecutionInput>,
    pub(crate) executed_with_ack: Vec<bool>,
    pub(crate) proved: usize,
    pub(crate) submitted: usize,
}

struct FakeExecutor {
    script: Arc<Mutex<Script>>,
    calls: Arc<Mutex<Calls>>,
}

#[async_trait]
impl ProposalExecutor for FakeExecutor {
    fn select_signatures(&self, _input: &ExecutionInput) -> Result<SignatureSelection> {
        let script = self.script.lock().unwrap();
        Ok(SignatureSelection {
            required: script.required,
            valid: script.valid,
            ignored: 1,
        })
    }

    async fn prepare(
        &self,
        input: ExecutionInput,
    ) -> std::result::Result<Box<dyn ExecutionAttempt>, ExecutionFailure> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.prepared += 1;
            calls.prepared_inputs.push(input);
        }
        let script = self.script.lock().unwrap().clone();
        script.prepare.clone()?;
        Ok(Box::new(FakeAttempt {
            script,
            calls: self.calls.clone(),
            payload: fixture_summary(),
        }))
    }

    async fn chain_tip(&self) -> std::result::Result<u32, String> {
        self.script.lock().unwrap().tip.clone()
    }
}

struct FakeAttempt {
    script: Script,
    calls: Arc<Mutex<Calls>>,
    payload: serde_json::Value,
}

#[async_trait]
impl ExecutionAttempt for FakeAttempt {
    fn reference_block(&self) -> u32 {
        100
    }

    fn summary_payload(&self) -> &serde_json::Value {
        &self.payload
    }

    fn requires_guardian_ack(&self) -> bool {
        true
    }

    async fn execute(
        &mut self,
        ack: Option<GuardianAck>,
    ) -> std::result::Result<ExecutedTransactionInfo, ExecutionFailure> {
        self.calls
            .lock()
            .unwrap()
            .executed_with_ack
            .push(ack.is_some_and(|ack| !ack.signature_hex.is_empty()));
        self.script.execute.clone()?;
        Ok(ExecutedTransactionInfo {
            final_account_commitment: NEW_COMMITMENT.to_string(),
        })
    }

    async fn prove(&mut self) -> std::result::Result<ProvenTransactionInfo, ExecutionFailure> {
        tokio::time::sleep(self.script.proving_time).await;
        self.calls.lock().unwrap().proved += 1;
        self.script.prove.clone()?;
        Ok(ProvenTransactionInfo {
            transaction_id: "0xtx".to_string(),
            reference_block: 100,
            expiration_block: self.script.expiration_block,
        })
    }

    async fn seal(&mut self) -> std::result::Result<(), ExecutionFailure> {
        tokio::time::sleep(self.script.sealing_time).await;
        self.script.seal.clone()
    }

    async fn submit(&mut self) -> SubmissionOutcome {
        self.calls.lock().unwrap().submitted += 1;
        self.script.submission.clone()
    }
}

pub(crate) struct Fixture {
    pub(crate) state: AppState,
    pub(crate) calls: Arc<Mutex<Calls>>,
    pub(crate) script: Arc<Mutex<Script>>,
    signer: Mutex<TestSigner>,
    signer_commitment: String,
}

impl Fixture {
    pub(crate) async fn new(script: Script) -> Self {
        let mut state = create_test_app_state().await;
        let calls = Arc::new(Mutex::new(Calls::default()));
        let script = Arc::new(Mutex::new(script));
        state.execution = ExecutionServices {
            executor: Some(Arc::new(FakeExecutor {
                script: script.clone(),
                calls: calls.clone(),
            })),
            ..ExecutionServices::default()
        };
        let signer = TestSigner::new();
        let fixture = Self {
            state,
            calls,
            script,
            signer_commitment: signer.commitment_hex.clone(),
            signer: Mutex::new(signer),
        };
        fixture.seed().await;
        fixture
    }

    async fn seed(&self) {
        self.state
            .metadata
            .set(crate::metadata::AccountMetadata {
                account_id: ACCOUNT.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![self.signer_commitment.clone()],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2026-09-30T12:00:00Z".to_string(),
                updated_at: "2026-09-30T12:00:00Z".to_string(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            })
            .await
            .unwrap();
        self.state
            .storage
            .submit_state(&StateObject {
                account_id: ACCOUNT.to_string(),
                commitment: BASE.to_string(),
                state_json: serde_json::json!({}),
                created_at: "2026-09-30T12:00:00Z".to_string(),
                updated_at: "2026-09-30T12:00:00Z".to_string(),
                auth_scheme: String::new(),
            })
            .await
            .unwrap();
        self.store_proposal(true).await;
    }

    pub(crate) async fn store_proposal(&self, guardian_executable: bool) {
        let mut payload = serde_json::json!({
            "tx_summary": fixture_summary(),
            "signatures": [],
            "metadata": { "proposal_type": "p2id" },
        });
        if guardian_executable {
            payload["transaction_request"] = serde_json::json!({
                "format_version": 1,
                "protocol_line": "0.17",
                "serializer_id": "0.17.0-rc.4",
                "checksum": "0x00",
                "bytes": "",
            });
        }
        self.state
            .storage
            .submit_delta_proposal(
                PROPOSAL,
                &DeltaObject {
                    account_id: ACCOUNT.to_string(),
                    nonce: 1,
                    prev_commitment: BASE.to_string(),
                    new_commitment: None,
                    delta_payload: payload,
                    ack_sig: String::new(),
                    ack_pubkey: String::new(),
                    ack_scheme: String::new(),
                    status: DeltaStatus::Pending {
                        timestamp: "2026-09-30T12:00:00Z".to_string(),
                        proposer_id: self.signer_commitment.clone(),
                        cosigner_sigs: vec![stored_signature()],
                    },
                    metadata: None,
                },
            )
            .await
            .unwrap();
    }

    /// The cosigner's key and a signature over `payload`, as a transport client sends them.
    pub(crate) fn sign_request(
        &self,
        payload: &guardian_shared::auth_request_payload::AuthRequestPayload,
    ) -> (String, String, i64) {
        let signer = self.signer.lock().unwrap();
        let (signature, timestamp) = signer.sign_request(ACCOUNT, payload);
        (signer.pubkey_hex.clone(), signature, timestamp)
    }

    pub(crate) fn credentials(&self) -> Credentials {
        let signer = self.signer.lock().unwrap();
        let (signature, timestamp) = signer.sign(ACCOUNT);
        Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
    }

    pub(crate) async fn request(&self) -> Result<ExecutionEnvelope> {
        request_execution(
            &self.state,
            RequestExecutionParams {
                account_id: ACCOUNT.to_string(),
                proposal_id: PROPOSAL.to_string(),
                credentials: self.credentials(),
            },
        )
        .await
    }

    pub(crate) async fn read(&self) -> Result<ExecutionEnvelope> {
        get_execution(&self.state, ACCOUNT, PROPOSAL, &self.credentials()).await
    }

    pub(crate) async fn settle(
        &self,
        until: impl Fn(ExecutionState) -> bool,
    ) -> (ExecutionEnvelope, Vec<ExecutionState>) {
        let mut observed = Vec::new();
        for _ in 0..200 {
            let envelope = self.read().await.unwrap();
            if observed.last() != Some(&envelope.state) {
                observed.push(envelope.state);
            }
            if until(envelope.state) {
                return (envelope, observed);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the execution did not settle: {observed:?}");
    }

    pub(crate) async fn promote(&self) -> PromoteWrite {
        let candidate = self
            .state
            .storage
            .pull_delta(ACCOUNT, 1)
            .await
            .expect("the boundary admitted a candidate");
        let now = chrono::Utc::now().to_rfc3339();
        let mut delta = candidate;
        delta.status = DeltaStatus::canonical(now.clone());
        self.state
            .storage
            .promote_candidate(
                self.state.metadata.as_ref(),
                CandidatePromotion {
                    state: StateObject {
                        account_id: ACCOUNT.to_string(),
                        commitment: NEW_COMMITMENT.to_string(),
                        state_json: serde_json::json!({}),
                        created_at: "2026-09-30T12:00:00Z".to_string(),
                        updated_at: now.clone(),
                        auth_scheme: String::new(),
                    },
                    delta,
                    new_auth: None,
                    now,
                    fence: None,
                    source: PromotableKind::Candidate,
                },
            )
            .await
            .unwrap()
    }
}

pub(crate) fn is_submitted_or_terminal(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Submitted | ExecutionState::Committed | ExecutionState::Failed
    )
}

#[tokio::test]
async fn a_threshold_met_proposal_is_executed_submitted_and_committed_by_promotion() {
    let f = Fixture::new(Script::default()).await;
    let accepted = f.request().await.unwrap();
    assert!(accepted.newly_accepted);
    assert_eq!(accepted.state, ExecutionState::Pending);
    assert_eq!(accepted.ignored_signatures, 1);

    let (submitted, observed) = f.settle(is_submitted_or_terminal).await;
    assert_eq!(
        submitted.state,
        ExecutionState::Submitted,
        "{observed:?} {:?}",
        submitted.error
    );
    assert_eq!(submitted.delta_nonce, Some(1));
    for _ in 0..200 {
        if f.calls.lock().unwrap().submitted > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    {
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.prepared, 1);
        assert_eq!(
            calls.executed_with_ack,
            vec![true],
            "executed with Guardian's acknowledgment"
        );
        assert_eq!(calls.submitted, 1);
    }

    let candidate = f.state.storage.pull_delta(ACCOUNT, 1).await.unwrap();
    assert!(candidate.status.is_candidate());
    assert!(!candidate.ack_sig.is_empty());
    assert_eq!(candidate.new_commitment.as_deref(), Some(NEW_COMMITMENT));

    assert_eq!(f.promote().await, PromoteWrite::Applied);
    let committed = f.read().await.unwrap();
    assert_eq!(committed.state, ExecutionState::Committed);
    assert!(
        f.state
            .storage
            .pull_delta(ACCOUNT, 1)
            .await
            .unwrap()
            .status
            .is_canonical(),
        "the delta moved through the ordinary candidate to canonical lifecycle"
    );
    assert!(
        current_execution(&f.state, ACCOUNT, &f.credentials())
            .await
            .unwrap()
            .is_none(),
        "a terminal execution is not in flight"
    );
}

#[tokio::test]
async fn reported_states_only_move_forward_through_the_vocabulary() {
    let f = Fixture::new(Script::default()).await;
    f.request().await.unwrap();
    let (_, observed) = f.settle(is_submitted_or_terminal).await;
    let order = [
        ExecutionState::Pending,
        ExecutionState::Proving,
        ExecutionState::Submitted,
    ];
    let mut position = 0;
    for state in observed {
        let index = order
            .iter()
            .position(|expected| *expected == state)
            .unwrap_or_else(|| panic!("unexpected state {state:?}"));
        assert!(index >= position, "states moved backwards");
        position = index;
    }
}

#[tokio::test]
async fn a_repeated_request_returns_the_active_execution() {
    let f = Fixture::new(Script::default()).await;
    f.request().await.unwrap();
    let again = f.request().await.unwrap();
    assert!(!again.newly_accepted);
}

#[tokio::test]
async fn a_proposal_below_threshold_is_refused_and_reserves_nothing() {
    let f = Fixture::new(Script {
        valid: 1,
        ..Script::default()
    })
    .await;
    match f.request().await {
        Err(GuardianError::ProposalNotReady { required, valid }) => {
            assert_eq!((required, valid), (2, 1));
        }
        other => panic!("expected not ready, got {other:?}"),
    }
    assert!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        f.read().await,
        Err(GuardianError::ExecutionNotFound { .. })
    ));
}

#[tokio::test]
async fn a_non_cosigner_is_refused_on_authentication() {
    let f = Fixture::new(Script::default()).await;
    let (pubkey, _, signature, timestamp) =
        generate_falcon_signature_with_timestamp(ACCOUNT, chrono::Utc::now().timestamp_millis());
    let result = request_execution(
        &f.state,
        RequestExecutionParams {
            account_id: ACCOUNT.to_string(),
            proposal_id: PROPOSAL.to_string(),
            credentials: Credentials::signature(pubkey, signature, timestamp),
        },
    )
    .await;
    assert!(
        matches!(result, Err(GuardianError::AuthenticationFailed(_))),
        "{result:?}"
    );
    assert!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_proposal_without_a_stored_request_is_not_guardian_executable() {
    let f = Fixture::new(Script::default()).await;
    f.store_proposal(false).await;
    assert!(matches!(
        f.request().await,
        Err(GuardianError::ProposalMissingTransactionRequest)
    ));
}

#[tokio::test]
async fn a_server_without_an_executor_refuses_execution() {
    let f = Fixture::new(Script::default()).await;
    let mut state = f.state.clone();
    state.execution = ExecutionServices::default();
    let result = request_execution(
        &state,
        RequestExecutionParams {
            account_id: ACCOUNT.to_string(),
            proposal_id: PROPOSAL.to_string(),
            credentials: f.credentials(),
        },
    )
    .await;
    assert!(matches!(result, Err(GuardianError::ProvingUnavailable)));
}

#[tokio::test]
async fn a_never_executed_proposal_reads_as_execution_not_found_and_nothing_in_flight() {
    let f = Fixture::new(Script::default()).await;
    assert!(matches!(
        f.read().await,
        Err(GuardianError::ExecutionNotFound { .. })
    ));
    assert!(
        current_execution(&f.state, ACCOUNT, &f.credentials())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_pre_boundary_failure_releases_the_account_and_permits_a_retry() {
    let f = Fixture::new(Script {
        prepare: Err(ExecutionFailure {
            code: ExecutionFailureCode::ChainBehind,
            message: "node behind".to_string(),
        }),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    assert_eq!(
        failed.error.as_ref().map(|error| error.code.as_str()),
        Some("GUARDIAN_EXECUTION_CHAIN_BEHIND")
    );
    assert!(failed.proposal_exists);
    assert!(
        f.state.storage.pull_delta(ACCOUNT, 1).await.is_err(),
        "no delta recorded"
    );
    assert!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_none(),
        "the account is unlocked"
    );
    assert!(
        f.request().await.unwrap().newly_accepted,
        "a failed proposal can be retried"
    );
}

#[tokio::test]
async fn a_definitely_rejected_submission_discards_the_candidate_and_the_proposal() {
    let f = Fixture::new(Script {
        submission: SubmissionOutcome::Rejected {
            reason: "stale".to_string(),
        },
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    assert_eq!(
        failed.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_SUBMISSION_REJECTED".to_string())
    );
    assert!(!failed.proposal_exists);
    assert!(f.state.storage.pull_delta(ACCOUNT, 1).await.is_err());
}

#[tokio::test]
async fn an_unknown_submission_outcome_stays_submitted_and_holds_the_account() {
    let f = Fixture::new(Script {
        submission: SubmissionOutcome::Unknown {
            reason: "timeout".to_string(),
        },
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    let (submitted, _) = f.settle(is_submitted_or_terminal).await;
    assert_eq!(submitted.state, ExecutionState::Submitted);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.read().await.unwrap().state, ExecutionState::Submitted);
    assert!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_some()
    );
}
