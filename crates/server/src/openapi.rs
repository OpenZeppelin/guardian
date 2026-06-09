//! OpenAPI specification generation for the Guardian HTTP API.
//!
//! Issue #241. The spec is generated from `#[utoipa::path]` annotations
//! on the HTTP handlers and `#[derive(utoipa::ToSchema)]` on the wire
//! models, so it cannot drift from the implementation.
//!
//! Guardian exposes two HTTP surfaces, both documented here: the
//! **client** API (`tag = "client"`) consumed by SDKs/packages, and the
//! operator **dashboard** API (`tag = "dashboard"`). The feature-gated
//! **evm** surface is included when the `evm` feature is on.
//!
//! Four documents are produced (see the `gen-openapi` binary):
//!   - the combined spec ([`openapi`]) served at `GET /api-docs/openapi.json`,
//!   - a per-surface spec for each of client / dashboard / evm, which map
//!     to the existing client packages and keep SDK generation scoped.

use serde::Serialize;
use utoipa::{
    Modify, OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};

/// Wire shape of a Guardian error response body. Mirrors the envelope
/// produced by [`crate::error::GuardianError`]'s `IntoResponse` impl.
/// Documented as the body of every non-2xx response. Optional fields
/// are populated only for the error codes that carry them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ApiErrorResponse {
    /// Always `false` for error responses.
    pub success: bool,
    /// Stable, machine-readable error code (e.g. `account_not_found`).
    pub code: String,
    /// Human-readable error message.
    pub error: String,
    /// Seconds to wait before retrying. Present only for
    /// `rate_limit_exceeded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u32>,
    /// Lex-sorted permissions the operator lacks. Present only for
    /// `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_permissions: Option<Vec<String>>,
    /// `false` for permission denials and `GUARDIAN_ACCOUNT_PAUSED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    /// RFC 3339 pause timestamp. Present only for
    /// `GUARDIAN_ACCOUNT_PAUSED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
    /// Pause reason. Present only for `GUARDIAN_ACCOUNT_PAUSED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_reason: Option<String>,
}

/// Security scheme name for the signed-request public key header.
pub const SEC_PUBKEY: &str = "x-pubkey";
/// Security scheme name for the signed-request signature header.
pub const SEC_SIGNATURE: &str = "x-signature";
/// Security scheme name for the signed-request timestamp header.
pub const SEC_TIMESTAMP: &str = "x-timestamp";
/// Security scheme name for the operator dashboard session cookie.
pub const SEC_OPERATOR_SESSION: &str = "operator_session";
/// Security scheme name for the EVM session cookie.
pub const SEC_EVM_SESSION: &str = "evm_session";

fn add_client_schemes(components: &mut utoipa::openapi::Components) {
    // Per-account Miden requests carry three signed headers; all three
    // are required together (`spec/api.md` "Miden Request Signing").
    components.add_security_scheme(
        SEC_PUBKEY,
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("x-pubkey"))),
    );
    components.add_security_scheme(
        SEC_SIGNATURE,
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("x-signature"))),
    );
    components.add_security_scheme(
        SEC_TIMESTAMP,
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("x-timestamp"))),
    );
}

fn add_operator_scheme(components: &mut utoipa::openapi::Components) {
    components.add_security_scheme(
        SEC_OPERATOR_SESSION,
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(
            "guardian_operator_session",
        ))),
    );
}

fn add_evm_scheme(components: &mut utoipa::openapi::Components) {
    components.add_security_scheme(
        SEC_EVM_SESSION,
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("guardian_evm_session"))),
    );
}

/// Registers the signed-header schemes used by the client API.
pub struct ClientSecurityAddon;
impl Modify for ClientSecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        add_client_schemes(openapi.components.get_or_insert_with(Default::default));
    }
}

/// Registers the operator-session cookie scheme used by the dashboard API.
pub struct DashboardSecurityAddon;
impl Modify for DashboardSecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        add_operator_scheme(openapi.components.get_or_insert_with(Default::default));
    }
}

/// Registers the EVM-session cookie scheme used by the EVM API.
pub struct EvmSecurityAddon;
impl Modify for EvmSecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        add_evm_scheme(openapi.components.get_or_insert_with(Default::default));
    }
}

