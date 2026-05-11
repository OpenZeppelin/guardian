//! Dashboard HTTP handlers for per-account history (deltas,
//! proposals) and (Phase 8/9) the global cross-account feeds.
//!
//! Spec reference: `005-operator-dashboard-metrics`.
//!
//! These handlers run behind the operator-session middleware
//! (`require_dashboard_session`). All responses use the
//! [`crate::services::dashboard_pagination::PagedResult`] envelope.
//! Errors are surfaced as [`crate::error::GuardianError`] which
//! implements `IntoResponse` with a stable `code` field per FR-028.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

use crate::dashboard::cursor::CursorKind;
use crate::error::Result;
use crate::services::{
    DashboardDeltaEntry, DashboardGlobalDeltaEntry, DashboardGlobalProposalEntry,
    DashboardProposalEntry, PagedResult, list_account_deltas, list_account_proposals,
    list_global_deltas, list_global_proposals, parse_cursor, parse_limit, parse_status_filter,
};
use crate::state::AppState;

/// Common `?limit=&cursor=` query parameters for the per-account
/// history endpoints.
#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `?limit=&cursor=&status=` query parameters for the global delta
/// feed (FR-031..FR-035). The `status` parameter is comma-separated
/// (e.g. `status=candidate,canonical`).
#[derive(Debug, Deserialize)]
pub struct GlobalDeltasQuery {
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// `GET /dashboard/accounts/{account_id}/deltas`. Returns the
/// per-account delta history paginated newest-first by `nonce DESC`,
/// surfacing only `candidate` / `canonical` / `discarded` statuses
/// (pending lives on the proposal queue endpoint).
pub async fn list_account_deltas_handler(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<PagedResult<DashboardDeltaEntry>>> {
    let limit = parse_limit(query.limit.as_deref())?;
    let cursor = parse_cursor(
        query.cursor.as_deref(),
        state.dashboard.cursor_secret(),
        CursorKind::AccountDeltas,
    )?;
    let result = list_account_deltas(&state, &account_id, limit, cursor).await?;
    Ok(Json(result))
}

/// `GET /dashboard/accounts/{account_id}/proposals`. Returns the
/// in-flight multisig proposal queue for one account, paginated
/// newest-first by `(nonce DESC, commitment DESC)`. Single-key Miden
/// and EVM accounts always return an empty page per FR-017.
pub async fn list_account_proposals_handler(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<PagedResult<DashboardProposalEntry>>> {
    let limit = parse_limit(query.limit.as_deref())?;
    let cursor = parse_cursor(
        query.cursor.as_deref(),
        state.dashboard.cursor_secret(),
        CursorKind::AccountProposals,
    )?;
    let result = list_account_proposals(&state, &account_id, limit, cursor).await?;
    Ok(Json(result))
}

/// `GET /dashboard/deltas`. Cross-account delta feed paginated
/// newest-first by `status_timestamp DESC`. Optional comma-separated
/// `status` filter restricts to a subset of `{candidate, canonical,
/// discarded}`. Pending entries live on the proposal feed.
///
/// Spec reference: `005-operator-dashboard-metrics` US6, FR-031..FR-035.
pub async fn list_global_deltas_handler(
    State(state): State<AppState>,
    Query(query): Query<GlobalDeltasQuery>,
) -> Result<Json<PagedResult<DashboardGlobalDeltaEntry>>> {
    let limit = parse_limit(query.limit.as_deref())?;
    let cursor = parse_cursor(
        query.cursor.as_deref(),
        state.dashboard.cursor_secret(),
        CursorKind::GlobalDeltas,
    )?;
    let status_filter = parse_status_filter(query.status.as_deref())?;
    let result = list_global_deltas(&state, limit, cursor, status_filter).await?;
    Ok(Json(result))
}

/// `GET /dashboard/proposals`. Cross-account in-flight proposal feed
/// paginated newest-first by `originating_timestamp DESC`. Takes no
/// `status` filter — every entry is in-flight by definition. EVM
/// accounts do not appear in v1 per FR-017.
///
/// Spec reference: `005-operator-dashboard-metrics` US7, FR-035..FR-037.
pub async fn list_global_proposals_handler(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<PagedResult<DashboardGlobalProposalEntry>>> {
    let limit = parse_limit(query.limit.as_deref())?;
    let cursor = parse_cursor(
        query.cursor.as_deref(),
        state.dashboard.cursor_secret(),
        CursorKind::GlobalProposals,
    )?;
    let result = list_global_proposals(&state, limit, cursor).await?;
    Ok(Json(result))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use guardian_shared::hex::FromHex;
    use miden_protocol::Word;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::api::dashboard::OperatorChallengeResponse;
    use crate::dashboard::DashboardState;
    use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::testing::helpers::{TestSigner, create_router, create_test_app_state};

    const FIXTURE_ACCOUNT_ID: &str = "0xacc0000000000000";

    async fn authenticate(app: &axum::Router, operator: &TestSigner) -> String {
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
        let bytes = to_bytes(challenge_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let challenge: OperatorChallengeResponse = serde_json::from_slice(&bytes).unwrap();
        let signing_digest = Word::from_hex(&challenge.challenge.signing_digest).unwrap();
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
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_string)
            .expect("session cookie")
    }

    fn miden_metadata(auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id: FIXTURE_ACCOUNT_ID.to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    fn canonical_delta(nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: FIXTURE_ACCOUNT_ID.into(),
            nonce,
            prev_commitment: format!("0xprev{nonce}"),
            new_commitment: Some(format!("0xnew{nonce}")),
            delta_payload: json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Canonical {
                timestamp: format!("2026-05-08T12:00:0{nonce}Z"),
            },
        }
    }

    fn pending_proposal(nonce: u64, commitment: &str, sigs: usize) -> DeltaObject {
        let cosigner_sigs = (0..sigs)
            .map(|i| CosignerSignature {
                signature: guardian_shared::ProposalSignature::from_scheme(
                    guardian_shared::SignatureScheme::Falcon,
                    "00".into(),
                    None,
                ),
                timestamp: format!("2026-05-08T12:0{i}:00Z"),
                signer_id: format!("0xsigner{i}"),
            })
            .collect();
        DeltaObject {
            account_id: FIXTURE_ACCOUNT_ID.into(),
            nonce,
            prev_commitment: format!("0xprev{nonce}"),
            new_commitment: Some(commitment.into()),
            delta_payload: json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: format!("2026-05-08T12:00:0{nonce}Z"),
                proposer_id: "0xproposer".into(),
                cosigner_sigs,
            },
        }
    }

    async fn seed_account(state: &crate::state::AppState, metadata: AccountMetadata) {
        state.metadata.set(metadata).await.expect("metadata write");
    }

    #[tokio::test]
    async fn deltas_endpoint_returns_200_with_paged_envelope() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xc1".into()],
        };
        seed_account(&state, miden_metadata(auth)).await;
        for nonce in 1u64..=3 {
            state
                .storage
                .submit_delta(&canonical_delta(nonce))
                .await
                .expect("submit delta");
        }
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/dashboard/accounts/{FIXTURE_ACCOUNT_ID}/deltas?limit=10"
                    ))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let nonces: Vec<u64> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["nonce"].as_u64().unwrap())
            .collect();
        assert_eq!(nonces, vec![3, 2, 1]);
        assert!(body["next_cursor"].is_null());
    }

    #[tokio::test]
    async fn deltas_endpoint_returns_404_for_unknown_account() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts/0xunknown/deltas")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "account_not_found");
    }

    #[tokio::test]
    async fn deltas_endpoint_rejects_out_of_range_limit() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/dashboard/accounts/{FIXTURE_ACCOUNT_ID}/deltas?limit=9999"
                    ))
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
    async fn deltas_endpoint_rejects_tampered_cursor() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/dashboard/accounts/{FIXTURE_ACCOUNT_ID}/deltas?cursor=garbage"
                    ))
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
    async fn deltas_endpoint_returns_401_without_session() {
        let state = create_test_app_state().await;
        let app = create_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/dashboard/accounts/{FIXTURE_ACCOUNT_ID}/deltas"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn proposals_endpoint_returns_in_flight_proposals_for_multisig_account() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xc1".into(), "0xc2".into(), "0xc3".into()],
        };
        seed_account(&state, miden_metadata(auth)).await;
        let proposal = pending_proposal(7, "0xab12cd34", 2);
        state
            .storage
            .submit_delta_proposal("0xab12cd34", &proposal)
            .await
            .expect("submit proposal");
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/dashboard/accounts/{FIXTURE_ACCOUNT_ID}/proposals"
                    ))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["nonce"], 7);
        assert_eq!(body["items"][0]["signatures_collected"], 2);
        assert_eq!(body["items"][0]["signatures_required"], 3);
        assert_eq!(body["items"][0]["proposer_id"], "0xproposer");
    }

    #[tokio::test]
    async fn proposals_endpoint_returns_empty_for_evm_account() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let auth = Auth::EvmEcdsa {
            signers: vec!["0xsigner".into()],
        };
        seed_account(&state, miden_metadata(auth)).await;
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/dashboard/accounts/{FIXTURE_ACCOUNT_ID}/proposals"
                    ))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
        assert!(body["next_cursor"].is_null());
    }

    #[tokio::test]
    async fn proposals_endpoint_returns_404_for_unknown_account() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/accounts/0xnotfound/proposals")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ----------------------------------------------------------------------
    // Phase 8/9 — global cross-account feed endpoints (US6, US7).
    // ----------------------------------------------------------------------

    #[tokio::test]
    async fn global_deltas_endpoint_returns_paged_envelope_with_account_id() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xc1".into()],
        };
        seed_account(&state, miden_metadata(auth)).await;
        for nonce in 1u64..=3 {
            state
                .storage
                .submit_delta(&canonical_delta(nonce))
                .await
                .expect("submit delta");
        }
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/deltas?limit=10")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        // Every entry carries account_id.
        for entry in items {
            assert_eq!(entry["account_id"], FIXTURE_ACCOUNT_ID);
        }
        assert!(body["next_cursor"].is_null());
    }

    #[tokio::test]
    async fn global_deltas_endpoint_rejects_unknown_status_filter_value() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/deltas?status=foo")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "invalid_status_filter");
    }

    #[tokio::test]
    async fn global_deltas_endpoint_accepts_csv_status_filter() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xc1".into()],
        };
        seed_account(&state, miden_metadata(auth)).await;
        for nonce in 1u64..=2 {
            state
                .storage
                .submit_delta(&canonical_delta(nonce))
                .await
                .expect("submit delta");
        }
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/deltas?status=candidate,canonical")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // 2 canonical entries from the seed; both pass the filter.
        assert_eq!(body["items"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn global_proposals_endpoint_returns_in_flight_proposals() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xc1".into(), "0xc2".into(), "0xc3".into()],
        };
        seed_account(&state, miden_metadata(auth)).await;
        let proposal = pending_proposal(7, "0xab12cd34", 2);
        state
            .storage
            .submit_delta_proposal("0xab12cd34", &proposal)
            .await
            .expect("submit proposal");
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/proposals")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["account_id"], FIXTURE_ACCOUNT_ID);
        assert_eq!(body["items"][0]["nonce"], 7);
        assert_eq!(body["items"][0]["signatures_collected"], 2);
        assert_eq!(body["items"][0]["signatures_required"], 3);
    }

    #[tokio::test]
    async fn global_feeds_require_operator_session() {
        let state = create_test_app_state().await;
        let app = create_router(state);

        for path in ["/dashboard/deltas", "/dashboard/proposals"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "expected 401 for {path}"
            );
        }
    }

    #[tokio::test]
    async fn global_deltas_endpoint_rejects_tampered_cursor() {
        let operator = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests(vec![(
            "operator-1".into(),
            operator.commitment_hex.clone(),
        )]));
        let app = create_router(state);
        let cookie = authenticate(&app, &operator).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/deltas?cursor=garbage")
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
}
