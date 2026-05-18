use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::dashboard::cursor::CursorKind;
use crate::dashboard::{AuthenticatedOperator, extract_cookie};
use crate::error::Result;
use crate::services::{
    DashboardAccountDetail, DashboardAccountSnapshot, DashboardAccountSummary,
    DashboardInfoResponse, PagedResult, get_account_snapshot, get_dashboard_account,
    get_dashboard_info, list_dashboard_accounts_paged, parse_cursor, parse_limit,
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

/// **Deprecated** as of feature `005-operator-dashboard-metrics`. The
/// account list endpoint now returns the [`PagedResult`] envelope and
/// no longer carries `total_count`. Aggregate inventory totals are
/// available via `GET /dashboard/info`. Kept here only to support
/// pre-existing test fixtures during the migration; new callers MUST
/// use [`PagedResult<DashboardAccountSummary>`].
#[derive(Debug, Serialize, Deserialize)]
#[deprecated(note = "use PagedResult<DashboardAccountSummary> per FR-001")]
#[allow(deprecated)]
pub struct DashboardAccountsResponse {
    pub success: bool,
    pub total_count: usize,
    pub accounts: Vec<DashboardAccountSummary>,
}

/// `?limit=&cursor=` query parameters for the paginated account list.
#[derive(Debug, Deserialize)]
pub struct AccountsQuery {
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
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
    Query(query): Query<AccountsQuery>,
) -> Result<Json<PagedResult<DashboardAccountSummary>>> {
    let limit = parse_limit(query.limit.as_deref())?;
    let cursor = parse_cursor(
        query.cursor.as_deref(),
        state.dashboard.cursor_secret(),
        CursorKind::AccountList,
    )?;
    let result = list_dashboard_accounts_paged(&state, limit, cursor).await?;
    Ok(Json(result))
}

/// `GET /dashboard/info` — point-in-time inventory and lifecycle
/// summary per feature `005-operator-dashboard-metrics` US2.
pub async fn get_dashboard_info_handler(
    State(state): State<AppState>,
    Extension(_operator): Extension<AuthenticatedOperator>,
) -> Result<Json<DashboardInfoResponse>> {
    let info = get_dashboard_info(&state).await?;
    Ok(Json(info))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionInfoResponse {
    pub operator_id: String,
    pub permissions: Vec<String>,
}

// `GET /dashboard/session` — session introspection (US6 / FR-033..FR-036).
// Bypasses the authz layer so `permissions: []` operators get 200 + [], not 403.
pub async fn get_dashboard_session_handler(
    Extension(operator): Extension<AuthenticatedOperator>,
) -> Json<SessionInfoResponse> {
    let mut permissions: Vec<String> = operator
        .effective_permissions
        .iter()
        .map(|p| p.as_str().to_owned())
        .collect();
    permissions.sort();
    Json(SessionInfoResponse {
        operator_id: operator.operator_id,
        permissions,
    })
}

pub async fn get_operator_account(
    State(state): State<AppState>,
    Extension(_operator): Extension<AuthenticatedOperator>,
    Path(account_id): Path<String>,
) -> Result<Json<DashboardAccountDetail>> {
    let response = get_dashboard_account(&state, &account_id).await?;
    Ok(Json(response.account))
}

pub async fn get_operator_account_snapshot(
    State(state): State<AppState>,
    Extension(_operator): Extension<AuthenticatedOperator>,
    Path(account_id): Path<String>,
) -> Result<Json<DashboardAccountSnapshot>> {
    let snapshot = get_account_snapshot(&state, &account_id).await?;
    Ok(Json(snapshot))
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
        // Breaking change per feature `005-operator-dashboard-metrics`
        // FR-001/FR-007: the list endpoint now returns the
        // PagedResult envelope and no longer carries `total_count`.
        // Aggregate inventory totals are exposed via /dashboard/info.
        let list_body: PagedResult<DashboardAccountSummary> = read_json(list_response).await;
        assert_eq!(list_body.items.len(), 2);
        assert_eq!(list_body.items[0].account_id, account_id_hex);
        assert_eq!(
            list_body.items[1].state_status,
            crate::services::DashboardAccountStateStatus::Unavailable
        );
        assert_eq!(list_body.items[1].current_commitment, None);
        assert!(list_body.next_cursor.is_none());

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
        let detail_body: DashboardAccountDetail = read_json(detail_response).await;
        assert_eq!(detail_body.account_id, account_id_hex);
        assert_eq!(
            detail_body.current_commitment,
            list_body.items[0].current_commitment
        );
        assert_eq!(detail_body.auth_scheme, "falcon");
        assert_eq!(detail_body.authorized_signer_count, 1);

        // Snapshot happy path: same account, same authenticated
        // session, hits the new GET /dashboard/accounts/{id}/snapshot
        // route. Confirms the route is registered behind the dashboard
        // middleware, the service decodes the fixture vault, and the
        // snapshot's `as_of_commitment` correlates with the account
        // detail's `current_commitment`.
        let snapshot_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/dashboard/accounts/{account_id_hex}/snapshot"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot_response.status(), StatusCode::OK);
        let snapshot_body: serde_json::Value = {
            let bytes = to_bytes(snapshot_response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        };
        assert_eq!(
            snapshot_body["commitment"].as_str().unwrap(),
            detail_body
                .current_commitment
                .as_deref()
                .expect("detail commitment present for fixture account"),
        );
        assert!(snapshot_body["updated_at"].is_string());
        assert!(snapshot_body["has_pending_candidate"].is_boolean());
        assert!(snapshot_body["vault"]["fungible"].is_array());
        assert!(snapshot_body["vault"]["non_fungible"].is_array());
    }

    #[tokio::test]
    async fn snapshot_endpoint_rejects_evm_accounts_with_unsupported_for_network() {
        use crate::metadata::AccountMetadata;
        use crate::metadata::auth::Auth;

        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".to_string(),
            operator.commitment_hex.clone(),
        )]));

        // Seed an EVM-network account. The snapshot endpoint must
        // surface this as `400 unsupported_for_network` per FR-045,
        // not 503/`account_data_unavailable` — EVM has no Miden vault
        // to decode and the condition is permanent for this surface.
        let account_address = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let chain_id: u64 = 11155111;
        let evm_account_id = crate::metadata::network::evm_account_id(chain_id, account_address);
        let evm_metadata = AccountMetadata {
            account_id: evm_account_id.clone(),
            auth: Auth::EvmEcdsa {
                signers: vec![account_address.to_string()],
            },
            network_config: crate::metadata::NetworkConfig::Evm {
                chain_id,
                account_address: account_address.to_string(),
                multisig_validator_address: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_string(),
            },
            created_at: "2026-05-11T00:00:00Z".to_string(),
            updated_at: "2026-05-11T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        };
        state
            .metadata
            .set(evm_metadata)
            .await
            .expect("metadata should be written");

        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/dashboard/accounts/{evm_account_id}/snapshot"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = {
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice(&bytes).unwrap()
        };
        assert_eq!(body["code"], "unsupported_for_network");
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

    // ----------------------------------------------------------------------
    // Feature `005-operator-dashboard-metrics` integration tests for
    // the breaking-change account list (US1) and the new info
    // endpoint (US2).
    // ----------------------------------------------------------------------

    #[tokio::test]
    async fn list_accounts_paginates_and_emits_cursor_for_resume() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        // Seed five accounts with strictly different updated_at so
        // the cursor predicate is unambiguous.
        for i in 0..5 {
            seed_account(
                &state,
                create_metadata(&format!("acc-{i}"), &format!("2026-05-09T12:0{i}:00Z")),
                None,
            )
            .await;
        }
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        // Page 1: limit=2 → expect 2 items + a next_cursor.
        let page1_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts?limit=2")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page1_response.status(), StatusCode::OK);
        let page1: PagedResult<DashboardAccountSummary> = read_json(page1_response).await;
        assert_eq!(page1.items.len(), 2);
        let cursor_token = page1.next_cursor.clone().expect("cursor for next page");
        // Newest-first by updated_at.
        assert_eq!(page1.items[0].account_id, "acc-4");
        assert_eq!(page1.items[1].account_id, "acc-3");

        // Page 2: resume with the cursor.
        let page2_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/dashboard/accounts?limit=2&cursor={}",
                        cursor_token
                    ))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page2_response.status(), StatusCode::OK);
        let page2: PagedResult<DashboardAccountSummary> = read_json(page2_response).await;
        assert_eq!(page2.items.len(), 2);
        assert_eq!(page2.items[0].account_id, "acc-2");
        assert_eq!(page2.items[1].account_id, "acc-1");
    }

    #[tokio::test]
    async fn list_accounts_rejects_out_of_range_limit_with_invalid_limit_code() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts?limit=9999")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "invalid_limit");
    }

    #[tokio::test]
    async fn list_accounts_rejects_tampered_cursor_with_invalid_cursor_code() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts?cursor=garbage")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "invalid_cursor");
    }

    #[tokio::test]
    async fn dashboard_info_returns_inventory_snapshot() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        for i in 0..3 {
            seed_account(
                &state,
                create_metadata(&format!("acc-{i}"), &format!("2026-05-09T10:0{i}:00Z")),
                None,
            )
            .await;
        }
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/info")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["service_status"], "healthy");
        assert_eq!(body["environment"], "testnet");
        assert_eq!(body["total_account_count"], 3);
        assert_eq!(body["delta_status_counts"]["candidate"], 0);
        assert_eq!(body["delta_status_counts"]["canonical"], 0);
        assert_eq!(body["delta_status_counts"]["discarded"], 0);
        assert_eq!(body["in_flight_proposal_count"], 0);
        assert!(body["latest_activity"].is_null());
        assert_eq!(body["degraded_aggregates"].as_array().unwrap().len(), 0);

        let build = &body["build"];
        assert!(
            build["version"].as_str().is_some_and(|v| !v.is_empty()),
            "build.version must be a non-empty string"
        );
        assert!(
            build["git_commit"].as_str().is_some_and(|v| !v.is_empty()),
            "build.git_commit must be a non-empty string"
        );
        let profile = build["profile"].as_str().unwrap();
        assert!(profile == "debug" || profile == "release");
        assert!(
            chrono::DateTime::parse_from_rfc3339(build["started_at"].as_str().unwrap()).is_ok(),
            "build.started_at must be RFC3339"
        );

        let backend = &body["backend"];
        let storage = backend["storage"].as_str().unwrap();
        assert!(storage == "filesystem" || storage == "postgres");
        let schemes: Vec<&str> = backend["supported_ack_schemes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(schemes.contains(&"falcon"));
        assert!(schemes.contains(&"ecdsa"));
        // canonicalization may be null (optimistic) or an object — we
        // don't assert which here because test fixtures vary; both
        // shapes are spec-valid.
        assert!(backend["canonicalization"].is_object() || backend["canonicalization"].is_null());

        // accounts_by_auth_method present and consistent with total
        // account count (sum of values == total).
        let by_method = body["accounts_by_auth_method"].as_object().unwrap();
        let summed: u64 = by_method.values().map(|v| v.as_u64().unwrap()).sum();
        assert_eq!(summed, 3);
    }

    #[tokio::test]
    async fn dashboard_info_requires_operator_session() {
        let state = create_test_app_state().await;
        let app = create_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn dashboard_info_response_does_not_leak_per_network_field() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/info")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // FR-009: no per-network counts and no singular `network` field.
        let object = body.as_object().unwrap();
        assert!(!object.contains_key("per_network_account_counts"));
        assert!(!object.contains_key("network"));
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

    // -----------------------------------------------------------------
    // Feature 006-operator-authz, User Story 1
    // Read-Only Operator Can Still Use The Dashboard.
    // -----------------------------------------------------------------

    use crate::dashboard::AuthenticatedOperator as AuthOp;
    use crate::dashboard::permissions::Permission;
    use std::collections::BTreeSet;

    fn op_with_perms(signer: &TestSigner, perms: &[Permission]) -> AuthOp {
        let mut set: BTreeSet<Permission> = BTreeSet::new();
        for p in perms {
            set.insert(*p);
        }
        AuthOp {
            operator_id: signer.commitment_hex.clone(),
            commitment: signer.commitment_hex.clone(),
            effective_permissions: Arc::new(set),
        }
    }

    /// US1 Acceptance Scenario 1 + 2: a legacy-grant or explicit
    /// `dashboard:read` operator successfully calls every existing
    /// dashboard read endpoint. Verifies the authz middleware does
    /// not regress existing behavior (SC-001).
    #[tokio::test]
    async fn dashboard_read_succeeds_for_legacy_grant_operator() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        // Legacy-grant (bare-hex equivalent) → {dashboard:read}.
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            op_with_perms(&operator, &[Permission::DashboardRead]),
        ]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        for path in [
            "/dashboard/accounts",
            "/dashboard/info",
            "/dashboard/deltas",
            "/dashboard/proposals",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "legacy-grant operator should be allowed on {path}"
            );
        }
    }

    /// US1 Acceptance Scenario 3: operator with `permissions: []`
    /// receives 403 `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` on
    /// every dashboard read endpoint, with `missing_permissions`
    /// listing `dashboard:read`. Explicit empty is permission
    /// revocation, not legacy-grant (FR-003 / SC-002).
    #[tokio::test]
    async fn dashboard_read_denied_for_empty_permissions_operator() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        // `permissions: []` — explicit revocation.
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            op_with_perms(&operator, &[]),
        ]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        for path in [
            "/dashboard/accounts",
            "/dashboard/info",
            "/dashboard/deltas",
            "/dashboard/proposals",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "explicit-empty operator should be denied on {path}",
            );

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["code"], "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION",
                "expected stable code on {path}",
            );
            assert_eq!(
                body["missing_permissions"],
                serde_json::json!(["dashboard:read"]),
                "expected missing_permissions on {path}",
            );
            assert_eq!(body["retryable"], serde_json::Value::Bool(false));
        }
    }

    /// US1 Acceptance Scenario alongside US2 Scenario 3: 401 still
    /// fires before the authz middleware runs, i.e. `permissions: []`
    /// without a session does not produce a 403; the session layer
    /// short-circuits first (FR-012).
    #[tokio::test]
    async fn unauthenticated_request_returns_401_before_authz() {
        let state = create_test_app_state().await;
        let app = create_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Must not be the authz code — the session layer rejected
        // before authz ran.
        assert_ne!(
            body["code"], "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION",
            "401 path must not surface authz code"
        );
    }

    // -----------------------------------------------------------------
    // Feature 006-operator-authz, User Story 2
    // Mutating Action Requires The Mutating Permission.
    // The probe endpoint stands in for #181 (Account Pause) until that
    // ships. Tests are gated behind the `authz-test-probe` Cargo feature so
    // they only run when the route is actually registered.
    // -----------------------------------------------------------------

    /// US2 Scenario 4 / FR-028 / SC-010: release builds (feature off)
    /// return 404 on the probe path and write no audit row.
    #[tokio::test]
    async fn probe_path_returns_404_without_authz_probe_feature() {
        // Skip when the feature is on — covered by the gated tests below.
        if cfg!(feature = "authz-test-probe") {
            return;
        }
        let state = create_test_app_state().await;
        let app = create_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/dashboard/_authz_probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "authz-test-probe")]
    mod authz_probe {
        use super::*;
        use crate::audit::{AuditOutcome, kinds};
        use crate::testing::helpers::CapturingAuditor;

        const PROBE_URI: &str = "/dashboard/_authz_probe";

        async fn state_with_capturing_auditor() -> (AppState, Arc<CapturingAuditor>) {
            let auditor = Arc::new(CapturingAuditor::new());
            let mut state = create_test_app_state().await;
            state.auditor = auditor.clone();
            (state, auditor)
        }

        /// US2 Scenario 2: pause-capable operator → 204 + one
        /// `probe.access` success event.
        #[tokio::test]
        async fn probe_succeeds_for_pause_capable_operator() {
            let operator = TestSigner::new();
            let (mut state, auditor) = state_with_capturing_auditor().await;
            state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
                op_with_perms(
                    &operator,
                    &[Permission::DashboardRead, Permission::AccountsPause],
                ),
            ]));
            let app = create_router(state);
            let cookie = authenticate_operator(&app, &operator).await;

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(PROBE_URI)
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);

            let events = auditor.snapshot();
            assert_eq!(events.len(), 1, "expected one audit event");
            assert_eq!(events[0].action_kind, kinds::PROBE_ACCESS);
            assert_eq!(events[0].outcome, AuditOutcome::Success);
            assert_eq!(events[0].error_code, None);
            assert_eq!(events[0].operator_identity, operator.commitment_hex);
            // Symmetric with the auth.denied payload shape.
            let payload = &events[0].payload;
            assert_eq!(payload["route_path"], "/dashboard/_authz_probe");
            assert_eq!(payload["http_method"], "POST");
            assert_eq!(
                payload["required_permissions"],
                serde_json::json!(["accounts:pause"])
            );
        }

        /// US2 Scenario 1: read-only operator → 403 + one
        /// `auth.denied` event with the missing-permission payload.
        #[tokio::test]
        async fn probe_denied_for_read_only_operator() {
            let operator = TestSigner::new();
            let (mut state, auditor) = state_with_capturing_auditor().await;
            state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
                op_with_perms(&operator, &[Permission::DashboardRead]),
            ]));
            let app = create_router(state);
            let cookie = authenticate_operator(&app, &operator).await;

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(PROBE_URI)
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["code"], "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION");
            assert_eq!(
                body["missing_permissions"],
                serde_json::json!(["accounts:pause"])
            );

            let events = auditor.snapshot();
            assert_eq!(events.len(), 1, "expected one audit event");
            assert_eq!(events[0].action_kind, kinds::AUTH_DENIED);
            assert_eq!(events[0].outcome, AuditOutcome::Denied);
            assert_eq!(
                events[0].error_code.as_deref(),
                Some("GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION")
            );
            assert_eq!(events[0].operator_identity, operator.commitment_hex);
            // Payload carries route + method + required (FR-025).
            // Note: nested routers see the inner path, not the outer
            // mount prefix — the audit row records what the route
            // declared in its own router, which is the stable form
            // across mount points.
            let payload = &events[0].payload;
            assert_eq!(payload["route_path"], "/dashboard/_authz_probe");
            assert_eq!(payload["http_method"], "POST");
            assert_eq!(
                payload["required_permissions"],
                serde_json::json!(["accounts:pause"])
            );
        }

        /// US2 Scenario 3: unauthenticated caller → 401 from session
        /// layer, no audit event written. Authentication failures are
        /// NOT authorization audit events (Edge Case 19).
        #[tokio::test]
        async fn probe_unauthenticated_returns_401_and_no_audit_row() {
            let (state, auditor) = state_with_capturing_auditor().await;
            let app = create_router(state);
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(PROBE_URI)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                auditor.snapshot().len(),
                0,
                "401 must not produce an audit event"
            );
        }
    }

    // GET /dashboard/session tests (US6 / SC-013).

    /// SC-013 case 1: populated permissions returned in lex order.
    #[tokio::test]
    async fn session_endpoint_returns_lex_ordered_permissions() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            op_with_perms(
                &operator,
                // Enum Ord differs from wire-string lex order — forces
                // the handler to sort the strings, not the enum.
                &[
                    Permission::DashboardRead,
                    Permission::PoliciesWrite,
                    Permission::AccountsPause,
                ],
            ),
        ]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["operator_id"], operator.commitment_hex);
        assert_eq!(
            body["permissions"],
            serde_json::json!(["accounts:pause", "dashboard:read", "policies:write"]),
            "permissions must be lex-ordered ASCII",
        );
    }

    /// SC-013 case 2 / FR-034: `permissions: []` → 200 + [], not 403.
    /// Lets the UI distinguish "no permissions" from "not logged in".
    #[tokio::test]
    async fn session_endpoint_returns_200_with_empty_permissions_for_empty_entry() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            op_with_perms(&operator, &[]),
        ]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/session must NOT be gated by the dashboard:read authz layer",
        );
        let body: serde_json::Value = read_json(response).await;
        assert_eq!(body["operator_id"], operator.commitment_hex);
        assert_eq!(body["permissions"], serde_json::json!([]));
    }

    /// SC-013 case 3: no valid session → 401 from the session middleware.
    #[tokio::test]
    async fn session_endpoint_requires_operator_session() {
        let state = create_test_app_state().await;
        let app = create_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// SC-013 case 4 / FR-008: allowlist edits reflect on the next
    /// `/session` call without re-login.
    #[tokio::test]
    async fn session_endpoint_reflects_hot_reload_within_active_session() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        let dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            op_with_perms(&operator, &[Permission::DashboardRead]),
        ]));
        state.dashboard = dashboard.clone();
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        let initial = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initial.status(), StatusCode::OK);
        let initial_body: serde_json::Value = read_json(initial).await;
        assert_eq!(
            initial_body["permissions"],
            serde_json::json!(["dashboard:read"]),
        );

        // Grant `accounts:pause` without re-issuing the session.
        dashboard
            .replace_allowlist_for_tests(vec![op_with_perms(
                &operator,
                &[Permission::DashboardRead, Permission::AccountsPause],
            )])
            .await;

        let after_grant = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after_grant.status(), StatusCode::OK);
        let after_grant_body: serde_json::Value = read_json(after_grant).await;
        assert_eq!(
            after_grant_body["permissions"],
            serde_json::json!(["accounts:pause", "dashboard:read"]),
            "hot-reloaded permissions must be visible on the next /session call",
        );

        dashboard
            .replace_allowlist_for_tests(vec![op_with_perms(&operator, &[])])
            .await;
        let after_revoke = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after_revoke.status(), StatusCode::OK);
        let after_revoke_body: serde_json::Value = read_json(after_revoke).await;
        assert_eq!(after_revoke_body["permissions"], serde_json::json!([]));
    }

    /// FR-035: `/session` writes no `admin_actions` event on success
    /// or 401.
    #[tokio::test]
    async fn session_endpoint_does_not_write_audit_events() {
        use crate::testing::helpers::CapturingAuditor;

        let operator = TestSigner::new();
        let auditor = Arc::new(CapturingAuditor::new());
        let mut state = create_test_app_state().await;
        state.auditor = auditor.clone();
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            op_with_perms(&operator, &[Permission::DashboardRead]),
        ]));
        let app = create_router(state);
        let cookie = authenticate_operator(&app, &operator).await;

        // Success path.
        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        // 401 path.
        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        assert_eq!(
            auditor.snapshot().len(),
            0,
            "/session must not produce any admin_actions events",
        );
    }
}
