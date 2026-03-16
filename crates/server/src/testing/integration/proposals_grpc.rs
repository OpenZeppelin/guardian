use crate::testing::helpers::{
    TestSigner, create_grpc_service, create_miden_falcon_rpo_auth, create_signed_request_with_auth,
    create_test_app_state, load_fixture_account_grpc as load_fixture_account, load_fixture_delta,
};
use tonic::Request;

use crate::api::grpc::state_manager::state_manager_server::StateManager;
use crate::api::grpc::state_manager::{
    ConfigureRequest, GetDeltaProposalRequest, GetDeltaProposalsRequest, ProposalSignature,
    PushDeltaProposalRequest, SignDeltaProposalRequest,
};

#[tokio::test]
async fn test_grpc_push_delta_proposal_success() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            signer.commitment_hex.clone(),
        ])),
        initial_state,
    };

    let configure_response = service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &signer,
        ))
        .await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Push delta proposal
    let delta_1 = load_fixture_delta(1);
    let delta_payload = serde_json::json!({
        "tx_summary": delta_1["delta_payload"],
        "signatures": [],
        "metadata": {
            "proposal_type": "change_threshold",
            "target_threshold": 1,
            "signer_commitments": [signer.commitment_hex.clone()]
        }
    });

    let push_proposal_req = PushDeltaProposalRequest {
        account_id: account_id_hex.clone(),
        nonce: 1,
        delta_payload: serde_json::to_string(&delta_payload).unwrap(),
    };

    let request = create_signed_request_with_auth(push_proposal_req, &account_id_hex, &signer);
    let push_response = service.push_delta_proposal(request).await;

    assert!(
        push_response.is_ok(),
        "Push delta proposal should succeed with valid auth"
    );
    let push_response = push_response.unwrap().into_inner();
    assert!(
        push_response.success,
        "Push response should be successful: {}",
        push_response.message
    );
    assert!(push_response.delta.is_some(), "Should return delta");
    assert!(
        !push_response.commitment.is_empty(),
        "Should return commitment"
    );
}

#[tokio::test]
async fn test_grpc_get_delta_proposals_empty() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            signer.commitment_hex.clone(),
        ])),
        initial_state,
    };

    service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .unwrap();

    // Get delta proposals when there are none
    let get_proposals_req = GetDeltaProposalsRequest {
        account_id: account_id_hex.clone(),
    };

    let request = create_signed_request_with_auth(get_proposals_req, &account_id_hex, &signer);
    let get_response = service.get_delta_proposals(request).await;

    assert!(get_response.is_ok(), "Get delta proposals should succeed");
    let get_response = get_response.unwrap().into_inner();
    assert!(get_response.success, "Get response should be successful");
    assert_eq!(get_response.proposals.len(), 0, "Should return empty list");
}

