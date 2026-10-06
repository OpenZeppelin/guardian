use crate::testing::helpers::{
    TestSigner, create_router, create_test_app_state, load_fixture_account, load_fixture_delta,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::FromJson;
use miden_protocol::Word;
use miden_protocol::transaction::TransactionSummary;
use serde_json::json;
use tower::Service;

#[tokio::test]
async fn test_push_delta_proposal_success() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex.clone()]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);

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
    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex.clone(),
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [signer.commitment_hex.clone()]
            }
        }
    });
    let (signature_hex_2, timestamp_2) = signer.sign_json_payload(&account_id_hex, &proposal_body);

    let push_proposal_request = Request::builder()
        .uri("/delta/proposal")
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

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);

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
    let query_payload = json!({ "account_id": account_id_hex.clone() });
    let (signature_hex_2, timestamp_2) = signer.sign_json_payload(&account_id_hex, &query_payload);
    let get_proposals_request = Request::builder()
        .uri(format!("/delta/proposal?account_id={}", account_id_hex))
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

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);

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
    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex.clone(),
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [signer.commitment_hex.clone()]
            }
        }
    });
    let (signature_hex_2, timestamp_2) = signer.sign_json_payload(&account_id_hex, &proposal_body);

    let push_proposal_request = Request::builder()
        .uri("/delta/proposal")
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
    let query_payload = json!({ "account_id": account_id_hex.clone() });
    let (signature_hex_3, timestamp_3) = signer.sign_json_payload(&account_id_hex, &query_payload);
    let get_proposals_request = Request::builder()
        .uri(format!("/delta/proposal?account_id={}", account_id_hex))
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
async fn test_get_delta_proposal_by_commitment() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);

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

    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex.clone(),
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": [],
            "metadata": { "proposal_type": "change_threshold", "target_threshold": 2, "signer_commitments": [] }
        }
    });
    let (signature_hex_2, timestamp_2) = signer.sign_json_payload(&account_id_hex, &proposal_body);

    let push_proposal_request = Request::builder()
        .uri("/delta/proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex_2)
        .header("x-timestamp", timestamp_2.to_string())
        .body(Body::from(serde_json::to_string(&proposal_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let push_response = app_clone.call(push_proposal_request).await.unwrap();
    assert_eq!(push_response.status(), StatusCode::OK);
    let push_body = to_bytes(push_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let push_json: serde_json::Value = serde_json::from_slice(&push_body).unwrap();
    let commitment = push_json["commitment"].as_str().unwrap().to_string();

    let query_payload = json!({
        "account_id": account_id_hex.clone(),
        "commitment": commitment.clone(),
    });
    let (signature_hex_3, timestamp_3) = signer.sign_json_payload(&account_id_hex, &query_payload);
    let get_proposal_request = Request::builder()
        .uri(format!(
            "/delta/proposal/single?account_id={}&commitment={}",
            account_id_hex, commitment
        ))
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex_3)
        .header("x-timestamp", timestamp_3.to_string())
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let get_response = app_clone.call(get_proposal_request).await.unwrap();

    assert_eq!(get_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_sign_delta_proposal_not_found() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (signer_signature, signer_timestamp) =
        signer.sign_json_payload(&account_id_hex, &configure_body);

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
    let dummy_sig = format!("0x{}", "a".repeat(666));
    let sign_body = json!({
        "account_id": account_id_hex.clone(),
        "commitment": "nonexistent_proposal",
        "signature": {
            "scheme": "falcon",
            "signature": dummy_sig
        }
    });
    let (signer_signature_2, signer_timestamp_2) =
        signer.sign_json_payload(&account_id_hex, &sign_body);

    let sign_proposal_request = Request::builder()
        .uri("/delta/proposal")
        .method("PUT")
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
    let unauthorized_signer = TestSigner::new();

    // Configure account with only authorized commitment
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [authorized_signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (authorized_sig, authorized_ts) =
        authorized_signer.sign_json_payload(&account_id_hex, &configure_body);

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
        "account_id": account_id_hex.clone(),
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [unauthorized_signer.commitment_hex.clone()]
            }
        }
    });
    let (unauthorized_sig, unauthorized_ts) =
        unauthorized_signer.sign_json_payload(&account_id_hex, &proposal_body);

    let push_proposal_request = Request::builder()
        .uri("/delta/proposal")
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
        StatusCode::UNAUTHORIZED,
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

/// Configures the fixture account with `signer` as its only cosigner and
/// returns the account id plus the commitment the fixture proposal signs.
async fn configure_fixture_account(app: &axum::Router, signer: &TestSigner) -> (String, Word) {
    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex.clone()]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);
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

    let summary = TransactionSummary::from_json(&load_fixture_delta(1)["delta_payload"]).unwrap();
    (account_id_hex, summary.to_commitment())
}

fn fixture_proposal_body(
    account_id_hex: &str,
    signer: &TestSigner,
    signatures: serde_json::Value,
) -> serde_json::Value {
    json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": {
            "tx_summary": load_fixture_delta(1)["delta_payload"],
            "signatures": signatures,
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [signer.commitment_hex.clone()]
            }
        }
    })
}

/// Builds a `/delta/proposal` request signed by `signer`.
fn proposal_request(
    method: &str,
    signer: &TestSigner,
    account_id_hex: &str,
    body: &serde_json::Value,
) -> Request<Body> {
    let (signature_hex, timestamp) = signer.sign_json_payload(account_id_hex, body);
    Request::builder()
        .uri("/delta/proposal")
        .method(method)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(serde_json::to_string(body).unwrap()))
        .unwrap()
}

async fn json_body(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn test_push_delta_proposal_rejects_unverified_signature() {
    let state = create_test_app_state().await;
    let app = create_router(state);
    let signer = TestSigner::new();
    let (account_id_hex, _summary_commitment) = configure_fixture_account(&app, &signer).await;

    // The proposer's own slot, but signed over another message.
    let body = fixture_proposal_body(
        &account_id_hex,
        &signer,
        json!([{
            "signer_id": signer.commitment_hex,
            "signature": { "scheme": "falcon", "signature": signer.sign_word(Word::default()) }
        }]),
    );
    let mut app_clone = app.clone();
    let response = app_clone
        .call(proposal_request("POST", &signer, &account_id_hex, &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["code"],
        "invalid_proposal_signature"
    );
}

#[tokio::test]
async fn test_sign_delta_proposal_verifies_signature() {
    let state = create_test_app_state().await;
    let app = create_router(state);
    let signer = TestSigner::new();
    let (account_id_hex, summary_commitment) = configure_fixture_account(&app, &signer).await;

    let body = fixture_proposal_body(&account_id_hex, &signer, json!([]));
    let mut app_clone = app.clone();
    let response = app_clone
        .call(proposal_request("POST", &signer, &account_id_hex, &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let commitment = json_body(response).await["commitment"]
        .as_str()
        .unwrap()
        .to_string();

    let sign_body = |signature: String| {
        json!({
            "account_id": account_id_hex,
            "commitment": commitment,
            "signature": { "scheme": "falcon", "signature": signature }
        })
    };

    let wrong = sign_body(signer.sign_word(Word::default()));
    let mut app_clone = app.clone();
    let response = app_clone
        .call(proposal_request("PUT", &signer, &account_id_hex, &wrong))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["code"],
        "invalid_proposal_signature"
    );

    let valid = sign_body(signer.sign_word(summary_commitment));
    let mut app_clone = app.clone();
    let response = app_clone
        .call(proposal_request("PUT", &signer, &account_id_hex, &valid))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
