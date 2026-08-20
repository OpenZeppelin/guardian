use crate::proto::auth_config::AuthType;
use crate::testing::mocks::{
    MockGuardianService, create_mock_account_state, create_mock_delta, start_mock_server,
};
use crate::{
    AccountRef, AuthConfig, ClientError, ConfigureResponse, FalconKeyStore,
    GetAccountByKeyCommitmentResponse, GetDeltaProposalResponse, GetDeltaProposalsResponse,
    GetDeltaResponse, GetDeltaSinceResponse, GetStateResponse, GetTransactionHistoryResponse,
    GuardianClient, HistoryEntry, HistoryNote, HistoryNoteAsset, PushDeltaProposalResponse,
    PushDeltaResponse, SignDeltaProposalResponse, Signer,
};
use guardian_shared::ProposalSignature as JsonProposalSignature;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use std::sync::Arc;
use tonic::Status;

fn create_test_account_id() -> AccountId {
    AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap()
}

fn create_test_signer() -> Arc<dyn Signer> {
    Arc::new(FalconKeyStore::new(SecretKey::new()))
}

fn guardian_auth_status(code: &str, retryable: bool) -> Status {
    let details = serde_json::json!({
        "code": code,
        "message": "test",
        "meta": { "retryable": retryable }
    })
    .to_string()
    .into_bytes();
    Status::with_details(tonic::Code::Unauthenticated, "test", details.into())
}

#[tokio::test]
async fn test_get_pubkey_success() {
    let service = MockGuardianService::default().with_get_pubkey(Ok("test_pubkey_123".to_string()));

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = GuardianClient::connect(endpoint).await.unwrap();

    let result = client.get_pubkey(None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().0, "test_pubkey_123");
}

#[tokio::test]
async fn test_get_pubkey_error() {
    let service =
        MockGuardianService::default().with_get_pubkey(Err(Status::internal("Server error")));

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = GuardianClient::connect(endpoint).await.unwrap();

    let result = client.get_pubkey(None).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::Status(_) => {}
        e => panic!("Expected Status error, got: {:?}", e),
    }
}

#[tokio::test]
async fn rate_limit_rejection_round_trips_retry_classification() {
    // The exact Status shape the server's gRPC rate-limit layer produces,
    // sent over a real channel so the retry-after metadata and the details
    // envelope survive actual HTTP/2 encoding.
    let details = serde_json::json!({
        "code": "rate_limit_exceeded",
        "message": "Too many requests — please try again shortly.",
        "meta": { "retryable": true, "retry_after_secs": 1 }
    })
    .to_string()
    .into_bytes();
    let mut status = Status::with_details(
        tonic::Code::ResourceExhausted,
        "Too many requests — please try again shortly.",
        details.into(),
    );
    status.metadata_mut().insert("retry-after", 1.into());

    let service = MockGuardianService::default().with_get_pubkey(Err(status));
    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = GuardianClient::connect(endpoint).await.unwrap();

    let err = client.get_pubkey(None).await.unwrap_err();
    assert_eq!(err.guardian_code().as_deref(), Some("rate_limit_exceeded"));
    assert!(err.is_retryable());
    assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(1)));
}

#[tokio::test]
async fn test_configure_success() {
    let service = MockGuardianService::default().with_configure(Ok(ConfigureResponse {
        success: true,
        message: "Account configured".to_string(),
        ack_pubkey: "test_pubkey_123".to_string(),
        ack_commitment: String::new(),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();
    let initial_state = serde_json::json!({"balance": 1000});
    let auth_config = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(crate::proto::MidenFalconRpoAuth {
            cosigner_commitments: vec!["0xabc".to_string()],
        })),
    };

    let result = client
        .configure(&account_id, auth_config, initial_state)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.message, "Account configured");
    assert_eq!(response.ack_pubkey, "test_pubkey_123");
}

