use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::dashboard::{AuthenticatedOperator, extract_cookie};
use crate::error::Result;
use crate::services::{
    DashboardAccountDetail, DashboardAccountSummary, get_dashboard_account, list_dashboard_accounts,
};
use crate::state::AppState;

#[derive(Debug, Deserialize, Serialize)]
pub struct ChallengeQuery {
    pub commitment: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerifyOperatorRequest {
    pub commitment: String,
    pub signature: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OperatorChallengeResponse {
    pub success: bool,
    pub challenge: OperatorChallengeView,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OperatorChallengeView {
    pub domain: String,
    pub commitment: String,
    pub nonce: String,
    pub expires_at: String,
    pub signing_digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerifyOperatorResponse {
    pub success: bool,
    pub operator_id: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogoutOperatorResponse {
    pub success: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardAccountsResponse {
    pub success: bool,
    pub total_count: usize,
    pub accounts: Vec<DashboardAccountSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardAccountResponse {
    pub success: bool,
    pub account: DashboardAccountDetail,
}

pub async fn challenge_operator_login(
    State(state): State<AppState>,
    Query(query): Query<ChallengeQuery>,
) -> Result<Json<OperatorChallengeResponse>> {
    let challenge = state
        .dashboard
        .issue_challenge(&query.commitment, state.clock.now())
        .await?;

    Ok(Json(OperatorChallengeResponse {
        success: true,
        challenge: OperatorChallengeView {
            domain: challenge.payload.domain,
            commitment: challenge.payload.commitment,
            nonce: challenge.payload.nonce,
            expires_at: challenge.payload.expires_at,
            signing_digest: challenge.signing_digest,
        },
    }))
}

pub async fn verify_operator_login(
    State(state): State<AppState>,
    Json(payload): Json<VerifyOperatorRequest>,
) -> Result<impl IntoResponse> {
    let session = state
        .dashboard
        .verify(&payload.commitment, &payload.signature, state.clock.now())
        .await?;

    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session.cookie_header)],
        Json(VerifyOperatorResponse {
            success: true,
            operator_id: session.operator.operator_id,
            expires_at: session.expires_at,
        }),
    ))
}

pub async fn logout_operator(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let token = extract_cookie(&headers, state.dashboard.cookie_name());
    state
        .dashboard
        .logout(token.as_deref(), state.clock.now())
        .await;

    (
        StatusCode::OK,
        [(header::SET_COOKIE, state.dashboard.clear_cookie_header())],
        Json(LogoutOperatorResponse { success: true }),
    )
}

pub async fn list_operator_accounts(
    State(state): State<AppState>,
    Extension(_operator): Extension<AuthenticatedOperator>,
) -> Result<Json<DashboardAccountsResponse>> {
    let response = list_dashboard_accounts(&state).await?;

    Ok(Json(DashboardAccountsResponse {
        success: true,
        total_count: response.total_count,
        accounts: response.accounts,
    }))
}

pub async fn get_operator_account(
    State(state): State<AppState>,
    Extension(_operator): Extension<AuthenticatedOperator>,
    Path(account_id): Path<String>,
) -> Result<Json<DashboardAccountResponse>> {
    let response = get_dashboard_account(&state, &account_id).await?;

    Ok(Json(DashboardAccountResponse {
        success: true,
        account: response.account,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use guardian_shared::FromJson;
    use guardian_shared::hex::FromHex;
    use miden_protocol::Word;
    use miden_protocol::account::Account;
    use serde::de::DeserializeOwned;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::dashboard::DashboardState;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state::AppState;
    use crate::state_object::StateObject;
    use crate::testing::helpers::{
        TestSigner, create_router, create_test_app_state, load_fixture_account,
    };

    use super::*;

    #[tokio::test]
    async fn operator_can_login_list_accounts_and_fetch_detail() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".to_string(),
            operator.commitment_hex.clone(),
        )]));

        let (_account_id, account_id_hex, account_json) = load_fixture_account();
        seed_account(
            &state,
            create_metadata(&account_id_hex, "2024-01-02T00:00:00Z"),
            Some(create_state_object(
                &account_id_hex,
                account_json.clone(),
                "2024-01-02T00:00:00Z",
            )),
        )
        .await;
        seed_account(
            &state,
            create_metadata("account-without-state", "2024-01-01T00:00:00Z"),
            None,
        )
        .await;

        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: DashboardAccountsResponse = read_json(list_response).await;
        assert_eq!(list_body.total_count, 2);
        assert_eq!(list_body.accounts[0].account_id, account_id_hex);
        assert_eq!(
            list_body.accounts[1].state_status,
            crate::services::DashboardAccountStateStatus::Unavailable
        );
        assert_eq!(list_body.accounts[1].current_commitment, None);

        let detail_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/dashboard/accounts/{account_id_hex}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::OK);
        let detail_body: DashboardAccountResponse = read_json(detail_response).await;
        assert_eq!(detail_body.account.account_id, account_id_hex);
        assert_eq!(
            detail_body.account.current_commitment,
            list_body.accounts[0].current_commitment
        );
        assert_eq!(detail_body.account.auth_scheme, "falcon");
        assert_eq!(detail_body.account.authorized_signer_count, 1);
    }

