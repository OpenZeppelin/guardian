use crate::middleware::rate_limit::{RateLimitConfig, RateLimitLayer, RateLimitStore};
use crate::middleware::rate_limit_grpc::GrpcRateLimitLayer;

use axum::http::StatusCode;
use axum::routing::get;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tonic::Code;
use tower::{Layer, Service, ServiceExt};

type GrpcResponse = http::Response<tonic::body::Body>;

fn grpc_service(
    store: RateLimitStore,
) -> (
    impl Service<http::Request<()>, Response = GrpcResponse, Error = Infallible> + Clone,
    Arc<AtomicUsize>,
) {
    let handled = Arc::new(AtomicUsize::new(0));
    let counter = handled.clone();
    let inner = tower::service_fn(move |_request: http::Request<()>| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(http::Response::new(tonic::body::Body::default()))
        }
    });
    (GrpcRateLimitLayer::new(store).layer(inner), handled)
}

fn grpc_request(path: &str, forwarded_for: Option<&str>) -> http::Request<()> {
    let mut builder = http::Request::builder().uri(path);
    if let Some(ip) = forwarded_for {
        builder = builder.header("x-forwarded-for", ip.to_string());
    }
    builder.body(()).unwrap()
}

fn http_pubkey_app(store: RateLimitStore) -> axum::Router {
    axum::Router::new()
        .route("/pubkey", get(|| async { "ok" }))
        .layer(RateLimitLayer::new(store))
}

fn http_get(ip: &str) -> http::Request<axum::body::Body> {
    http::Request::builder()
        .method("GET")
        .uri("/pubkey")
        .header("x-forwarded-for", ip)
        .body(axum::body::Body::empty())
        .unwrap()
}

fn status_of(response: &GrpcResponse) -> tonic::Status {
    tonic::Status::from_header_map(response.headers())
        .expect("rejection must be a trailers-only gRPC status response")
}

async fn call(
    service: &(impl Service<http::Request<()>, Response = GrpcResponse, Error = Infallible> + Clone),
    request: http::Request<()>,
) -> GrpcResponse {
    service.clone().oneshot(request).await.unwrap()
}

fn assert_admitted(response: &GrpcResponse) {
    assert!(
        response.headers().get("grpc-status").is_none(),
        "expected the call to reach the handler, got {:?}",
        response.headers()
    );
}

async fn admit_n(
    service: &(impl Service<http::Request<()>, Response = GrpcResponse, Error = Infallible> + Clone),
    path: &str,
    forwarded_for: Option<&str>,
    admissions: usize,
) {
    for _ in 0..admissions {
        assert_admitted(&call(service, grpc_request(path, forwarded_for)).await);
    }
}

#[tokio::test]
async fn rejection_carries_status_metadata_and_canonical_envelope() {
    let (service, handled) = grpc_service(RateLimitStore::new(RateLimitConfig::new(2, 1000)));
    let path = "/guardian.Guardian/GetPubkey";

    admit_n(&service, path, Some("1.2.3.4"), 2).await;
    let rejected = call(&service, grpc_request(path, Some("1.2.3.4"))).await;

    assert_eq!(handled.load(Ordering::SeqCst), 2, "handler must not run");

    let status = status_of(&rejected);
    assert_eq!(status.code(), Code::ResourceExhausted);
    assert_eq!(
        status
            .metadata()
            .get(crate::error::RETRY_AFTER_METADATA_KEY)
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );

    let envelope: serde_json::Value = serde_json::from_slice(status.details()).unwrap();
    assert_eq!(envelope["code"], "rate_limit_exceeded");
    assert!(envelope["message"].is_string());
    assert_eq!(envelope["meta"]["retryable"], serde_json::json!(true));
    assert_eq!(envelope["meta"]["retry_after_secs"], serde_json::json!(1));
}

#[tokio::test]
async fn sustained_rejection_hints_sixty_seconds() {
    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(100, 2)));
    let path = "/guardian.Guardian/GetPubkey";

    admit_n(&service, path, Some("2.3.4.5"), 2).await;
    let rejected = call(&service, grpc_request(path, Some("2.3.4.5"))).await;

    let status = status_of(&rejected);
    assert_eq!(status.code(), Code::ResourceExhausted);
    assert_eq!(
        status
            .metadata()
            .get(crate::error::RETRY_AFTER_METADATA_KEY)
            .and_then(|v| v.to_str().ok()),
        Some("60")
    );
    let envelope: serde_json::Value = serde_json::from_slice(status.details()).unwrap();
    assert_eq!(envelope["meta"]["retry_after_secs"], serde_json::json!(60));
}