/// Injects the cross-cutting error responses every endpoint can return
/// from middleware — `429` (rate limit) and `413` (body size limit) —
/// into every operation, so they need not be repeated in each
/// `#[utoipa::path]` annotation. Existing per-operation responses for
/// those codes are left untouched.
pub struct CommonResponsesAddon;
impl Modify for CommonResponsesAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::{Content, Ref, RefOr, ResponseBuilder};

        let make = |desc: &str| {
            RefOr::T(
                ResponseBuilder::new()
                    .description(desc)
                    .content(
                        "application/json",
                        Content::new(Some(RefOr::Ref(Ref::from_schema_name("ApiErrorResponse")))),
                    )
                    .build(),
            )
        };

        for item in openapi.paths.paths.values_mut() {
            let ops = [
                item.get.as_mut(),
                item.put.as_mut(),
                item.post.as_mut(),
                item.delete.as_mut(),
                item.patch.as_mut(),
            ];
            for op in ops.into_iter().flatten() {
                op.responses
                    .responses
                    .entry("429".to_string())
                    .or_insert_with(|| make("Rate limit exceeded"));
                op.responses
                    .responses
                    .entry("413".to_string())
                    .or_insert_with(|| make("Request body exceeds the configured size limit"));
            }
        }
    }
}

/// Client-facing API surface (signed-header auth).
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Guardian Client API",
        description = "Client-facing Guardian HTTP API consumed by SDKs and packages.",
        license(name = "AGPL-3.0", identifier = "AGPL-3.0"),
    ),
    paths(
        crate::api::http::configure,
        crate::api::http::push_delta,
        crate::api::http::get_delta,
        crate::api::http::get_delta_since,
        crate::api::http::get_state,
        crate::api::http::lookup,
        crate::api::http::get_pubkey,
        crate::api::http::push_delta_proposal,
        crate::api::http::get_delta_proposals,
        crate::api::http::get_delta_proposal,
        crate::api::http::sign_delta_proposal,
    ),
    components(schemas(ApiErrorResponse)),
    modifiers(&ClientSecurityAddon, &CommonResponsesAddon),
    tags((name = "client", description = "Client-facing API consumed by SDKs and packages.")),
)]
pub struct ClientApiDoc;

/// Operator dashboard API surface (operator-session cookie auth).
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Guardian Dashboard API",
        description = "Operator dashboard Guardian HTTP API.",
        license(name = "AGPL-3.0", identifier = "AGPL-3.0"),
    ),
    paths(
        crate::api::dashboard::challenge_operator_login,
        crate::api::dashboard::verify_operator_login,
        crate::api::dashboard::logout_operator,
        crate::api::dashboard::list_operator_accounts,
        crate::api::dashboard::get_dashboard_info_handler,
        crate::api::dashboard::get_dashboard_session_handler,
        crate::api::dashboard::get_operator_account,
        crate::api::dashboard::get_operator_account_snapshot,
        crate::api::dashboard::pause_account_handler,
        crate::api::dashboard::unpause_account_handler,
        crate::api::dashboard_feeds::list_account_deltas_handler,
        crate::api::dashboard_feeds::list_account_delta_detail_handler,
        crate::api::dashboard_feeds::list_account_proposals_handler,
        crate::api::dashboard_feeds::list_global_deltas_handler,
        crate::api::dashboard_feeds::list_global_proposals_handler,
    ),
    components(schemas(ApiErrorResponse)),
    modifiers(&DashboardSecurityAddon, &CommonResponsesAddon),
    tags((name = "dashboard", description = "Operator dashboard API.")),
)]
pub struct DashboardApiDoc;

/// Feature-gated EVM API surface (EVM-session cookie auth).
#[cfg(feature = "evm")]
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Guardian EVM API",
        description = "EVM smart-account Guardian HTTP API (the `evm` feature).",
        license(name = "AGPL-3.0", identifier = "AGPL-3.0"),
    ),
    paths(
        crate::api::evm::challenge_evm_session,
        crate::api::evm::verify_evm_session,
        crate::api::evm::logout_evm_session,
        crate::api::evm::register_evm_account,
        crate::api::evm::create_evm_proposal,
        crate::api::evm::list_evm_proposals,
        crate::api::evm::get_evm_proposal,
        crate::api::evm::approve_evm_proposal,
        crate::api::evm::get_executable_evm_proposal,
        crate::api::evm::cancel_evm_proposal,
    ),
    components(schemas(ApiErrorResponse)),
    modifiers(&EvmSecurityAddon, &CommonResponsesAddon),
    tags((name = "evm", description = "EVM smart-account API.")),
)]
pub struct EvmApiDoc;

fn with_version(mut doc: utoipa::openapi::OpenApi) -> utoipa::openapi::OpenApi {
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();
    doc
}

/// Client-only spec (`docs/openapi-client.json`).
pub fn client_openapi() -> utoipa::openapi::OpenApi {
    with_version(ClientApiDoc::openapi())
}

/// Dashboard-only spec (`docs/openapi-dashboard.json`).
pub fn dashboard_openapi() -> utoipa::openapi::OpenApi {
    with_version(DashboardApiDoc::openapi())
}

