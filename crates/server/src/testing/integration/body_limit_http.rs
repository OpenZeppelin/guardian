use crate::builder::handle::{HttpRouterConfig, build_http_router};
use crate::middleware::BodyLimitConfig;
use crate::testing::helpers::{create_test_app_state, test_router_config};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

fn build_configure_request(body: String, pubkey: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/configure")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", pubkey)
        .header("x-signature", "sig")
        .header("x-timestamp", "1")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn test_body_limit_rejects_large_payload() {
    let state = create_test_app_state().await;
    let app = build_http_router(
        state,
        HttpRouterConfig {
            body_limit_config: Some(BodyLimitConfig { max_bytes: 100 }),
            ..test_router_config()
        },
    );

    let large_data = "x".repeat(200);
    let configure_body = json!({
        "account_id": "0x01",
        "auth": { "MidenFalconRpo": { "cosigner_commitments": [] } },
        "initial_state": { "data": large_data }
    });

    let req = build_configure_request(serde_json::to_string(&configure_body).unwrap(), "pk-1");
    let res = app.clone().oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn test_body_limit_allows_small_payload() {
    let state = create_test_app_state().await;
    let app = build_http_router(
        state,
        HttpRouterConfig {
            body_limit_config: Some(BodyLimitConfig { max_bytes: 1024 }),
            ..test_router_config()
        },
    );

    let configure_body = json!({
        "account_id": "0x01",
        "auth": { "MidenFalconRpo": { "cosigner_commitments": [] } },
        "initial_state": { "data": "small" }
    });

    let req = build_configure_request(serde_json::to_string(&configure_body).unwrap(), "pk-2");
    let res = app.clone().oneshot(req).await.unwrap();

    assert_ne!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