#[tokio::test]
async fn burst_budgets_are_per_method() {
    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(2, 1000)));
    let ip = Some("3.4.5.6");

    let exhausted = "/guardian.Guardian/GetPubkey";
    admit_n(&service, exhausted, ip, 2).await;
    let rejected = call(&service, grpc_request(exhausted, ip)).await;
    assert_eq!(status_of(&rejected).code(), Code::ResourceExhausted);

    let other = call(&service, grpc_request("/guardian.Guardian/GetState", ip)).await;
    assert_admitted(&other);
}

#[tokio::test]
async fn kill_switch_disables_grpc_limiting() {
    let config = RateLimitConfig {
        enabled: false,
        burst_per_sec: 0,
        per_min: 0,
    };
    let (service, handled) = grpc_service(RateLimitStore::new(config));

    admit_n(
        &service,
        "/guardian.Guardian/GetPubkey",
        Some("4.5.6.7"),
        10,
    )
    .await;
    assert_eq!(handled.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn distinct_forwarded_clients_have_distinct_budgets() {
    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(2, 1000)));
    let path = "/guardian.Guardian/GetPubkey";

    admit_n(&service, path, Some("10.0.0.1"), 2).await;
    let rejected = call(&service, grpc_request(path, Some("10.0.0.1"))).await;
    assert_eq!(status_of(&rejected).code(), Code::ResourceExhausted);

    assert_admitted(&call(&service, grpc_request(path, Some("10.0.0.2"))).await);
}

#[tokio::test]
async fn distinct_peer_addresses_have_distinct_budgets() {
    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(2, 1000)));
    let path = "/guardian.Guardian/GetPubkey";

    let peer_request = |ip: &str| {
        let mut request = grpc_request(path, None);
        request
            .extensions_mut()
            .insert(tonic::transport::server::TcpConnectInfo {
                local_addr: None,
                remote_addr: Some(SocketAddr::new(ip.parse().unwrap(), 5555)),
            });
        request
    };

    assert_admitted(&call(&service, peer_request("198.51.100.1")).await);
    assert_admitted(&call(&service, peer_request("198.51.100.1")).await);
    let rejected = call(&service, peer_request("198.51.100.1")).await;
    assert_eq!(status_of(&rejected).code(), Code::ResourceExhausted);

    assert_admitted(&call(&service, peer_request("198.51.100.2")).await);
}

#[tokio::test]
async fn sustained_per_ip_binds_even_with_different_signer_metadata() {
    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(100, 2)));
    let path = "/guardian.Guardian/GetState";

    let signed_request = |pubkey: &str| {
        let mut request = grpc_request(path, Some("5.6.7.8"));
        request
            .headers_mut()
            .insert("x-pubkey", pubkey.parse().unwrap());
        request
    };

    assert_admitted(&call(&service, signed_request("pubkey-a")).await);
    assert_admitted(&call(&service, signed_request("pubkey-b")).await);
    let rejected = call(&service, signed_request("pubkey-c")).await;
    assert_eq!(status_of(&rejected).code(), Code::ResourceExhausted);
}

