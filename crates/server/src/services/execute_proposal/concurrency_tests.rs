use crate::error::GuardianError;
use crate::services::execution_status::ExecutionState;

use super::tests::{ACCOUNT, Fixture, PROPOSAL, Script, is_submitted_or_terminal};
use super::{RequestExecutionParams, request_execution};

fn joined_one_execution(
    results: [crate::error::Result<crate::services::execution_status::ExecutionEnvelope>; 2],
) {
    let accepted = results
        .iter()
        .filter(|result| matches!(result, Ok(envelope) if envelope.newly_accepted))
        .count();
    assert_eq!(accepted, 1, "{results:?}");
    for result in &results {
        match result {
            Ok(envelope) => assert_eq!(envelope.proposal_id, PROPOSAL),
            Err(GuardianError::ExecutionConflict {
                blocking_proposal_id,
            }) => assert_eq!(blocking_proposal_id, PROPOSAL),
            Err(GuardianError::ExecutionBusy) => {}
            Err(other) => panic!("unexpected refusal {other:?}"),
        }
    }
}

#[tokio::test]
async fn two_concurrent_requests_for_one_proposal_start_one_execution() {
    let f = Fixture::new(Script::default()).await;
    let (first, second) = tokio::join!(f.request(), f.request());
    joined_one_execution([first, second]);
    let (settled, _) = f.settle(is_submitted_or_terminal).await;
    assert_eq!(settled.state, ExecutionState::Submitted);
    let calls = f.calls.lock().unwrap();
    assert_eq!((calls.prepared, calls.proved), (1, 1));
}

#[tokio::test]
async fn two_replicas_sharing_storage_prove_and_submit_once() {
    let f = Fixture::new(Script::default()).await;
    let mut other_replica = f.state.clone();
    other_replica.execution.replica_id = "replica-b".to_string();
    let params = || RequestExecutionParams {
        account_id: ACCOUNT.to_string(),
        proposal_id: PROPOSAL.to_string(),
        credentials: f.credentials(),
        allow_private_note: false,
    };
    let (first, second) = tokio::join!(
        request_execution(&f.state, params()),
        request_execution(&other_replica, params()),
    );
    joined_one_execution([first, second]);
    f.settle(is_submitted_or_terminal).await;
    for _ in 0..200 {
        if f.calls.lock().unwrap().submitted > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let calls = f.calls.lock().unwrap();
    assert_eq!((calls.prepared, calls.proved, calls.submitted), (1, 1, 1));
}

#[tokio::test]
async fn guardian_admits_its_own_candidate_under_its_own_reservation() {
    let f = Fixture::new(Script::default()).await;
    f.request().await.unwrap();
    let (settled, observed) = f.settle(is_submitted_or_terminal).await;
    assert_eq!(settled.state, ExecutionState::Submitted, "{observed:?}");
    for _ in 0..200 {
        if f.calls.lock().unwrap().submitted > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        f.state
            .storage
            .pull_delta(ACCOUNT, 1)
            .await
            .unwrap()
            .status
            .is_candidate(),
        "the reservation did not block Guardian's own boundary commit"
    );
}

async fn client_push(
    f: &Fixture,
) -> crate::error::Result<crate::services::push_delta::PushDeltaResult> {
    let delta: crate::delta_object::DeltaObject =
        serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON).unwrap();
    crate::services::push_delta::push_delta(
        &f.state,
        crate::services::push_delta::PushDeltaParams {
            delta: crate::delta_object::DeltaObject {
                account_id: ACCOUNT.to_string(),
                nonce: 1,
                prev_commitment: super::tests::BASE.to_string(),
                ..delta
            },
            credentials: f.credentials(),
        },
    )
    .await
}

#[tokio::test]
async fn a_client_push_is_refused_while_an_execution_holds_the_account() {
    let f = Fixture::new(Script {
        proving_time: std::time::Duration::from_millis(500),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    match client_push(&f).await {
        Err(GuardianError::ExecutionConflict {
            blocking_proposal_id,
        }) => assert_eq!(blocking_proposal_id, PROPOSAL),
        other => panic!("before the boundary: expected an execution conflict, got {other:?}"),
    }
    f.settle(is_submitted_or_terminal).await;
    for _ in 0..200 {
        if f.calls.lock().unwrap().submitted > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    match client_push(&f).await {
        Err(GuardianError::ExecutionConflict {
            blocking_proposal_id,
        }) => assert_eq!(blocking_proposal_id, PROPOSAL),
        other => panic!("after the boundary: expected an execution conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn a_shutdown_fails_an_attempt_short_of_the_boundary_and_releases_its_account() {
    let f = Fixture::new(Script {
        proving_time: std::time::Duration::from_secs(30),
        ..Script::default()
    })
    .await;
    f.request().await.unwrap();
    f.settle(|state| state == ExecutionState::Proving).await;

    assert!(
        f.state
            .execution
            .drain(std::time::Duration::from_secs(5))
            .await,
        "the worker did not finish within the grace"
    );

    let (failed, _) = f.settle(|state| state == ExecutionState::Failed).await;
    assert_eq!(
        failed.error.as_ref().map(|error| error.code.as_str()),
        Some("GUARDIAN_EXECUTION_ABANDONED")
    );
    assert!(failed.proposal_exists);
    assert!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_none(),
        "the account is still reserved"
    );
    assert!(
        f.state.storage.pull_delta(ACCOUNT, 1).await.is_err(),
        "no candidate was admitted"
    );
    assert_eq!(f.calls.lock().unwrap().submitted, 0);
}

#[tokio::test]
async fn a_request_during_shutdown_is_refused_as_busy_and_reserves_nothing() {
    let f = Fixture::new(Script::default()).await;
    assert!(
        f.state
            .execution
            .drain(std::time::Duration::from_secs(1))
            .await
    );
    assert!(matches!(
        f.request().await,
        Err(GuardianError::ExecutionBusy)
    ));
    assert!(
        f.state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.calls.lock().unwrap().prepared, 0);
    assert!(
        f.state
            .execution
            .capacity
            .clone()
            .try_acquire_owned()
            .is_err(),
        "a drained process still hands out execution permits"
    );
}
