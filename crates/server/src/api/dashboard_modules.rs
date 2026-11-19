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

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ParamsBody {
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
    pub reason: String,
    /// The params version the change was made against: `params_version` for a global write,
    /// `account_params_version` for an account layer. Set, a write after another change is
    /// refused with 409 `GUARDIAN_MODULE_PARAMS_STALE`.
    #[serde(default)]
    pub expected_version: Option<u64>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LayerRemovalBody {
    pub reason: String,
    /// The `account_params_version` the removal was made against; see [`ParamsBody`].
    #[serde(default)]
    pub expected_version: Option<u64>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ReasonBody {
    pub reason: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AssignmentBody {
    pub modules: Vec<Assigned>,
    pub reason: String,
    /// The `account_params_version` the assignment was made against; see [`ParamsBody`].
    #[serde(default)]
    pub expected_version: Option<u64>,
}

async fn body<T: serde::de::DeserializeOwned>(
    request: axum::extract::Request,
) -> Result<(T, Option<String>)> {
    let client_ip = crate::middleware::client_ip::extract_client_ip(&request);
    let (_parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, BODY_LIMIT).await.map_err(|_| {
        GuardianError::ModuleWriteRefused(format!(
            "the request body could not be read or is over {BODY_LIMIT} bytes"
        ))
    })?;
    let malformed = |e: serde_json::Error| {
        GuardianError::ModuleWriteRefused(format!("malformed request body: {e}"))
    };
    serde::de::DeserializeSeed::deserialize(
        UniqueKeys,
        &mut serde_json::Deserializer::from_slice(&bytes),
    )
    .map_err(malformed)?;
    let parsed = serde_json::from_slice(&bytes).map_err(malformed)?;
    Ok((parsed, client_ip))
}

/// Walks a JSON document and fails on the first object that names a key twice. `serde_json`
/// keeps the last of two equal keys without a word, so `{"cap": "5", "cap": "50"}` would store
/// a cap of 50 that the operator may not have meant; params are refused instead.
struct UniqueKeys;

impl<'de> serde::de::DeserializeSeed<'de> for UniqueKeys {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for UniqueKeys {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<(), A::Error> {
        while seq.next_element_seed(UniqueKeys)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<(), A::Error> {
        let mut seen = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "key {key:?} appears twice in one object"
                )));
            }
            map.next_value_seed(UniqueKeys)?;
        }
        Ok(())
    }
}

#[utoipa::path(
    get,
    path = "/dashboard/modules",
    tag = "dashboard",
    security(("operator_session" = [])),
    responses(
        (status = 200, description = "Module instances with their runtime state", body = ModuleCatalogView),
        (status = 400, description = "No policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing dashboard:read permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn list_modules(State(state): State<AppState>) -> Result<Json<ModuleCatalogView>> {
    admin::catalog_view(&state).await.map(Json)
}

#[utoipa::path(
    get,
    path = "/dashboard/accounts/{account_id}/modules",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("account_id" = String, Path, description = "Account identifier")),
    responses(
        (status = 200, description = "The account's effective module set, in evaluation order", body = AccountModulesView),
        (status = 400, description = "No policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing dashboard:read permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn account_modules(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
) -> Result<Json<AccountModulesView>> {
    admin::account_view(&state, &account_id).await.map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    /// Only entries whose `id` is below this one, to page backwards.
    before: Option<u64>,
    /// At most this many entries (1 to 1,000; default 100).
    limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/dashboard/modules/{module_id}/params/history",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("module_id" = String, Path, description = "Module instance id"), HistoryQuery),
    responses(
        (status = 200, description = "Params changes, newest first", body = ParamsHistoryView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing dashboard:read permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn params_history(
    State(state): State<AppState>,
    Path(module_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Result<Json<ParamsHistoryView>> {
    admin::history(&state, &module_id, query.before, query.limit)
        .await
        .map(Json)
}

#[utoipa::path(
    put,
    path = "/dashboard/modules/{module_id}/params",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("module_id" = String, Path, description = "Module instance id")),
    request_body = ParamsBody,
    responses(
        (status = 200, description = "Global params stored", body = ModuleInstanceView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing policies:write_global permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
        (status = 409, description = "expected_version is set and the params changed since (GUARDIAN_MODULE_PARAMS_STALE)", body = crate::openapi::ApiErrorResponse),
    )
)]
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
        admin::Write {
            reason: &body.reason,
            expected_version: body.expected_version,
        },
        client_ip,
    )
    .await
    .map(Json)
}

#[utoipa::path(
    post,
    path = "/dashboard/modules/{module_id}/enable",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("module_id" = String, Path, description = "Module instance id")),
    request_body = ReasonBody,
    responses(
        (status = 200, description = "Instance enabled", body = ModuleInstanceView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing policies:write permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
        (status = 503, description = "The module failed its startup checks; nothing changed", body = crate::openapi::ApiErrorResponse),
    )
)]
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