#[tokio::test]
async fn test_grpc_get_delta_proposals_with_proposals() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            signer.commitment_hex.clone(),
        ])),
        initial_state,
    };

    service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .unwrap();

    // Push a delta proposal
    let delta_1 = load_fixture_delta(1);
    let delta_payload = serde_json::json!({
        "tx_summary": delta_1["delta_payload"],
        "signatures": [],
        "metadata": {
            "proposal_type": "change_threshold",
            "target_threshold": 1,
            "signer_commitments": [signer.commitment_hex.clone()]
        }
    });

    let push_proposal_req = PushDeltaProposalRequest {
        account_id: account_id_hex.clone(),
        nonce: 1,
        delta_payload: serde_json::to_string(&delta_payload).unwrap(),
    };

    service
        .push_delta_proposal(create_signed_request_with_auth(
            push_proposal_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .unwrap();

    // Get delta proposals - need fresh signature
    let get_proposals_req = GetDeltaProposalsRequest {
        account_id: account_id_hex.clone(),
    };

    let request = create_signed_request_with_auth(get_proposals_req, &account_id_hex, &signer);
    let get_response = service.get_delta_proposals(request).await;

    assert!(get_response.is_ok());
    let get_response = get_response.unwrap().into_inner();
    assert!(get_response.success);
    assert_eq!(
        get_response.proposals.len(),
        1,
        "Should return one proposal"
    );
    assert_eq!(get_response.proposals[0].nonce, 1);
}

#[tokio::test]
async fn test_grpc_get_delta_proposal_by_commitment() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            signer.commitment_hex.clone(),
        ])),
        initial_state,
    };

    service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .unwrap();

    let delta_1 = load_fixture_delta(1);
    let delta_payload = serde_json::json!({
        "tx_summary": delta_1["delta_payload"],
        "signatures": [],
        "metadata": { "proposal_type": "change_threshold", "target_threshold": 2, "signer_commitments": [] }
    });

    let push_proposal_req = PushDeltaProposalRequest {
        account_id: account_id_hex.clone(),
        nonce: 1,
        delta_payload: serde_json::to_string(&delta_payload).unwrap(),
    };

    let push_response = service
        .push_delta_proposal(create_signed_request_with_auth(
            push_proposal_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(push_response.success);
    let commitment = push_response.commitment;

    let get_proposal_req = GetDeltaProposalRequest {
        account_id: account_id_hex.clone(),
        commitment,
    };
    let request = create_signed_request_with_auth(get_proposal_req, &account_id_hex, &signer);
    let get_response = service.get_delta_proposal(request).await;

    assert!(get_response.is_ok());
    let get_response = get_response.unwrap().into_inner();
    assert!(get_response.success);
    assert!(get_response.proposal.is_some());
    assert_eq!(get_response.proposal.unwrap().nonce, 1);
}

#[tokio::test]
async fn test_grpc_sign_delta_proposal_not_found() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            signer.commitment_hex.clone(),
        ])),
        initial_state,
    };

    service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .unwrap();

    // Try to sign nonexistent proposal - need fresh signature
    let dummy_sig = format!("0x{}", "a".repeat(666));
    let sign_proposal_req = SignDeltaProposalRequest {
        account_id: account_id_hex.clone(),
        commitment: "nonexistent_proposal".to_string(),
        signature: Some(ProposalSignature {
            scheme: "falcon".to_string(),
            signature: dummy_sig,
            public_key: None,
        }),
    };

    let request = create_signed_request_with_auth(sign_proposal_req, &account_id_hex, &signer);
    let sign_response = service.sign_delta_proposal(request).await;

    assert!(sign_response.is_ok(), "gRPC call should succeed");
    let sign_response = sign_response.unwrap().into_inner();
    assert!(
        !sign_response.success,
        "Sign should fail for nonexistent proposal"
    );
    assert!(
        sign_response.message.contains("not found") || sign_response.message.contains("Proposal"),
        "Error message should mention proposal not found"
    );
}

#[tokio::test]
async fn test_grpc_push_delta_proposal_unauthorized() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();

    // Generate two different key pairs
    let authorized_signer = TestSigner::new();
    let unauthorized_signer = TestSigner::new();

    // Configure account with ONLY the authorized commitment
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            authorized_signer.commitment_hex.clone(),
        ])),
        initial_state,
    };

    service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &authorized_signer,
        ))
        .await
        .unwrap();

    // Try to push proposal with UNAUTHORIZED key
    let delta_1 = load_fixture_delta(1);
    let delta_payload = serde_json::json!({
        "tx_summary": delta_1["delta_payload"],
        "signatures": [],
        "metadata": {
            "proposal_type": "change_threshold",
            "target_threshold": 1,
            "signer_commitments": [unauthorized_signer.commitment_hex.clone()]
        }
    });

    let push_proposal_req = PushDeltaProposalRequest {
        account_id: account_id_hex.clone(),
        nonce: 1,
        delta_payload: serde_json::to_string(&delta_payload).unwrap(),
    };

    let request =
        create_signed_request_with_auth(push_proposal_req, &account_id_hex, &unauthorized_signer);
    let push_response = service.push_delta_proposal(request).await;

    assert!(push_response.is_ok(), "gRPC call should succeed");
    let push_response = push_response.unwrap().into_inner();
    assert!(
        !push_response.success,
        "Push should fail with unauthorized cosigner"
    );
    assert!(
        push_response.message.contains("not authorized"),
        "Error message should mention authorization"
    );
}

#[tokio::test]
async fn test_grpc_get_pubkey() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let get_pubkey_req = crate::api::grpc::state_manager::GetPubkeyRequest { scheme: None };

    let request = Request::new(get_pubkey_req);
    let response = service.get_pubkey(request).await;

    assert!(response.is_ok(), "Get pubkey should succeed");
    let response = response.unwrap().into_inner();
    assert!(!response.pubkey.is_empty(), "Should return pubkey");
    assert!(response.pubkey.starts_with("0x"), "Pubkey should be hex");
}
