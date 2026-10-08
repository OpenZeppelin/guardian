use crate::error::GuardianError;
use crate::services::execution_status::ExecutionState;

use super::tests::{ACCOUNT, Fixture, PROPOSAL, Script, is_submitted_or_terminal};
use super::{ExecutionState as ExecutionServices, RequestExecutionParams, request_execution};

async fn request_on(
    f: &Fixture,
    state: &crate::state::AppState,
) -> crate::error::Result<crate::services::execution_status::ExecutionEnvelope> {
    request_execution(
        state,
        RequestExecutionParams {
            account_id: ACCOUNT.to_string(),
            proposal_id: PROPOSAL.to_string(),
            credentials: f.credentials(),
            allow_private_note: false,
        },
    )
    .await
}

async fn assert_refused_every_time_and_nothing_reserved(
    f: &Fixture,
    state: &crate::state::AppState,
) {
    for _ in 0..25 {
        assert!(matches!(
            request_on(f, state).await,
            Err(GuardianError::ProvingUnavailable)
        ));
    }
    assert_eq!(
        f.calls.lock().unwrap().prepared,
        0,
        "nothing was reproduced or proven"
    );
    assert!(
        state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_none(),
        "no reservation was created"
    );
}

#[tokio::test]
async fn a_server_that_offers_no_execution_refuses_every_request_the_same_way() {
    let f = Fixture::new(Script::default()).await;
    let mut unavailable = f.state.clone();
    unavailable.execution = ExecutionServices::default();
    assert_refused_every_time_and_nothing_reserved(&f, &unavailable).await;
}

#[tokio::test]
async fn optimistic_mode_offers_no_execution_even_with_an_executor() {
    let f = Fixture::new(Script::default()).await;
    let mut optimistic = f.state.clone();
    optimistic.canonicalization = None;
    assert_refused_every_time_and_nothing_reserved(&f, &optimistic).await;
}

#[tokio::test]
async fn the_same_server_executes_once_the_capability_is_present() {
    let f = Fixture::new(Script::default()).await;
    assert!(f.request().await.unwrap().newly_accepted);
    let (settled, _) = f.settle(is_submitted_or_terminal).await;
    assert_eq!(settled.state, ExecutionState::Submitted);
}
