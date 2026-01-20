use crate::testing::helpers::{
    create_router, create_test_app_state, generate_falcon_signature, load_fixture_account,
    load_fixture_delta,
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
    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();
    assert_eq!(configure_response.status(), StatusCode::OK);

    // Push delta proposal
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
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
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
    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();
    assert_eq!(configure_response.status(), StatusCode::OK);

    // Get delta proposals
    let get_proposals_request = Request::builder()
        .uri(format!(
            "/get_delta_proposals?account_id={}",
            account_id_hex
        ))
        .method("GET")
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
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
    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [commitment_hex]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(configure_request).await.unwrap();

    // Push first proposal
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
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
        .body(Body::from(serde_json::to_string(&proposal_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(push_proposal_request).await.unwrap();

    // Get delta proposals
    let get_proposals_request = Request::builder()
        .uri(format!(
            "/get_delta_proposals?account_id={}",
            account_id_hex
        ))
        .method("GET")
        .header("x-pubkey", &pubkey_hex)
        .header("x-signature", &signature_hex)
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
    let (signer_pubkey, signer_commitment, signer_signature) =
        generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer_commitment]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer_pubkey)
        .header("x-signature", &signer_signature)
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    app_clone.call(configure_request).await.unwrap();

    // Try to sign nonexistent proposal
    let dummy_sig = format!("0x{}", "a".repeat(666));
    let sign_body = json!({
        "account_id": account_id_hex,
        "commitment": "nonexistent_proposal",
        "signature": {
            "scheme": "falcon",
            "signature": dummy_sig
        }
    });

    let sign_proposal_request = Request::builder()
        .uri("/sign_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer_pubkey)
        .header("x-signature", &signer_signature)
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
    let (authorized_pubkey, authorized_commitment, authorized_sig) =
        generate_falcon_signature(&account_id_hex);
    let (unauthorized_pubkey, _, unauthorized_sig) = generate_falcon_signature(&account_id_hex);

    // Configure account with only authorized commitment
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [authorized_commitment]
            }
        },
        "initial_state": initial_state
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &authorized_pubkey)
        .header("x-signature", &authorized_sig)
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
        .header("x-pubkey", &unauthorized_pubkey)
        .header("x-signature", &unauthorized_sig)
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