/// EVM-only spec (`docs/openapi-evm.json`). Only available when the
/// `evm` feature is enabled.
#[cfg(feature = "evm")]
pub fn evm_openapi() -> utoipa::openapi::OpenApi {
    with_version(EvmApiDoc::openapi())
}

/// Build the combined OpenAPI document for the running server build.
/// EVM paths/schemas are merged in only when the `evm` feature is active
/// so the spec always reflects the routes actually mounted. Security
/// schemes for every merged surface are (re)applied explicitly so they
/// are present regardless of `merge` semantics.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = ClientApiDoc::openapi();
    doc.merge(DashboardApiDoc::openapi());
    #[cfg(feature = "evm")]
    doc.merge(EvmApiDoc::openapi());

    let components = doc.components.get_or_insert_with(Default::default);
    add_client_schemes(components);
    add_operator_scheme(components);
    #[cfg(feature = "evm")]
    add_evm_scheme(components);

    doc.info.title = "Guardian API".to_string();
    doc.info.description = Some(
        "Guardian coordination service HTTP API. Covers the client-facing contract \
         consumed by SDKs/packages and the operator dashboard API."
            .to_string(),
    );
    with_version(doc)
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn openapi_spec_builds_and_serializes() {
        let doc = openapi();
        let json = serde_json::to_value(&doc).expect("spec serializes to JSON");
        assert_eq!(json["openapi"].as_str().unwrap_or(""), "3.1.0");

        // A representative sample of every surface is present.
        let paths = json["paths"].as_object().expect("paths object");
        assert!(paths.contains_key("/configure"), "client API path missing");
        assert!(paths.contains_key("/delta"), "client API path missing");
        assert!(
            paths.contains_key("/dashboard/accounts"),
            "dashboard API path missing"
        );
        assert!(
            paths.contains_key("/dashboard/accounts/{account_id}/deltas/{nonce}"),
            "dashboard feed path missing"
        );

        // Core wire models are registered as components.
        let schemas = json["components"]["schemas"]
            .as_object()
            .expect("schemas object");
        assert!(
            schemas.contains_key("DeltaObject"),
            "DeltaObject schema missing"
        );
        assert!(
            schemas.contains_key("StateObject"),
            "StateObject schema missing"
        );
        assert!(
            schemas.contains_key("ApiErrorResponse"),
            "error schema missing"
        );

        // Security schemes are registered and applied to authenticated ops.
        let schemes = json["components"]["securitySchemes"]
            .as_object()
            .expect("securitySchemes object");
        assert!(schemes.contains_key(SEC_PUBKEY), "header scheme missing");
        assert!(
            schemes.contains_key(SEC_OPERATOR_SESSION),
            "operator cookie scheme missing"
        );
        // A signed client endpoint requires all three headers.
        let cfg_sec = &json["paths"]["/configure"]["post"]["security"][0];
        assert!(cfg_sec.get(SEC_PUBKEY).is_some(), "configure not secured");
        assert!(cfg_sec.get(SEC_SIGNATURE).is_some());
        assert!(cfg_sec.get(SEC_TIMESTAMP).is_some());
        // Public endpoint carries no security requirement.
        assert!(
            json["paths"]["/pubkey"]["get"].get("security").is_none(),
            "/pubkey should be public"
        );
    }

    #[test]
    fn per_surface_specs_are_scoped() {
        let client = serde_json::to_value(client_openapi()).unwrap();
        assert!(
            client["paths"]
                .as_object()
                .unwrap()
                .contains_key("/configure")
        );
        assert!(
            !client["paths"]
                .as_object()
                .unwrap()
                .contains_key("/dashboard/accounts"),
            "client spec must not contain dashboard paths"
        );

        let dash = serde_json::to_value(dashboard_openapi()).unwrap();
        assert!(
            dash["paths"]
                .as_object()
                .unwrap()
                .contains_key("/dashboard/accounts")
        );
        assert!(
            !dash["paths"]
                .as_object()
                .unwrap()
                .contains_key("/configure"),
            "dashboard spec must not contain client paths"
        );
    }

    #[cfg(feature = "evm")]
    #[test]
    fn openapi_spec_includes_evm_paths_when_feature_enabled() {
        let json = serde_json::to_value(openapi()).unwrap();
        let paths = json["paths"].as_object().unwrap();
        assert!(paths.contains_key("/evm/proposals"), "evm path missing");

        let evm = serde_json::to_value(evm_openapi()).unwrap();
        assert!(
            evm["paths"]
                .as_object()
                .unwrap()
                .contains_key("/evm/proposals")
        );
        assert!(
            evm["components"]["securitySchemes"]
                .as_object()
                .unwrap()
                .contains_key(SEC_EVM_SESSION)
        );
    }
}
