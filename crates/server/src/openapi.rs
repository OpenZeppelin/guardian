//! OpenAPI specification generation for the Guardian HTTP API.
//!
//! Issue #241. The spec is generated from `#[utoipa::path]` annotations
//! on the HTTP handlers and `#[derive(utoipa::ToSchema)]` on the wire
//! models. [`openapi`] returns the assembled document; it is served at
//! runtime from `GET /api-docs/openapi.json` (see `builder/handle.rs`)
//! and written to a committed file by the `gen-openapi` binary.
//!
//! Guardian exposes two HTTP surfaces, both documented here: the
//! **client** API (`tag = "client"`) consumed by SDKs/packages, and the
//! operator **dashboard** API (`tag = "dashboard"`). The feature-gated
//! **evm** surface is included when the `evm` feature is on.

use serde::Serialize;
use utoipa::OpenApi;

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

/// Always-on Guardian API surface: the client API and the operator
/// dashboard API. EVM routes are merged in separately by [`openapi`]
/// when the `evm` feature is enabled.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Guardian API",
        description = "Guardian coordination service HTTP API. Covers the client-facing \
                       contract consumed by SDKs/packages and the operator dashboard API.",
        license(name = "AGPL-3.0", identifier = "AGPL-3.0"),
    ),
    paths(
        // --- client API ---
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
        // --- dashboard API ---
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
    tags(
        (name = "client", description = "Client-facing API consumed by SDKs and packages."),
        (name = "dashboard", description = "Operator dashboard API."),
        (name = "evm", description = "EVM smart-account API (available when the `evm` feature is enabled)."),
    )
)]
pub struct ApiDoc;

/// Feature-gated EVM API surface, merged into the base document by
/// [`openapi`] when the `evm` feature is enabled.
#[cfg(feature = "evm")]
#[derive(OpenApi)]
#[openapi(paths(
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
))]
struct EvmApiDoc;

/// Build the complete OpenAPI document for the running server build.
/// The version is taken from the crate version at compile time. EVM
/// paths/schemas are merged in only when the `evm` feature is active so
/// the spec always reflects the routes actually mounted.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();

    #[cfg(feature = "evm")]
    doc.merge(EvmApiDoc::openapi());

    doc
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn openapi_spec_builds_and_serializes() {
        let doc = openapi();
        // Serializes to valid JSON.
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
    }

    #[cfg(feature = "evm")]
    #[test]
    fn openapi_spec_includes_evm_paths_when_feature_enabled() {
        let json = serde_json::to_value(openapi()).unwrap();
        let paths = json["paths"].as_object().unwrap();
        assert!(paths.contains_key("/evm/proposals"), "evm path missing");
    }
}