#[tokio::test]
async fn test_configure_server_error() {
    let service = MockGuardianService::default().with_configure(Ok(ConfigureResponse {
        success: false,
        message: "Account already exists".to_string(),
        ack_pubkey: String::new(),
        ack_commitment: String::new(),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();
    let initial_state = serde_json::json!({"balance": 1000});
    let auth_config = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(crate::proto::MidenFalconRpoAuth {
            cosigner_commitments: vec!["0xabc".to_string()],
        })),
    };

    let result = client
        .configure(&account_id, auth_config, initial_state)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert_eq!(msg, "Account already exists");
        }
        e => panic!("Expected ServerError, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_push_delta_proposal_success() {
    let mock_delta = create_mock_delta();
    let service =
        MockGuardianService::default().with_push_delta_proposal(Ok(PushDeltaProposalResponse {
            success: true,
            message: String::new(),
            commitment: "proposal_commitment_123".to_string(),
            delta: Some(mock_delta.clone()),
        }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();
    let delta_payload = serde_json::json!({"tx_summary": {}, "signatures": []});

    let result = client
        .push_delta_proposal(&account_id, 1, delta_payload)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.commitment, "proposal_commitment_123");
    assert!(response.delta.is_some());
}

#[tokio::test]
async fn test_get_delta_proposals_success() {
    let mock_delta1 = create_mock_delta();
    let mut mock_delta2 = create_mock_delta();
    mock_delta2.nonce = 2;

    let service =
        MockGuardianService::default().with_get_delta_proposals(Ok(GetDeltaProposalsResponse {
            success: true,
            message: String::new(),
            proposals: vec![mock_delta1, mock_delta2],
        }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_delta_proposals(&account_id).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.proposals.len(), 2);
}

#[tokio::test]
async fn test_get_delta_proposals_empty() {
    let service =
        MockGuardianService::default().with_get_delta_proposals(Ok(GetDeltaProposalsResponse {
            success: true,
            message: String::new(),
            proposals: vec![],
        }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_delta_proposals(&account_id).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.proposals.len(), 0);
}

#[tokio::test]
async fn test_get_delta_proposal_success() {
    let mock_delta = create_mock_delta();
    let service =
        MockGuardianService::default().with_get_delta_proposal(Ok(GetDeltaProposalResponse {
            success: true,
            message: String::new(),
            proposal: Some(mock_delta),
        }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client
        .get_delta_proposal(&account_id, "proposal_commitment_123")
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.proposal.is_some());
}

#[tokio::test]
async fn test_sign_delta_proposal_success() {
    let mock_delta = create_mock_delta();
    let service =
        MockGuardianService::default().with_sign_delta_proposal(Ok(SignDeltaProposalResponse {
            success: true,
            message: "Signature added".to_string(),
            delta: Some(mock_delta),
        }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();
    let signature = JsonProposalSignature::Falcon {
        signature: "0xabcd".to_string(),
    };

    let result = client
        .sign_delta_proposal(&account_id, "commitment_123", signature)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.message, "Signature added");
    assert!(response.delta.is_some());
}

#[tokio::test]
async fn test_push_delta_success() {
    let mock_delta = create_mock_delta();
    let service = MockGuardianService::default().with_push_delta(Ok(PushDeltaResponse {
        success: true,
        message: "Delta pushed".to_string(),
        delta: Some(mock_delta),
        ack_sig: Some("0xsig".to_string()),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();
    let delta_payload = serde_json::json!({"updates": []});

    let result = client
        .push_delta(&account_id, 1, "0x123", delta_payload)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.message, "Delta pushed");
    assert!(response.delta.is_some());
}

#[tokio::test]
async fn test_get_delta_success() {
    let mock_delta = create_mock_delta();
    let service = MockGuardianService::default().with_get_delta(Ok(GetDeltaResponse {
        success: true,
        message: String::new(),
        delta: Some(mock_delta),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_delta(&account_id, 1).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.delta.is_some());
    assert_eq!(response.delta.unwrap().nonce, 1);
}

#[tokio::test]
async fn test_get_delta_not_found() {
    let service = MockGuardianService::default().with_get_delta(Ok(GetDeltaResponse {
        success: false,
        message: "Delta not found".to_string(),
        delta: None,
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_delta(&account_id, 999).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert_eq!(msg, "Delta not found");
        }
        e => panic!("Expected ServerError, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_get_delta_since_success() {
    let mock_delta = create_mock_delta();
    let service = MockGuardianService::default().with_get_delta_since(Ok(GetDeltaSinceResponse {
        success: true,
        message: String::new(),
        merged_delta: Some(mock_delta),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_delta_since(&account_id, 1).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.merged_delta.is_some());
}

#[tokio::test]
async fn test_get_transaction_history_success() {
    let service = MockGuardianService::default().with_get_transaction_history(Ok(
        GetTransactionHistoryResponse {
            success: true,
            message: String::new(),
            entries: vec![HistoryEntry {
                nonce: 3,
                timestamp: "2026-08-01T12:00:03Z".to_string(),
                new_commitment: Some("0xnew0003".to_string()),
                input_notes: vec![],
                output_notes: vec![HistoryNote {
                    note_id: "0xnote".to_string(),
                    tag: "p2id".to_string(),
                    assets: vec![HistoryNoteAsset {
                        asset_id: "0xfaucet".to_string(),
                        kind: "fungible".to_string(),
                        amount: Some("100".to_string()),
                    }],
                    sender: None,
                    recipient: Some("0xrecipient".to_string()),
                }],
                decode_warnings: vec![],
            }],
            next_cursor: Some("cursor-token".to_string()),
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let response = client
        .get_transaction_history(&account_id, Some(10), None)
        .await
        .expect("get_transaction_history should succeed");
    assert!(response.success);
    assert_eq!(response.entries.len(), 1);
    assert_eq!(response.entries[0].nonce, 3);
    assert_eq!(response.entries[0].output_notes[0].tag, "p2id");
    assert_eq!(response.next_cursor.as_deref(), Some("cursor-token"));
}

#[tokio::test]
async fn test_get_state_success() {
    let mock_state = create_mock_account_state();
    let service = MockGuardianService::default().with_get_state(Ok(GetStateResponse {
        success: true,
        message: String::new(),
        state: Some(mock_state),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_state(&account_id).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.state.is_some());
    assert!(response.state.unwrap().state_json.contains("balance"));
}

#[tokio::test]
async fn replay_rejections_are_retried_with_fresh_timestamp_and_signature_each_attempt() {
    // Two queued replay errors force the client to use its entire retry
    // budget; the mock then serves its default success, so a passing result
    // proves both retries happened.
    let service = MockGuardianService::default()
        .with_get_state(Err(guardian_auth_status("authentication_replay", true)))
        .with_get_state(Err(guardian_auth_status("authentication_replay", true)));
    let recorded_headers = service.get_state_auth_headers_handle();

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(create_test_signer());

    let response = client
        .get_state(&create_test_account_id())
        .await
        .expect("replay rejections within the retry budget must be retried to success");
    assert!(response.success);

    let attempts = recorded_headers.lock().unwrap().clone();
    assert_eq!(attempts.len(), 3, "one initial attempt plus two retries");
    assert!(
        attempts[0].0 < attempts[1].0 && attempts[1].0 < attempts[2].0,
        "every attempt must mint a strictly increasing timestamp: {attempts:?}"
    );
    let signatures: std::collections::HashSet<&String> =
        attempts.iter().map(|(_, signature)| signature).collect();
    assert_eq!(
        signatures.len(),
        3,
        "every attempt must carry a fresh signature over the new timestamp"
    );
}

#[tokio::test]
async fn replay_retries_stop_at_the_bounded_budget() {
    // Three queued replay errors exceed the two-retry budget. If the client
    // sent a fourth attempt it would hit the mock's default success, so the
    // surfaced replay error proves the bound is exact.
    let service = MockGuardianService::default()
        .with_get_state(Err(guardian_auth_status("authentication_replay", true)))
        .with_get_state(Err(guardian_auth_status("authentication_replay", true)))
        .with_get_state(Err(guardian_auth_status("authentication_replay", true)));
    let recorded_headers = service.get_state_auth_headers_handle();

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(create_test_signer());

    let error = client
        .get_state(&create_test_account_id())
        .await
        .expect_err("a replay condition outlasting the retry budget must surface");
    assert!(error.is_replay_rejection());
    assert_eq!(
        recorded_headers.lock().unwrap().len(),
        3,
        "one initial attempt plus exactly two retries"
    );
}

#[tokio::test]
async fn terminal_authentication_failure_is_not_retried() {
    // Same mock semantics as above: a retry would hit the default success
    // response, so the surfaced error proves the client gave up immediately.
    let service = MockGuardianService::default()
        .with_get_state(Err(guardian_auth_status("authentication_failed", false)));

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(create_test_signer());

    let error = client
        .get_state(&create_test_account_id())
        .await
        .expect_err("a terminal authentication failure must not be retried");
    assert_eq!(
        error.guardian_code().as_deref(),
        Some("authentication_failed")
    );
    assert!(!error.is_replay_rejection());
}

#[tokio::test]
async fn test_get_state_not_found() {
    let service = MockGuardianService::default().with_get_state(Ok(GetStateResponse {
        success: false,
        message: "State not found".to_string(),
        state: None,
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let account_id = create_test_account_id();

    let result = client.get_state(&account_id).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert_eq!(msg, "State not found");
        }
        e => panic!("Expected ServerError, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_signer_pubkey_hex_without_signer() {
    let service = MockGuardianService::default();
    let endpoint = start_mock_server(service).await.unwrap();
    let client = GuardianClient::connect(endpoint).await.unwrap();

    let result = client.signer_pubkey_hex();

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::InvalidResponse(msg) => {
            assert!(msg.contains("no signer configured"));
        }
        e => panic!("Expected InvalidResponse, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_signer_pubkey_hex_with_signer() {
    let service = MockGuardianService::default();
    let endpoint = start_mock_server(service).await.unwrap();
    let signer = create_test_signer();
    let expected_pubkey = signer.public_key_hex();
    let client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let result = client.signer_pubkey_hex();

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), expected_pubkey);
}

// --- lookup_account_by_key_commitment -

fn lookup_test_signer() -> (Arc<dyn Signer>, String) {
    let signer = Arc::new(FalconKeyStore::new(SecretKey::new())) as Arc<dyn Signer>;
    let commitment_hex = signer.commitment_hex();
    (signer, commitment_hex)
}

#[tokio::test]
async fn test_lookup_account_by_key_commitment_single_match() {
    let service = MockGuardianService::default().with_get_account_by_key_commitment(Ok(
        GetAccountByKeyCommitmentResponse {
            accounts: vec![AccountRef {
                account_id: "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string(),
            }],
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let (signer, commitment_hex) = lookup_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let response = client
        .lookup_account_by_key_commitment(&commitment_hex)
        .await
        .expect("happy path returns Ok");
    assert_eq!(response.accounts.len(), 1);
    assert_eq!(
        response.accounts[0].account_id,
        "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"
    );
}

#[tokio::test]
async fn test_lookup_account_by_key_commitment_empty_result() {
    // Empty list MUST surface as a successful response (not an error) so the
    // SDK matches the server contract: the multisig recoverByKey helper relies
    // on this to distinguish "no matches" from a real RPC failure.
    let service = MockGuardianService::default().with_get_account_by_key_commitment(Ok(
        GetAccountByKeyCommitmentResponse { accounts: vec![] },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let (signer, commitment_hex) = lookup_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let response = client
        .lookup_account_by_key_commitment(&commitment_hex)
        .await
        .expect("empty list is a successful response");
    assert!(response.accounts.is_empty());
}

#[tokio::test]
async fn test_lookup_account_by_key_commitment_multi_match() {
    let service = MockGuardianService::default().with_get_account_by_key_commitment(Ok(
        GetAccountByKeyCommitmentResponse {
            accounts: vec![
                AccountRef {
                    account_id: "0xaaaaaaaaaaaaaa012aaaaaaaaaaaaa".to_string(),
                },
                AccountRef {
                    account_id: "0xbbbbbbbabbbbbb013bbbbbbbbbbbbb".to_string(),
                },
            ],
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let (signer, commitment_hex) = lookup_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let response = client
        .lookup_account_by_key_commitment(&commitment_hex)
        .await
        .expect("multi-match returns all entries");
    assert_eq!(response.accounts.len(), 2);
    let ids: Vec<&str> = response
        .accounts
        .iter()
        .map(|a| a.account_id.as_str())
        .collect();
    assert!(ids.contains(&"0xaaaaaaaaaaaaaa012aaaaaaaaaaaaa"));
    assert!(ids.contains(&"0xbbbbbbbabbbbbb013bbbbbbbbbbbbb"));
}

#[tokio::test]
async fn test_lookup_account_by_key_commitment_unauthenticated_error_propagates() {
    let service = MockGuardianService::default().with_get_account_by_key_commitment(Err(
        Status::unauthenticated(
            "submitted public key does not derive to the queried key_commitment",
        ),
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let (signer, commitment_hex) = lookup_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let result = client
        .lookup_account_by_key_commitment(&commitment_hex)
        .await;
    let err = result.expect_err("unauthenticated must propagate as ClientError");
    match err {
        ClientError::Status(status) => {
            assert_eq!(status.code(), tonic::Code::Unauthenticated);
        }
        other => panic!("expected Status error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_lookup_account_by_key_commitment_rejects_invalid_commitment_hex() {
    // The client SHOULD fail-fast on a malformed commitment before issuing
    // the gRPC call, so misconfigured callers get a clear local error.
    let service = MockGuardianService::default();
    let endpoint = start_mock_server(service).await.unwrap();
    let (signer, _commitment_hex) = lookup_test_signer();
    let mut client = GuardianClient::connect(endpoint)
        .await
        .unwrap()
        .with_signer(signer);

    let result = client.lookup_account_by_key_commitment("not-hex").await;
    let err = result.expect_err("malformed hex must fail locally");
    match err {
        ClientError::InvalidResponse(msg) => {
            assert!(msg.contains("Invalid key_commitment hex"), "{msg}");
        }
        other => panic!("expected InvalidResponse, got {other:?}"),
    }
}
