use crate::error::GuardianError;
use crate::services::execution_status::ExecutionState;
use crate::state_object::StateObject;
use crate::storage::execution::{
    ExpirationBound, ForeignAccountUnavailableReason, RequestInvalidReason,
};
use crate::storage::{ExecutionFailure, ExecutionFailureCode};

use super::SubmissionOutcome;
use super::tests::{ACCOUNT, Fixture, PROPOSAL, Script, stored_signature};

const OTHER_PROPOSAL: &str = "0x3333333333333333333333333333333333333333333333333333333333333333";

fn failing(code: ExecutionFailureCode) -> Result<(), ExecutionFailure> {
    Err(ExecutionFailure {
        code,
        message: "scripted failure".to_string(),
    })
}

fn is_settled(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Submitted | ExecutionState::Committed | ExecutionState::Failed
    )
}

impl Fixture {
    async fn assert_left_no_trace(&self) {
        assert!(
            self.state.storage.pull_delta(ACCOUNT, 1).await.is_err(),
            "no delta was recorded"
        );
        assert!(
            self.state
                .storage
                .load_active_execution(ACCOUNT)
                .await
                .unwrap()
                .is_none(),
            "no reservation is held"
        );
        assert!(
            !self
                .state
                .metadata
                .get(ACCOUNT)
                .await
                .unwrap()
                .unwrap()
                .has_pending_candidate,
            "the account is not locked"
        );
        assert!(
            self.state
                .storage
                .pull_delta_proposal(ACCOUNT, PROPOSAL)
                .await
                .is_ok(),
            "the proposal survives"
        );
    }

    async fn assert_retry_submits(&self) {
        *self.script.lock().unwrap() = Script::default();
        assert!(self.request().await.unwrap().newly_accepted);
        let (settled, observed) = self.settle(is_settled).await;
        assert_eq!(settled.state, ExecutionState::Submitted, "{observed:?}");
    }
}

#[tokio::test]
async fn an_account_advanced_past_the_base_is_a_state_mismatch_before_any_chain_work() {
    let f = Fixture::new(Script::default()).await;
    f.state
        .storage
        .submit_state(&StateObject {
            account_id: ACCOUNT.to_string(),
            commitment: "0x4444444444444444444444444444444444444444444444444444444444444444"
                .to_string(),
            nonce: None,
            state_json: serde_json::json!({}),
            created_at: "2026-09-30T12:00:00Z".to_string(),
            updated_at: "2026-09-30T12:05:00Z".to_string(),
            auth_scheme: String::new(),
        })
        .await
        .unwrap();
    f.request().await.unwrap();
    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    assert_eq!(
        failed.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_STATE_MISMATCH".to_string())
    );
    assert_eq!(
        f.calls.lock().unwrap().prepared,
        0,
        "nothing was reproduced"
    );
    assert_eq!(f.calls.lock().unwrap().proved, 0);
}

#[tokio::test]
async fn every_pre_boundary_failure_is_reported_leaves_no_trace_and_can_be_retried() {
    let cases = [
        Script {
            prepare: failing(ExecutionFailureCode::BindingMismatch),
            ..Script::default()
        },
        Script {
            prepare: failing(ExecutionFailureCode::RequestInvalid(
                RequestInvalidReason::BoundBlockNotDeclared,
            )),
            ..Script::default()
        },
        Script {
            prepare: failing(ExecutionFailureCode::ExpirationReached(
                ExpirationBound::Approval,
            )),
            ..Script::default()
        },
        Script {
            execute: failing(ExecutionFailureCode::ForeignAccountUnavailable(
                ForeignAccountUnavailableReason::Private,
            )),
            ..Script::default()
        },
        Script {
            execute: failing(ExecutionFailureCode::InsufficientFee),
            ..Script::default()
        },
        Script {
            prove: failing(ExecutionFailureCode::ExpirationReached(
                ExpirationBound::Transaction,
            )),
            ..Script::default()
        },
        Script {
            prove: failing(ExecutionFailureCode::ProvingFailed),
            ..Script::default()
        },
        Script {
            seal: failing(ExecutionFailureCode::SealingFailed),
            ..Script::default()
        },
    ];
    for script in cases {
        let expected = [
            &script.prepare,
            &script.execute,
            &script.prove,
            &script.seal,
        ]
        .into_iter()
        .find_map(|step| step.clone().err())
        .unwrap()
        .code;
        let f = Fixture::new(script).await;
        f.request().await.unwrap();
        let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
        let error = failed.error.expect("a failed execution carries its cause");
        assert_eq!(error.code, expected.as_str());
        assert_eq!(error.meta, expected.meta(), "{}", expected.as_str());
        assert!(failed.proposal_exists);
        assert_eq!(f.calls.lock().unwrap().submitted, 0);
        f.assert_left_no_trace().await;
        f.assert_retry_submits().await;
    }
}