#[tokio::test]
async fn burst_budgets_are_per_transport_by_design() {
    let store = RateLimitStore::new(RateLimitConfig::new(2, 1000));
    let http_app = http_pubkey_app(store.clone());
    let (grpc, _) = grpc_service(store);

    for _ in 0..2 {
        let admitted = http_app.clone().oneshot(http_get("6.7.8.9")).await.unwrap();
        assert_ne!(admitted.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    let http_rejected = http_app.clone().oneshot(http_get("6.7.8.9")).await.unwrap();
    assert_eq!(http_rejected.status(), StatusCode::TOO_MANY_REQUESTS);

    let grpc_admitted = call(
        &grpc,
        grpc_request("/guardian.Guardian/GetPubkey", Some("6.7.8.9")),
    )
    .await;
    assert_admitted(&grpc_admitted);
}

#[tokio::test]
async fn sprayed_unknown_paths_collapse_into_one_burst_bucket() {
    let (service, handled) = grpc_service(RateLimitStore::new(RateLimitConfig::new(2, 1000)));
    let ip = Some("11.0.0.1");

    assert_admitted(&call(&service, grpc_request("/x.Spray/Path1", ip)).await);
    assert_admitted(&call(&service, grpc_request("/y.Spray/Path2", ip)).await);
    let rejected = call(&service, grpc_request("/z.Spray/Path3", ip)).await;
    assert_eq!(
        status_of(&rejected).code(),
        Code::ResourceExhausted,
        "distinct unserved paths must share one burst bucket"
    );
    assert_eq!(handled.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rejection_envelope_is_byte_identical_across_transports() {
    use axum::response::IntoResponse;

    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(1, 1000)));
    let path = "/guardian.Guardian/GetPubkey";
    call(&service, grpc_request(path, Some("12.0.0.1"))).await;
    let rejected = call(&service, grpc_request(path, Some("12.0.0.1"))).await;
    let grpc_details = status_of(&rejected).details().to_vec();

    let http_response = crate::error::GuardianError::RateLimitExceeded {
        retry_after_secs: 1,
        scope: "burst".to_string(),
    }
    .into_response();
    let http_body = axum::body::to_bytes(http_response.into_body(), usize::MAX)
        .await
        .unwrap();

    assert_eq!(grpc_details, http_body.to_vec());
}

#[tokio::test]
async fn spoofed_prefix_neither_mints_nor_escapes_a_budget() {
    let (service, _) = grpc_service(RateLimitStore::new(RateLimitConfig::new(2, 1000)));
    let path = "/guardian.Guardian/GetPubkey";

    admit_n(&service, path, Some("10.0.0.1"), 2).await;

    let forged_prefix = call(&service, grpc_request(path, Some("9.9.9.9, 10.0.0.1"))).await;
    assert_eq!(
        status_of(&forged_prefix).code(),
        Code::ResourceExhausted,
        "a forged prefix in front of the exhausted address must not mint a fresh budget"
    );

    let other_client = call(&service, grpc_request(path, Some("10.0.0.1, 10.0.0.2"))).await;
    assert_admitted(&other_client);
}

#[tokio::test]
async fn sustained_budget_is_shared_across_transports() {
    let store = RateLimitStore::new(RateLimitConfig::new(100, 2));
    let http_app = http_pubkey_app(store.clone());
    let (grpc, _) = grpc_service(store);

    for _ in 0..2 {
        let admitted = http_app.clone().oneshot(http_get("7.7.7.7")).await.unwrap();
        assert_ne!(admitted.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    let rejected = call(
        &grpc,
        grpc_request("/guardian.Guardian/GetPubkey", Some("7.7.7.7")),
    )
    .await;
    assert_eq!(status_of(&rejected).code(), Code::ResourceExhausted);

    let other_client = call(
        &grpc,
        grpc_request("/guardian.Guardian/GetPubkey", Some("7.7.7.8")),
    )
    .await;
    assert_admitted(&other_client);
}

#[test]
fn rejections_are_counted_per_transport() {
    let recorder = crate::metrics::recorder::build_recorder();
    let handle = recorder.handle();

    metrics::with_local_recorder(&recorder, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = RateLimitStore::new(RateLimitConfig::new(1, 1000));
            let http_app = http_pubkey_app(store.clone());
            let (grpc, _) = grpc_service(store);

            for _ in 0..2 {
                http_app.clone().oneshot(http_get("8.8.8.8")).await.unwrap();
            }
            for _ in 0..2 {
                call(
                    &grpc,
                    grpc_request("/guardian.Guardian/GetState", Some("8.8.8.8")),
                )
                .await;
            }
        });
    });

    let rendered = handle.render();
    assert!(
        rendered.contains(
            "guardian_rate_limit_rejections_total{limit_type=\"burst\",transport=\"http\"} 1"
        ),
        "missing http rejection series in:\n{rendered}"
    );
    assert!(
        rendered.contains(
            "guardian_rate_limit_rejections_total{limit_type=\"burst\",transport=\"grpc\"} 1"
        ),
        "missing grpc rejection series in:\n{rendered}"
    );
}
