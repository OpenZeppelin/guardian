use crate::testing::helpers::{
    TestSigner, create_router, create_test_app_state, load_fixture_account, load_fixture_delta,
};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tower::Service;

#[tokio::test]
async fn test_push_delta_proposal_success() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();
    let (signature_hex, timestamp) = signer.sign(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex.clone()]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();
    assert_eq!(configure_response.status(), StatusCode::OK);

    // Push delta proposal
    let (signature_hex_2, timestamp_2) = signer.sign(&account_id_hex);
    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": []
        }
    });

    let push_proposal_request = Request::builder()
        .uri("/push_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex_2)
        .header("x-timestamp", timestamp_2.to_string())
        .body(Body::from(serde_json::to_string(&proposal_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let push_response = app_clone.call(push_proposal_request).await.unwrap();

    assert_eq!(
        push_response.status(),
        StatusCode::OK,
        "Push delta proposal should succeed"
    );
}

#[tokio::test]
async fn test_get_delta_proposals_empty() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();
    let (signature_hex, timestamp) = signer.sign(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();
    assert_eq!(configure_response.status(), StatusCode::OK);

    // Get delta proposals - need fresh signature for new request
    let (signature_hex_2, timestamp_2) = signer.sign(&account_id_hex);
    let get_proposals_request = Request::builder()
        .uri(format!(
            "/get_delta_proposals?account_id={}",
            account_id_hex
        ))
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex_2)
        .header("x-timestamp", timestamp_2.to_string())
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let get_response = app_clone.call(get_proposals_request).await.unwrap();

    assert_eq!(get_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_delta_proposals_with_proposals() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();
    let (signature_hex, timestamp) = signer.sign(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(configure_request).await.unwrap();

    // Push first proposal - need fresh signature
    let (signature_hex_2, timestamp_2) = signer.sign(&account_id_hex);
    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": []
        }
    });

    let push_proposal_request = Request::builder()
        .uri("/push_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex_2)
        .header("x-timestamp", timestamp_2.to_string())
        .body(Body::from(serde_json::to_string(&proposal_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(push_proposal_request).await.unwrap();

    // Get delta proposals - need fresh signature
    let (signature_hex_3, timestamp_3) = signer.sign(&account_id_hex);
    let get_proposals_request = Request::builder()
        .uri(format!(
            "/get_delta_proposals?account_id={}",
            account_id_hex
        ))
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex_3)
        .header("x-timestamp", timestamp_3.to_string())
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let get_response = app_clone.call(get_proposals_request).await.unwrap();

    assert_eq!(get_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_sign_delta_proposal_not_found() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();
    let (signer_signature, signer_timestamp) = signer.sign(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signer_signature)
        .header("x-timestamp", signer_timestamp.to_string())
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(configure_request).await.unwrap();

    // Try to sign nonexistent proposal - need fresh signature
    let (signer_signature_2, signer_timestamp_2) = signer.sign(&account_id_hex);
    let dummy_sig = format!("0x{}", "a".repeat(666));
    let sign_body = json!({
        "account_id": account_id_hex,
        "commitment": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "signature": {
            "scheme": "falcon",
            "signature": dummy_sig
        }
    });

    let sign_proposal_request = Request::builder()
        .uri("/sign_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signer_signature_2)
        .header("x-timestamp", signer_timestamp_2.to_string())
        .body(Body::from(serde_json::to_string(&sign_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let sign_response = app_clone.call(sign_proposal_request).await.unwrap();

    assert_eq!(
        sign_response.status(),
        StatusCode::BAD_REQUEST,
        "Sign nonexistent proposal should fail"
    );
}

#[tokio::test]
async fn test_push_delta_proposal_unauthorized() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let authorized_signer = TestSigner::new();
    let (authorized_sig, authorized_ts) = authorized_signer.sign(&account_id_hex);
    let unauthorized_signer = TestSigner::new();
    let (unauthorized_sig, unauthorized_ts) = unauthorized_signer.sign(&account_id_hex);

    // Configure account with only authorized commitment
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [authorized_signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &authorized_signer.pubkey_hex)
        .header("x-signature", &authorized_sig)
        .header("x-timestamp", authorized_ts.to_string())
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(configure_request).await.unwrap();

    // Try to push proposal with unauthorized credentials
    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": []
        }
    });

    let push_proposal_request = Request::builder()
        .uri("/push_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &unauthorized_signer.pubkey_hex)
        .header("x-signature", &unauthorized_sig)
        .header("x-timestamp", unauthorized_ts.to_string())
        .body(Body::from(serde_json::to_string(&proposal_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let push_response = app_clone.call(push_proposal_request).await.unwrap();

    assert_eq!(
        push_response.status(),
        StatusCode::BAD_REQUEST,
        "Unauthorized push should fail"
    );
}

#[tokio::test]
async fn test_get_pubkey() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let get_pubkey_request = Request::builder()
        .uri("/pubkey")
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(get_pubkey_request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