#[utoipa::path(
    post,
    path = "/dashboard/modules/{module_id}/disable",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("module_id" = String, Path, description = "Module instance id")),
    request_body = ReasonBody,
    responses(
        (status = 200, description = "Instance disabled", body = ModuleInstanceView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing policies:write or modules:disable permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
        (status = 409, description = "The catalog does not mark the instance operator_can_disable", body = crate::openapi::ApiErrorResponse),
    )
)]
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

#[utoipa::path(
    put,
    path = "/dashboard/accounts/{account_id}/modules/{module_id}/params",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("account_id" = String, Path, description = "Account identifier"), ("module_id" = String, Path, description = "Module instance id")),
    request_body = ParamsBody,
    responses(
        (status = 200, description = "Account layer stored", body = AccountModulesView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing policies:write permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
        (status = 409, description = "the account params changed since expected_version, or, without it, since this write read them (GUARDIAN_MODULE_PARAMS_STALE)", body = crate::openapi::ApiErrorResponse),
    )
)]
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
        admin::Write {
            reason: &body.reason,
            expected_version: body.expected_version,
        },
        client_ip,
    )
    .await
    .map(Json)
}

#[utoipa::path(
    delete,
    path = "/dashboard/accounts/{account_id}/modules/{module_id}/params",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("account_id" = String, Path, description = "Account identifier"), ("module_id" = String, Path, description = "Module instance id")),
    request_body = LayerRemovalBody,
    responses(
        (status = 200, description = "Account layer removed", body = AccountModulesView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing policies:write permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
        (status = 409, description = "the account params changed since expected_version, or, without it, since this write read them (GUARDIAN_MODULE_PARAMS_STALE)", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn remove_account_layer(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    Path((account_id, module_id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Result<Json<AccountModulesView>> {
    let (body, client_ip): (LayerRemovalBody, _) = body(request).await?;
    admin::set_layer(
        &state,
        &operator,
        &account_id,
        &module_id,
        None,
        admin::Write {
            reason: &body.reason,
            expected_version: body.expected_version,
        },
        client_ip,
    )
    .await
    .map(Json)
}

#[utoipa::path(
    put,
    path = "/dashboard/accounts/{account_id}/modules",
    tag = "dashboard",
    security(("operator_session" = [])),
    params(("account_id" = String, Path, description = "Account identifier")),
    request_body = AssignmentBody,
    responses(
        (status = 200, description = "Assignment stored", body = AccountModulesView),
        (status = 400, description = "Invalid params, reason or assignment, or no policy modules are configured", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "No operator session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Missing policies:write permission", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Account or module not found", body = crate::openapi::ApiErrorResponse),
        (status = 409, description = "the account params changed since expected_version, or, without it, since this write read them (GUARDIAN_MODULE_PARAMS_STALE)", body = crate::openapi::ApiErrorResponse),
    )
)]
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
        admin::Write {
            reason: &body.reason,
            expected_version: body.expected_version,
        },
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
    use guardian_module_sdk::policies::RecipientPolicy;
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

    fn unique_keys(text: &str) -> std::result::Result<(), serde_json::Error> {
        serde::de::DeserializeSeed::deserialize(
            UniqueKeys,
            &mut serde_json::Deserializer::from_str(text),
        )
    }

    #[test]
    fn a_body_naming_a_key_twice_is_refused_at_any_depth() {
        assert!(unique_keys(r#"{"params":{"cap":"5"},"reason":"r"}"#).is_ok());
        assert!(
            unique_keys(
                r#"{"params":{"max_per_transaction":{"0xa":"5","0xa":"50"}},"reason":"r"}"#
            )
            .unwrap_err()
            .to_string()
            .contains(r#"key "0xa" appears twice"#)
        );
        assert!(unique_keys(r#"{"modules":[{"module_id":"a","module_id":"b"}]}"#).is_err());
        assert!(unique_keys(r#"{"reason":"r","reason":"s"}"#).is_err());
        assert!(
            unique_keys(r#"{"params":{"cap":"5","\u0063ap":"50"}}"#).is_err(),
            "an escaped spelling of the same key is the same key"
        );
        assert!(
            unique_keys(r#"{"a":{"k":1},"b":{"k":2}}"#).is_ok(),
            "the same key in two objects is fine"
        );
    }

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
            Arc::new(RecipientPolicy),
            json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
        )]));
        let app = create_router(state);
        let support_cookie = login(&app, &support).await;
        let admin_cookie = login(&app, &admin).await;
        let body = json!({ "params": { "denylist": ["0x000000000000000000000000000aaa", "0x000000000000000000000000000ddd"] }, "reason": "OP-90" });

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
        assert_eq!(
            view["params"]["denylist"][1],
            "0x000000000000000000000000000ddd"
        );

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
            Arc::new(RecipientPolicy),
            json!({}),
        )]));
        state
            .modules
            .as_deref()
            .unwrap()
            .store()
            .seed_instance(
                "risk-scorer",
                true,
                &json!({}),
                &json!({}),
                "2026-10-15T00:00:00Z",
            )
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
