//! The dedicated metrics router served on its own listener.
//!
//! Deliberately separate from the main API router: scrapes bypass the
//! rate limiter, CORS, and body limits, and the main API port never
//! exposes `/metrics`. Protection is layered per the Prometheus
//! security model — loopback bind by default, optional shared-secret
//! bearer token, TLS delegated to a reverse proxy or private network.

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use metrics_process::Collector;
use std::sync::Arc;

use super::config::MetricsConfig;
use crate::secret::{SecretString, ct_eq};

/// Classic Prometheus text exposition content type. OpenMetrics and
/// native histograms are deliberate non-goals for v1 (no Rust
/// text-format support yet); Prometheus 3.x scrapes this natively.
const EXPOSITION_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Clone)]
struct MetricsEndpoint {
    handle: PrometheusHandle,
    /// Standard `process_*` collector (CPU, RSS, fds). Collected on
    /// each scrape: cheap kernel reads, not storage queries.
    process: Collector,
    bearer_token: Option<Arc<SecretString>>,
}

/// Build the metrics router for the dedicated listener. The exposition
/// path comes from config; everything else 404s.
pub fn metrics_router(handle: PrometheusHandle, config: &MetricsConfig) -> Router {
    let endpoint = MetricsEndpoint {
        handle,
        process: Collector::default(),
        bearer_token: config.bearer_token.clone().map(Arc::new),
    };
    endpoint.process.describe();

    Router::new()
        .route(&config.path, get(serve_metrics))
        .layer(middleware::from_fn_with_state(
            endpoint.clone(),
            require_bearer,
        ))
        .with_state(endpoint)
}

async fn serve_metrics(State(endpoint): State<MetricsEndpoint>) -> impl IntoResponse {
    endpoint.process.collect();
    (
        [(header::CONTENT_TYPE, EXPOSITION_CONTENT_TYPE)],
        endpoint.handle.render(),
    )
}

/// Shared-secret scrape guard. With no token configured the listener
/// is open (operators relying on network isolation alone); with a
/// token, anything but a constant-time-equal `Authorization: Bearer`
/// value gets a bodyless `401`. No `WWW-Authenticate` detail is
/// returned — a scraper misconfiguration is diagnosed from the
/// Prometheus side, not by hinting at the expected scheme.
async fn require_bearer(
    State(endpoint): State<MetricsEndpoint>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = endpoint.bearer_token.as_deref() else {
        return next.run(request).await;
    };

    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match provided {
        Some(token) if ct_eq(token.as_bytes(), expected.expose_secret().as_bytes()) => {
            next.run(request).await
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::recorder::build_recorder;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn test_config() -> MetricsConfig {
        MetricsConfig::default()
    }

    async fn send(router: Router, path: &str, auth: Option<&str>) -> axum::http::Response<Body> {
        let mut builder = HttpRequest::builder().uri(path);
        if let Some(value) = auth {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        router
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn serves_exposition_with_content_type() {
        let recorder = build_recorder();
        let router = metrics_router(recorder.handle(), &test_config());

        let response = send(router, "/metrics", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            EXPOSITION_CONTENT_TYPE
        );
    }

    #[tokio::test]
    async fn honors_configured_path() {
        let recorder = build_recorder();
        let mut config = test_config();
        config.path = "/internal/metrics".to_string();
        let router = metrics_router(recorder.handle(), &config);

        let ok = send(router.clone(), "/internal/metrics", None).await;
        assert_eq!(ok.status(), StatusCode::OK);

        let miss = send(router, "/metrics", None).await;
        assert_eq!(miss.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn no_token_configured_leaves_endpoint_open() {
        let recorder = build_recorder();
        let router = metrics_router(recorder.handle(), &test_config());
        let response = send(router, "/metrics", None).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_guard_rejects_missing_wrong_scheme_and_wrong_token() {
        let recorder = build_recorder();
        let config = test_config().with_bearer_token("scrape-secret".to_string());
        let router = metrics_router(recorder.handle(), &config);

        for auth in [
            None,
            Some("Basic scrape-secret"),
            Some("Bearer wrong-token"),
            Some("scrape-secret"),
        ] {
            let response = send(router.clone(), "/metrics", auth).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "auth {auth:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn bearer_guard_accepts_correct_token() {
        let recorder = build_recorder();
        let config = test_config().with_bearer_token("scrape-secret".to_string());
        let router = metrics_router(recorder.handle(), &config);

        let response = send(router, "/metrics", Some("Bearer scrape-secret")).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn render_includes_recorded_metrics() {
        let recorder = build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!(crate::metrics::names::RATE_LIMIT_REJECTIONS_TOTAL,
                crate::metrics::names::LABEL_LIMIT_TYPE => "burst")
            .increment(3);
        });

        let router = metrics_router(handle, &test_config());
        let response = send(router, "/metrics", None).await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("guardian_rate_limit_rejections_total{limit_type=\"burst\"} 3"),
            "missing counter in:\n{text}"
        );
    }
}