#[tokio::test]
async fn a_node_that_catches_up_serves_a_later_request() {
    let f = Fixture::new(Script {
        prepare: failing(ExecutionFailureCode::ChainBehind),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    assert_eq!(
        failed.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_CHAIN_BEHIND".to_string())
    );
    f.assert_left_no_trace().await;
    f.assert_retry_submits().await;
}

#[tokio::test]
async fn the_executor_sees_exactly_the_signatures_guardian_holds() {
    let f = Fixture::new(Script::default()).await;
    f.request().await.unwrap();
    f.settle(is_settled).await;
    let calls = f.calls.lock().unwrap();
    assert_eq!(calls.prepared_inputs.len(), 1);
    assert_eq!(
        calls.prepared_inputs[0].cosigner_signatures,
        vec![stored_signature()]
    );
}

#[tokio::test]
async fn a_second_proposal_is_refused_while_another_holds_the_account() {
    let f = Fixture::new(Script {
        submission: SubmissionOutcome::Unknown {
            reason: "timeout".to_string(),
        },
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    f.settle(is_settled).await;
    let proposal = f
        .state
        .storage
        .pull_delta_proposal(ACCOUNT, PROPOSAL)
        .await
        .unwrap();
    f.state
        .storage
        .submit_delta_proposal(OTHER_PROPOSAL, &proposal)
        .await
        .unwrap();

    let result = super::request_execution(
        &f.state,
        super::RequestExecutionParams {
            account_id: ACCOUNT.to_string(),
            proposal_id: OTHER_PROPOSAL.to_string(),
            credentials: f.credentials(),
        },
    )
    .await;
    match result {
        Err(GuardianError::ExecutionConflict {
            blocking_proposal_id,
        }) => assert_eq!(blocking_proposal_id, PROPOSAL),
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .unwrap()
            .reservation
            .proposal_id,
        PROPOSAL
    );
}

#[tokio::test]
async fn a_paused_account_is_refused_and_nothing_is_reserved() {
    let f = Fixture::new(Script::default()).await;
    f.state
        .metadata
        .set_pause(ACCOUNT, chrono::Utc::now(), "incident")
        .await
        .unwrap();
    assert!(matches!(
        f.request().await,
        Err(GuardianError::AccountPaused { .. })
    ));
    assert_eq!(f.calls.lock().unwrap().prepared, 0);
    f.assert_left_no_trace().await;
}

#[tokio::test]
async fn a_transaction_that_expires_while_it_is_proved_is_refused_before_the_boundary() {
    let f = Fixture::new(Script {
        tip: Ok(356),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    let error = failed.error.expect("a failed execution carries its cause");
    assert_eq!(error.code, "GUARDIAN_EXECUTION_EXPIRATION_REACHED");
    assert_eq!(
        error.meta,
        Some(serde_json::json!({ "bound": "transaction" }))
    );
    assert!(failed.proposal_exists, "the proposal can still be retried");
    assert_eq!(f.calls.lock().unwrap().submitted, 0);
    f.assert_left_no_trace().await;
}

#[tokio::test]
async fn an_unreadable_tip_before_the_boundary_is_a_node_failure_and_leaves_no_trace() {
    let f = Fixture::new(Script {
        tip: Err("connection refused".to_string()),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    assert_eq!(
        failed.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_NODE_UNAVAILABLE".to_string())
    );
    assert_eq!(f.calls.lock().unwrap().submitted, 0);
    f.assert_left_no_trace().await;
}