    #[tokio::test]
    async fn operator_verify_replay_is_rejected() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".to_string(),
            operator.commitment_hex.clone(),
        )]));

        let app = create_router(state);

        let challenge = fetch_challenge(&app, &operator).await;
        let signature =
            operator.sign_word(Word::from_hex(&challenge.challenge.signing_digest).unwrap());
        let verify_body = serde_json::to_vec(&json!({
            "commitment": operator.commitment_hex.clone(),
            "signature": signature,
        }))
        .unwrap();

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(verify_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);

        let replay_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(verify_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn operator_verify_rejects_commitment_mismatch_without_consuming_challenge() {
        let operator = TestSigner::new();
        let attacker = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![
            ("operator-1".to_string(), operator.commitment_hex.clone()),
            ("operator-2".to_string(), attacker.commitment_hex.clone()),
        ]));

        let app = create_router(state);

        let challenge = fetch_challenge(&app, &operator).await;
        let signing_digest = Word::from_hex(&challenge.challenge.signing_digest)
            .expect("challenge signing digest should be a valid word");

        let mismatch_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "commitment": operator.commitment_hex.clone(),
                            "signature": attacker.sign_word(signing_digest),
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatch_response.status(), StatusCode::UNAUTHORIZED);

        let success_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "commitment": operator.commitment_hex.clone(),
                            "signature": operator.sign_word(signing_digest),
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(success_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn operator_logout_invalidates_dashboard_session() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".to_string(),
            operator.commitment_hex.clone(),
        )]));

        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let logout_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/logout")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout_response.status(), StatusCode::OK);

        let rejected_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn dashboard_detail_returns_explicit_unavailable_error_when_state_is_missing() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".to_string(),
            operator.commitment_hex.clone(),
        )]));
        seed_account(
            &state,
            create_metadata("missing-state-account", "2024-01-01T00:00:00Z"),
            None,
        )
        .await;

        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let detail_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts/missing-state-account")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    async fn authenticate_operator(app: &axum::Router, operator: &TestSigner) -> String {
        let challenge = fetch_challenge(app, operator).await;
        let signing_digest = Word::from_hex(&challenge.challenge.signing_digest)
            .expect("challenge signing digest should be a valid word");
        let signature = operator.sign_word(signing_digest);
        let verify_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "commitment": operator.commitment_hex.clone(),
                            "signature": signature,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(verify_response.status(), StatusCode::OK);

        verify_response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_string)
            .expect("verify response should set a session cookie")
    }

    async fn fetch_challenge(
        app: &axum::Router,
        operator: &TestSigner,
    ) -> OperatorChallengeResponse {
        let challenge_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/auth/challenge?commitment={}",
                        operator.commitment_hex
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(challenge_response.status(), StatusCode::OK);
        read_json(challenge_response).await
    }

    async fn read_json<T: DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        serde_json::from_slice(&bytes).expect("response body should be valid json")
    }

    fn create_metadata(account_id: &str, updated_at: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xfeedbeef".to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    fn create_state_object(
        account_id: &str,
        state_json: serde_json::Value,
        updated_at: &str,
    ) -> StateObject {
        let account = Account::from_json(&state_json).expect("fixture account should deserialize");
        StateObject {
            account_id: account_id.to_string(),
            state_json,
            commitment: format!("0x{}", hex::encode(account.to_commitment().as_bytes())),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            auth_scheme: "falcon".to_string(),
        }
    }

    async fn seed_account(
        state: &AppState,
        metadata: AccountMetadata,
        state_object: Option<StateObject>,
    ) {
        state
            .metadata
            .set(metadata)
            .await
            .expect("metadata should be written");
        if let Some(state_object) = state_object {
            state
                .storage
                .submit_state(&state_object)
                .await
                .expect("state should be written");
        }
    }
}
