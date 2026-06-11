//! HTTP request-path instrumentation.
//!
//! `track_http` is added with `Router::layer` as the outermost layer
//! of the main API router, so it observes every response the server
//! produces — including rate-limiter 429s and the 404 fallback. axum
//! runs router-level layers after routing, so `MatchedPath` (the
//! bounded route template) is readable from request extensions here;
//! requests that matched no route carry none and collapse into the
//! single `route="unmatched"` series.

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use metrics::{counter, gauge, histogram};
use std::time::Instant;

use super::names::{
    HTTP_REQUEST_DURATION_SECONDS, HTTP_REQUESTS_IN_FLIGHT, HTTP_REQUESTS_TOTAL, LABEL_METHOD,
    LABEL_ROUTE, LABEL_STATUS, normalize_method, normalize_route,
};

/// Decrements the in-flight gauge on drop, so cancelled requests
/// (client disconnects abort the middleware future) still release
/// their slot.
struct InFlightGuard;

impl InFlightGuard {
    fn acquire() -> Self {
        gauge!(HTTP_REQUESTS_IN_FLIGHT).increment(1.0);
        Self
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        gauge!(HTTP_REQUESTS_IN_FLIGHT).decrement(1.0);
    }
}

/// Record request count, latency, and in-flight depth for every HTTP
/// request. All three label values are bounded: methods collapse to
/// the standard set, routes to templates, statuses to the numeric
/// codes the server actually produces.
pub async fn track_http(request: Request, next: Next) -> Response {
    let method = normalize_method(request.method());
    let route = normalize_route(
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
    )
    .to_owned();

    let started = Instant::now();
    let in_flight = InFlightGuard::acquire();
    let response = next.run(request).await;
    drop(in_flight);

    let status = response.status().as_u16().to_string();
    counter!(HTTP_REQUESTS_TOTAL,
        LABEL_METHOD => method, LABEL_ROUTE => route.clone(), LABEL_STATUS => status)
    .increment(1);
    histogram!(HTTP_REQUEST_DURATION_SECONDS,
        LABEL_METHOD => method, LABEL_ROUTE => route)
    .record(started.elapsed().as_secs_f64());

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::recorder::build_recorder;
    use axum::{Router, body::Body, http::StatusCode, middleware, routing::get};
    use tower::ServiceExt;

    fn test_app() -> Router {
        Router::new()
            .route("/pubkey", get(async || "ok"))
            .route(
                "/dashboard/accounts/{account_id}",
                get(async || (StatusCode::ACCEPTED, "ok")),
            )
            .layer(middleware::from_fn(track_http))
    }

    /// Local recorders are thread-local, so the request must be driven
    /// on this thread: build a current-thread runtime *inside* the
    /// recorder scope.
    fn render_after(requests: &[&str]) -> String {
        let recorder = build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                for uri in requests {
                    let app = test_app();
                    let _ = app
                        .oneshot(
                            axum::http::Request::builder()
                                .uri(*uri)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                }
            });
        });
        handle.render()
    }

    #[test]
    fn records_route_template_not_raw_path() {
        let rendered = render_after(&["/dashboard/accounts/0xdeadbeef42"]);
        assert!(
            rendered.contains("route=\"/dashboard/accounts/{account_id}\""),
            "expected templated route in:\n{rendered}"
        );
        assert!(
            !rendered.contains("0xdeadbeef42"),
            "raw account id leaked into labels:\n{rendered}"
        );
        assert!(rendered.contains("status=\"202\""));
    }

    #[test]
    fn unmatched_paths_collapse_into_one_series() {
        let rendered = render_after(&["/no-such-route-1", "/no-such-route-2"]);
        assert!(
            rendered.contains("route=\"unmatched\",status=\"404\"} 2"),
            "expected a single unmatched series with count 2 in:\n{rendered}"
        );
        assert!(!rendered.contains("no-such-route"));
    }

    #[test]
    fn records_duration_histogram_and_counter() {
        let rendered = render_after(&["/pubkey"]);
        assert!(rendered.contains(
            "guardian_http_requests_total{method=\"GET\",route=\"/pubkey\",status=\"200\"} 1"
        ));
        assert!(
            rendered.contains("guardian_http_request_duration_seconds_bucket"),
            "expected duration buckets in:\n{rendered}"
        );
    }

    #[test]
    fn in_flight_gauge_returns_to_zero() {
        let rendered = render_after(&["/pubkey", "/pubkey"]);
        assert!(
            rendered.contains("guardian_http_requests_in_flight 0"),
            "in-flight gauge must return to 0 in:\n{rendered}"
        );
    }
}
