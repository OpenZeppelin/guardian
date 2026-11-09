//! Operator endpoints for policy module parameters.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Deserialize;

use crate::dashboard::AuthenticatedOperator;
use crate::error::{GuardianError, Result};
use crate::modules::admin::{
    self, AccountModulesView, ModuleCatalogView, ModuleInstanceView, ParamsHistoryView,
};
use crate::modules::store::Assigned;
use crate::state::AppState;

const BODY_LIMIT: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
pub struct ParamsBody {
    pub params: serde_json::Value,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ReasonBody {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct AssignmentBody {
    pub modules: Vec<Assigned>,
    pub reason: String,
}

async fn body<T: serde::de::DeserializeOwned>(
    request: axum::extract::Request,
) -> Result<(T, Option<String>)> {
    let client_ip = crate::middleware::client_ip::extract_client_ip(&request);
    let (_parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| GuardianError::InvalidInput("failed to read request body".into()))?;
    let parsed = serde_json::from_slice(&bytes)
        .map_err(|e| GuardianError::InvalidInput(format!("malformed request body: {e}")))?;
    Ok((parsed, client_ip))
}

pub async fn list_modules(State(state): State<AppState>) -> Result<Json<ModuleCatalogView>> {
    admin::catalog_view(&state).await.map(Json)
}

pub async fn account_modules(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
) -> Result<Json<AccountModulesView>> {
    admin::account_view(&state, &account_id).await.map(Json)
}

pub async fn params_history(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
) -> Result<Json<ParamsHistoryView>> {
    admin::history(&state, &module_id).await.map(Json)
}

pub async fn set_global_params(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path(module_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<ModuleInstanceView>> {
    let (body, client_ip): (ParamsBody, _) = body(request).await?;
    admin::set_global(
        &state,
        &operator,
        &module_id,
        body.params,
        &body.reason,
        client_ip,
    )
    .await
    .map(Json)
}

pub async fn enable_module(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path(module_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<ModuleInstanceView>> {
    let (body, client_ip): (ReasonBody, _) = body(request).await?;
    admin::enable(&state, &operator, &module_id, &body.reason, client_ip)
        .await
        .map(Json)
}

pub async fn disable_module(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path(module_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<ModuleInstanceView>> {
    let (body, client_ip): (ReasonBody, _) = body(request).await?;
    admin::disable(&state, &operator, &module_id, &body.reason, client_ip)
        .await
        .map(Json)
}

pub async fn set_account_layer(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path((account_id, module_id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Result<Json<AccountModulesView>> {
    let (body, client_ip): (ParamsBody, _) = body(request).await?;
    admin::set_layer(
        &state,
        &operator,
        &account_id,
        &module_id,
        Some(body.params),
        &body.reason,
        client_ip,
    )
    .await
    .map(Json)
}

pub async fn remove_account_layer(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path((account_id, module_id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Result<Json<AccountModulesView>> {
    let (body, client_ip): (ReasonBody, _) = body(request).await?;
    admin::set_layer(
        &state,
        &operator,
        &account_id,
        &module_id,
        None,
        &body.reason,
        client_ip,
    )
    .await
    .map(Json)
}

pub async fn set_assignment(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path(account_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<AccountModulesView>> {
    let (body, client_ip): (AssignmentBody, _) = body(request).await?;
    admin::set_assignment(
        &state,
        &operator,
        &account_id,
        body.modules,
        &body.reason,
        client_ip,
    )
    .await
    .map(Json)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use guardian_module_sdk::basic_policy::BasicPolicy;
    use guardian_shared::hex::FromHex;
    use miden_protocol::Word;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::dashboard::DashboardState;
    use crate::dashboard::permissions::Permission;
    use crate::modules::catalog::HookName;
    use crate::modules::testing::{instance_with_params, pipeline};
    use crate::testing::helpers::{TestSigner, create_router, create_test_app_state};

    use super::*;

    fn operator(signer: &TestSigner, permissions: &[Permission]) -> AuthenticatedOperator {
        AuthenticatedOperator {
            operator_id: signer.commitment_hex.clone(),
            commitment: signer.commitment_hex.clone(),
            effective_permissions: Arc::new(permissions.iter().copied().collect::<BTreeSet<_>>()),
        }
    }

    async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn login(app: &axum::Router, signer: &TestSigner) -> String {
        let (_, challenge) = send(
            app,
            Request::get(format!(
                "/auth/challenge?commitment={}",
                signer.commitment_hex
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
        let digest = challenge["challenge"]["signing_digest"].as_str().unwrap();
        let signature = signer.sign_word(Word::from_hex(digest).unwrap());
        let response = app
            .clone()
            .oneshot(
                Request::post("/auth/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "commitment": signer.commitment_hex, "signature": signature })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    fn put(path: &str, cookie: &str, body: serde_json::Value) -> Request<Body> {
        Request::put(path)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(path: &str, cookie: &str) -> Request<Body> {
        Request::get(path)
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn global_params_need_their_own_permission_and_are_recorded() {
        let support = TestSigner::new();
        let admin = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![
            operator(
                &support,
                &[Permission::DashboardRead, Permission::PoliciesWrite],
            ),
            operator(
                &admin,
                &[Permission::DashboardRead, Permission::PoliciesWriteGlobal],
            ),
        ]));
        state.modules = Some(pipeline(vec![instance_with_params(
            "blocklist",
            &[HookName::BeforeTransactionApproved],
            Arc::new(BasicPolicy),
            json!({ "recipient_denylist": ["0xaaa"] }),
        )]));
        let app = create_router(state);
        let support_cookie = login(&app, &support).await;
        let admin_cookie = login(&app, &admin).await;
        let body =
            json!({ "params": { "recipient_denylist": ["0xaaa", "0xddd"] }, "reason": "OP-90" });

        let (status, _) = send(
            &app,
            put(
                "/dashboard/modules/blocklist/params",
                &support_cookie,
                body.clone(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, view) = send(
            &app,
            put("/dashboard/modules/blocklist/params", &admin_cookie, body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["params"]["recipient_denylist"][1], "0xddd");

        let (status, catalog) = send(&app, get("/dashboard/modules", &support_cookie)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(catalog["modules"][0]["params_drift"], true);

        let (status, history) = send(
            &app,
            get(
                "/dashboard/modules/blocklist/params/history",
                &support_cookie,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(history["entries"][0]["reason"], "OP-90");
        assert_eq!(history["entries"][0]["operator"], admin.commitment_hex);

        let (status, _) = send(
            &app,
            put(
                "/dashboard/accounts/0xmissing/modules/blocklist/params",
                &support_cookie,
                json!({ "params": {}, "reason": "r" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    fn post(path: &str, cookie: &str, body: serde_json::Value) -> Request<Body> {
        Request::post(path)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn the_disable_permission_can_disable_but_not_enable() {
        let on_call = TestSigner::new();
        let mut state = create_test_app_state().await;
        state.dashboard = Arc::new(DashboardState::for_tests_with_permissions(vec![operator(
            &on_call,
            &[Permission::ModulesDisable],
        )]));
        state.modules = Some(pipeline(vec![instance_with_params(
            "risk-scorer",
            &[HookName::BeforeTransactionApproved],
            Arc::new(BasicPolicy),
            json!({}),
        )]));
        state
            .modules
            .as_deref()
            .unwrap()
            .store()
            .seed_instance("risk-scorer", true, &json!({}), "2026-10-15T00:00:00Z")
            .await
            .unwrap();
        let app = create_router(state);
        let cookie = login(&app, &on_call).await;
        let reason = json!({ "reason": "scorer returning errors" });

        let (status, view) = send(
            &app,
            post(
                "/dashboard/modules/risk-scorer/disable",
                &cookie,
                reason.clone(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["enabled"], false);

        let (status, _) = send(
            &app,
            post("/dashboard/modules/risk-scorer/enable", &cookie, reason),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
