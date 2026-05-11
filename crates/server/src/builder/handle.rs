use axum::{
    Router, extract::DefaultBodyLimit, middleware::from_fn_with_state, routing::get, routing::post,
    routing::put,
};
use tonic::transport::Server;
use tower_http::cors::CorsLayer;

use crate::api::dashboard::{
    challenge_operator_login, get_dashboard_info_handler, get_operator_account,
    get_operator_account_snapshot, list_operator_accounts, logout_operator, verify_operator_login,
};
use crate::api::dashboard_feeds::{
    list_account_deltas_handler, list_account_proposals_handler, list_global_deltas_handler,
    list_global_proposals_handler,
};
#[cfg(feature = "evm")]
use crate::api::evm::{
    approve_evm_proposal, cancel_evm_proposal, challenge_evm_session, create_evm_proposal,
    get_evm_proposal, get_executable_evm_proposal, list_evm_proposals, logout_evm_session,
    register_evm_account, verify_evm_session,
};
use crate::api::grpc::GuardianService;
use crate::api::grpc::guardian::FILE_DESCRIPTOR_SET;
use crate::api::grpc::guardian::guardian_server::GuardianServer;
use crate::api::http::{
    configure, get_delta, get_delta_proposal, get_delta_proposals, get_delta_since, get_pubkey,
    get_state, lookup, push_delta, push_delta_proposal, sign_delta_proposal,
};
use crate::dashboard::require_dashboard_session;
use crate::middleware::{BodyLimitConfig, RateLimitConfig, RateLimitLayer};
use crate::services::start_canonicalization_worker;
use crate::state::AppState;

/// Handle for a configured server instance
///
/// Provides methods to run the server with the configured settings.
pub struct ServerHandle {
    pub(crate) app_state: AppState,
    pub(crate) cors_layer: Option<CorsLayer>,
    pub(crate) rate_limit_config: Option<RateLimitConfig>,
    pub(crate) body_limit_config: Option<BodyLimitConfig>,
    pub(crate) http_enabled: bool,
    pub(crate) http_port: u16,
    pub(crate) grpc_enabled: bool,
    pub(crate) grpc_port: u16,
}

impl ServerHandle {
    /// Run the server with the configured settings
    pub async fn run(self) {
        async fn root() -> &'static str {
            "Hello, World!"
        }

        let mut tasks = Vec::new();

        // Start background jobs based on canonicalization config
        if let Some(config) = &self.app_state.canonicalization {
            tracing::info!(
                check_interval_seconds = config.check_interval_seconds,
                max_retries = config.max_retries,
                submission_grace_period_seconds = config.submission_grace_period_seconds,
                "Starting canonicalization worker"
            );
            start_canonicalization_worker(self.app_state.clone());
        } else {
            tracing::info!(
                "Running in optimistic mode - deltas accepted without on-chain verification"
            );
        }

        // Start HTTP server if enabled
        if self.http_enabled {
            let state = self.app_state.clone();
            let port = self.http_port;
            let cors_layer = self.cors_layer.clone();
            let rate_limit_config = self.rate_limit_config.clone();
            let body_limit_config = self.body_limit_config;

            let task = tokio::spawn(async move {
                let dashboard_routes = Router::new()
                    .route("/accounts", get(list_operator_accounts))
                    .route("/accounts/{account_id}", get(get_operator_account))
                    .route(
                        "/accounts/{account_id}/snapshot",
                        get(get_operator_account_snapshot),
                    )
                    .route(
                        "/accounts/{account_id}/deltas",
                        get(list_account_deltas_handler),
                    )
                    .route(
                        "/accounts/{account_id}/proposals",
                        get(list_account_proposals_handler),
                    )
                    .route("/info", get(get_dashboard_info_handler))
                    .route("/deltas", get(list_global_deltas_handler))
                    .route("/proposals", get(list_global_proposals_handler))
                    .route_layer(from_fn_with_state(state.clone(), require_dashboard_session));

                let app = Router::new()
                    .route("/", get(root))
                    .route("/delta", post(push_delta))
                    .route("/delta", get(get_delta))
                    .route("/delta/since", get(get_delta_since))
                    .route("/delta/proposal", post(push_delta_proposal))
                    .route("/delta/proposal", get(get_delta_proposals))
                    .route("/delta/proposal/single", get(get_delta_proposal))
                    .route("/delta/proposal", put(sign_delta_proposal))
                    .route("/configure", post(configure))
                    .route("/state", get(get_state))
                    .route("/state/lookup", get(lookup))
                    .route("/pubkey", get(get_pubkey))
                    .route("/auth/challenge", get(challenge_operator_login))
                    .route("/auth/verify", post(verify_operator_login))
                    .route("/auth/logout", post(logout_operator));

                #[cfg(feature = "evm")]
                let app = app
                    .route("/evm/auth/challenge", get(challenge_evm_session))
                    .route("/evm/auth/verify", post(verify_evm_session))
                    .route("/evm/auth/logout", post(logout_evm_session))
                    .route("/evm/accounts", post(register_evm_account))
                    .route(
                        "/evm/proposals",
                        post(create_evm_proposal).get(list_evm_proposals),
                    )
                    .route("/evm/proposals/{proposal_id}", get(get_evm_proposal))
                    .route(
                        "/evm/proposals/{proposal_id}/approve",
                        post(approve_evm_proposal),
                    )
                    .route(
                        "/evm/proposals/{proposal_id}/executable",
                        get(get_executable_evm_proposal),
                    )
                    .route(
                        "/evm/proposals/{proposal_id}/cancel",
                        post(cancel_evm_proposal),
                    );

                let mut app = app.nest("/dashboard", dashboard_routes).with_state(state);

                // Apply body size limit
                let body_limit = body_limit_config.unwrap_or_else(BodyLimitConfig::from_env);
                app = app.layer(DefaultBodyLimit::max(body_limit.max_bytes));

                // Apply rate limiting
                let rate_limit = rate_limit_config.unwrap_or_else(RateLimitConfig::from_env);
                app = app.layer(RateLimitLayer::new(rate_limit));

                if let Some(cors) = cors_layer {
                    app = app.layer(cors);
                }

                let addr = format!("0.0.0.0:{port}");
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .expect("Failed to bind HTTP server");

                tracing::info!(
                    address = %listener.local_addr().unwrap(),
                    "HTTP server listening"
                );

                // Use into_make_service_with_connect_info to capture client socket address
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .expect("HTTP server failed");
            });

            tasks.push(task);
        }

        // Start gRPC server if enabled
        if self.grpc_enabled {
            let state = self.app_state.clone();
            let port = self.grpc_port;

            let task = tokio::spawn(async move {
                let addr = format!("0.0.0.0:{port}")
                    .parse()
                    .expect("Invalid gRPC address");

                let service = GuardianService { app_state: state };

                // Enable gRPC reflection
                let reflection_service = tonic_reflection::server::Builder::configure()
                    .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
                    .build_v1()
                    .expect("Failed to build reflection service");

                tracing::info!(address = %addr, "gRPC server listening");

                Server::builder()
                    .add_service(GuardianServer::new(service))
                    .add_service(reflection_service)
                    .serve(addr)
                    .await
                    .expect("gRPC server failed");
            });

            tasks.push(task);
        }

        if tasks.is_empty() {
            tracing::warn!("No servers enabled");
            return;
        }

        // Wait for all servers
        for task in tasks {
            let _ = task.await;
        }
    }
}
