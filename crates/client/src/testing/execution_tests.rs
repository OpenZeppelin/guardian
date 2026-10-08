use tonic::Status;

use crate::execution::{
    ExecutionFailureCode, ExecutionState, ExpirationBound, ProposalExecution, RequestInvalidReason,
};
use crate::testing::mocks::{MockGuardianService, start_mock_server};
use crate::{ClientError, ExecutionEnvelope, ExecutionError, FalconKeyStore, GuardianClient};
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use std::sync::Arc;

const PROPOSAL: &str = "0x2222";

fn account() -> AccountId {
    AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap()
}

fn envelope(state: &str, error: Option<ExecutionError>) -> ExecutionEnvelope {
    ExecutionEnvelope {
        account_id: account().to_string(),
        proposal_id: PROPOSAL.to_string(),
        state: state.to_string(),
        error,
        delta_nonce: None,
        newly_accepted: false,
        proposal_exists: true,
        ignored_signatures: 0,
        updated_at: "2026-09-30T12:00:00Z".to_string(),
    }
}

async fn client(service: MockGuardianService) -> GuardianClient {
    let endpoint = start_mock_server(service).await.unwrap();
    GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(Arc::new(FalconKeyStore::new(SecretKey::new())))
}

#[tokio::test]
async fn an_accepted_request_reports_the_new_execution() {
    let mut accepted = envelope("pending", None);
    accepted.newly_accepted = true;
    accepted.ignored_signatures = 1;
    let mut client = client(MockGuardianService::default().with_execution(Ok(accepted))).await;
    let execution = client
        .execute_delta_proposal(&account(), PROPOSAL)
        .await
        .unwrap();
    assert_eq!(execution.state, ExecutionState::Pending);
    assert!(execution.newly_accepted);
    assert_eq!(execution.ignored_signatures, 1);
    assert!(execution.error.is_none());
}

#[tokio::test]
async fn a_failure_decodes_to_its_typed_code_and_meta() {
    let failures = [
        (
            "GUARDIAN_EXECUTION_REQUEST_INVALID",
            Some(r#"{"reason":"input_notes_not_pinned"}"#),
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::InputNotesNotPinned),
        ),
        (
            "GUARDIAN_EXECUTION_EXPIRATION_REACHED",
            Some(r#"{"bound":"transaction"}"#),
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Transaction),
        ),
        (
            "GUARDIAN_EXECUTION_EXPIRED",
            None,
            ExecutionFailureCode::Expired,
        ),
    ];
    for (code, meta, expected) in failures {
        let mut failed = envelope(
            "failed",
            Some(ExecutionError {
                code: code.to_string(),
                message: "scripted".to_string(),
                meta_json: meta.map(str::to_string),
            }),
        );
        failed.proposal_exists = false;
        let mut client = client(MockGuardianService::default().with_execution(Ok(failed))).await;
        let execution = client
            .get_delta_proposal_execution(&account(), PROPOSAL)
            .await
            .unwrap();
        assert_eq!(execution.state, ExecutionState::Failed);
        assert_eq!(execution.error.unwrap().code, expected);
        assert!(!execution.proposal_exists);
    }
}

#[tokio::test]
async fn nothing_in_flight_is_none_not_an_error() {
    let mut client = client(MockGuardianService::default().with_current_execution(Ok(None))).await;
    assert_eq!(
        client.get_current_execution(&account()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn an_unknown_state_or_a_code_without_its_meta_is_an_invalid_response() {
    let malformed = [
        envelope("queued", None),
        envelope(
            "failed",
            Some(ExecutionError {
                code: "GUARDIAN_EXECUTION_REQUEST_INVALID".to_string(),
                message: "scripted".to_string(),
                meta_json: None,
            }),
        ),
        envelope("failed", None),
    ];
    for envelope in malformed {
        let result = ProposalExecution::try_from(envelope);
        assert!(
            matches!(result, Err(ClientError::InvalidResponse(_))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn a_refusal_arrives_as_a_status_with_its_code() {
    let details = serde_json::json!({
        "code": "GUARDIAN_PROPOSAL_NOT_READY",
        "message": "This transaction still needs more signatures.",
        "meta": { "retryable": false }
    });
    let status = Status::with_details(
        tonic::Code::FailedPrecondition,
        "not ready",
        serde_json::to_vec(&details).unwrap().into(),
    );
    let mut client = client(MockGuardianService::default().with_execution(Err(status))).await;
    let error = client
        .execute_delta_proposal(&account(), PROPOSAL)
        .await
        .unwrap_err();
    assert_eq!(
        error.guardian_code().as_deref(),
        Some("GUARDIAN_PROPOSAL_NOT_READY")
    );
}

#[tokio::test]
async fn a_switch_guardian_refusal_names_the_proposal_type() {
    let details = serde_json::json!({
        "code": guardian_shared::execution::refusal_codes::PROPOSAL_EXECUTES_LOCALLY,
        "message": "A guardian switch is executed by the wallet that finishes the handoff. Execute it from your wallet instead.",
        "meta": { "retryable": false, "proposal_type": "switch_guardian" }
    });
    let status = Status::with_details(
        tonic::Code::FailedPrecondition,
        "executes locally",
        serde_json::to_vec(&details).unwrap().into(),
    );
    let mut client = client(MockGuardianService::default().with_execution(Err(status))).await;
    let error = client
        .execute_delta_proposal(&account(), PROPOSAL)
        .await
        .unwrap_err();
    assert_eq!(
        error.guardian_code().as_deref(),
        Some("GUARDIAN_PROPOSAL_EXECUTES_LOCALLY")
    );
    assert_eq!(
        error.guardian_meta().unwrap()["proposal_type"],
        "switch_guardian"
    );
    assert!(!error.is_retryable());
}
