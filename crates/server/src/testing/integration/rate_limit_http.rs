use crate::middleware::{RateLimitConfig, RateLimitLayer};
use crate::testing::helpers::{create_router, create_test_app_state};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn build_get_request(uri: &str, ip: &str, pubkey: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-forwarded-for", ip);

    if let Some(pubkey) = pubkey {
        builder = builder
            .header("x-pubkey", pubkey)
            .header("x-signature", "sig")
            .header("x-timestamp", "1");
    }

    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn test_rate_limit_burst_per_endpoint() {
    let state = create_test_app_state().await;
    let app = create_router(state).layer(RateLimitLayer::new(RateLimitConfig::new(2, 1000)));

    let ip = "1.2.3.4";
    let req1 = build_get_request("/pubkey", ip, None);
    let req2 = build_get_request("/pubkey", ip, None);
    let req3 = build_get_request("/pubkey", ip, None);

    let res1 = app.clone().oneshot(req1).await.unwrap();
    let res2 = app.clone().oneshot(req2).await.unwrap();
    let res3 = app.clone().oneshot(req3).await.unwrap();

    assert_ne!(res1.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(res2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res3.status(), StatusCode::TOO_MANY_REQUESTS);

    let res_other = app
        .clone()
        .oneshot(build_get_request(
            "/get_state?account_id=0x01",
            ip,
            Some("pubkey-1"),
        ))
        .await
        .unwrap();

    assert_ne!(res_other.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_rate_limit_sustained_per_ip_even_with_different_signers() {
    let state = create_test_app_state().await;
    let app = create_router(state).layer(RateLimitLayer::new(RateLimitConfig::new(100, 2)));

    let ip = "5.6.7.8";
    let pubkeys = ["pubkey-a", "pubkey-b", "pubkey-c"];

    let res1 = app
        .clone()
        .oneshot(build_get_request(
            "/get_state?account_id=0x01",
            ip,
            Some(pubkeys[0]),
        ))
        .await
        .unwrap();
    let res2 = app
        .clone()
        .oneshot(build_get_request(
            "/get_state?account_id=0x01",
            ip,
            Some(pubkeys[1]),
        ))
        .await
        .unwrap();
    let res3 = app
        .clone()
        .oneshot(build_get_request(
            "/get_state?account_id=0x01",
            ip,
            Some(pubkeys[2]),
        ))
        .await
        .unwrap();

    assert_ne!(res1.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(res2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res3.status(), StatusCode::TOO_MANY_REQUESTS);
}
