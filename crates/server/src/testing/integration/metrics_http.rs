//! End-to-end metrics exposition: real filesystem-backed app state,
//! instrumented storage, the HTTP tracking layer over the full API
//! router, the slow-aggregate refresher logic, and a scrape through
//! the dedicated metrics router with the bearer guard engaged.
//!
//! Tests drive a current-thread runtime *inside*
//! `metrics::with_local_recorder` — local recorders are thread-local
//! and never installed globally (the global recorder is process-wide
//! and set-once; see `crate::metrics::recorder`).

use crate::metrics::config::MetricsConfig;
use crate::metrics::recorder::build_recorder;
use crate::metrics::refresher::{apply_snapshot, fetch_snapshot};
use crate::metrics::router::metrics_router;
use crate::metrics::storage::InstrumentedStorage;
use crate::metrics::track_http;
use crate::testing::helpers::{create_router, create_test_app_state};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "integration-scrape-token";

async fn body_text(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Drive API traffic and a refresher round, then scrape the metrics
/// router and return the exposition body.
fn run_scrape_scenario() -> String {
    let recorder = build_recorder();
    let handle = recorder.handle();

    metrics::with_local_recorder(&recorder, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut state = create_test_app_state().await;
            state.storage = Arc::new(InstrumentedStorage::new(state.storage));

            let app = create_router(state.clone()).layer(axum::middleware::from_fn(track_http));

            // Known route, unmatched route, and a templated dashboard
            // route with a raw account id in the path.
            for uri in [
                "/pubkey",
                "/definitely-not-a-route",
                "/state?account_id=0xabc123",
            ] {
                let _ = app
                    .clone()
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
            }

            // One refresher round (storage call goes through the
            // instrumented decorator, gauges land in the recorder).
            let snapshot = fetch_snapshot(&state.storage, &state.metadata, &state.clock)
                .await
                .expect("refresh against empty filesystem backend succeeds");
            apply_snapshot(&snapshot);

            // Scrape through the dedicated router with the guard on.
            let config = MetricsConfig::default().with_bearer_token(TOKEN.to_string());
            let metrics_app = metrics_router(handle.clone(), &config);
            let response = metrics_app
                .oneshot(
                    Request::builder()
                        .uri("/metrics")
                        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            body_text(response).await
        })
    })
}

#[test]
fn scrape_exposes_http_storage_and_aggregate_metrics() {
    let exposition = run_scrape_scenario();

    // HTTP request path: bounded route labels, unmatched collapse.
    assert!(
        exposition.contains(
            "guardian_http_requests_total{method=\"GET\",route=\"/pubkey\",status=\"200\"} 1"
        ),
        "missing /pubkey counter in:\n{exposition}"
    );
    assert!(exposition.contains("route=\"unmatched\""));
    assert!(
        !exposition.contains("definitely-not-a-route"),
        "raw unmatched path leaked into labels"
    );
    assert!(exposition.contains("guardian_http_request_duration_seconds_bucket"));

    // Storage path: the refresher's aggregate queries went through the
    // instrumented decorator.
    assert!(
        exposition.contains(
            "guardian_storage_operations_total{operation=\"count_deltas_by_status\",outcome=\"ok\"} 1"
        ),
        "missing storage counter in:\n{exposition}"
    );

    // Slow aggregates: gauges populated by the refresher round, plus
    // the staleness timestamp.
    assert!(exposition.contains("guardian_deltas{status=\"canonical\"} 0"));
    assert!(exposition.contains("guardian_proposals_in_flight 0"));
    assert!(exposition.contains("guardian_accounts 0"));
    assert!(exposition.contains("guardian_metrics_refresh_timestamp_seconds"));

    // Process metrics from the collector run during the scrape.
    assert!(
        exposition.contains("process_"),
        "missing process_* metrics in:\n{exposition}"
    );
}

#[test]
fn metrics_scrape_requires_bearer_token_when_configured() {
    let recorder = build_recorder();
    let handle = recorder.handle();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let config = MetricsConfig::default().with_bearer_token(TOKEN.to_string());
        let metrics_app = metrics_router(handle, &config);

        let unauthorized = metrics_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = metrics_app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    });
}

/// The main API router must never expose `/metrics` — the exposition
/// lives exclusively on the dedicated listener.
#[tokio::test]
async fn main_api_router_does_not_serve_metrics() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
