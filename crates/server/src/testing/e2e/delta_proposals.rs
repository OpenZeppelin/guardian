use crate::delta_object::{DeltaObject, DeltaStatus, ProposalSignature};
use crate::metadata::auth::{Auth, Credentials};
use crate::services::{
    ConfigureAccountParams, GetDeltaProposalsParams, PushDeltaParams, PushDeltaProposalParams,
    SignDeltaProposalParams, configure_account, get_delta_proposals, push_delta,
    push_delta_proposal, sign_delta_proposal,
};
use crate::testing::fixtures;
use crate::testing::helpers::{create_test_app_state, generate_falcon_signature};

#[tokio::test]
async fn test_sign_delta_proposal() {
    let state = create_test_app_state().await;

    let account_json: serde_json::Value =
        serde_json::from_str(fixtures::ACCOUNT_JSON).expect("Failed to parse account.json");
    let commitments_json: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("Failed to parse commitments.json");

    let account_id = commitments_json["account_id"]
        .as_str()
        .expect("Missing account_id")
        .to_string();

    // Generate two different cosigner keys
    let (pubkey1_hex, commitment1_hex, signature1_hex) = generate_falcon_signature(&account_id);
    let (pubkey2_hex, commitment2_hex, signature2_hex) = generate_falcon_signature(&account_id);

    // Configure account with two cosigners
    let configure_params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment1_hex.clone(), commitment2_hex.clone()],
        },
        initial_state: account_json.clone(),
        credential: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    configure_account(&state, configure_params)
        .await
        .expect("Failed to configure account");

    // Load delta fixture - the fixture has delta_payload which is the TransactionSummary
    let delta_fixture = crate::testing::helpers::load_fixture_delta(1);

    // Wrap it in the expected format for push_delta_proposal (with tx_summary field)
    let delta_payload = serde_json::json!({
        "tx_summary": delta_fixture.get("delta_payload").expect("Missing delta_payload in fixture"),
        "signatures": []
    });

    // Create delta proposal with first cosigner
    let proposal_params = PushDeltaProposalParams {
        account_id: account_id.clone(),
        nonce: 1,
        delta_payload,
        credentials: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    let proposal_result = push_delta_proposal(&state, proposal_params)
        .await
        .expect("Failed to push delta proposal");

    let commitment = proposal_result.commitment.clone();

    // Verify initial proposal has 0 signatures (proposer didn't include signature in payload)
    match &proposal_result.delta.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => {
            assert_eq!(cosigner_sigs.len(), 0, "Expected 0 signatures initially");
        }
        _ => panic!("Expected Pending status"),
    }

    // Second cosigner signs the proposal with a dummy signature
    // In a real scenario, they would sign the commitment with their private key
    let dummy_sig = format!("0x{}", "a".repeat(666)); // Falcon signatures are 666 hex chars

    let sign_params = SignDeltaProposalParams {
        account_id: account_id.clone(),
        commitment: commitment.clone(),
        signature: ProposalSignature::Falcon {
            signature: dummy_sig,
        },
        credentials: Credentials::signature(pubkey2_hex.clone(), signature2_hex.clone()),
    };

    let sign_result = sign_delta_proposal(&state, sign_params)
        .await
        .expect("Failed to sign proposal");

    // Verify proposal now has 1 signature (from second cosigner)
    match &sign_result.delta.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => {
            assert_eq!(
                cosigner_sigs.len(),
                1,
                "Expected 1 signature after second cosigner signs"
            );
        }
        _ => panic!("Expected Pending status"),
    }
}

#[tokio::test]
async fn test_multi_cosigner_signing_workflow() {
    let state = create_test_app_state().await;

    let account_json: serde_json::Value =
        serde_json::from_str(fixtures::ACCOUNT_JSON).expect("Failed to parse account.json");
    let commitments_json: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("Failed to parse commitments.json");

    let account_id = commitments_json["account_id"]
        .as_str()
        .expect("Missing account_id")
        .to_string();

    // Generate three different cosigner keys
    let (pubkey1_hex, commitment1_hex, signature1_hex) = generate_falcon_signature(&account_id);
    let (pubkey2_hex, commitment2_hex, signature2_hex) = generate_falcon_signature(&account_id);
    let (pubkey3_hex, commitment3_hex, signature3_hex) = generate_falcon_signature(&account_id);

    // Configure account with three cosigners
    let configure_params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec![
                commitment1_hex.clone(),
                commitment2_hex.clone(),
                commitment3_hex.clone(),
            ],
        },
        initial_state: account_json.clone(),
        credential: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    configure_account(&state, configure_params)
        .await
        .expect("Failed to configure account");

    // Load delta fixture - the fixture has delta_payload which is the TransactionSummary
    let delta_fixture = crate::testing::helpers::load_fixture_delta(1);

    // Wrap it in the expected format for push_delta_proposal (with tx_summary field)
    let delta_payload = serde_json::json!({
        "tx_summary": delta_fixture.get("delta_payload").expect("Missing delta_payload in fixture"),
        "signatures": []
    });

    // Cosigner 1 creates proposal
    let proposal_params = PushDeltaProposalParams {
        account_id: account_id.clone(),
        nonce: 1,
        delta_payload,
        credentials: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    let proposal_result = push_delta_proposal(&state, proposal_params)
        .await
        .expect("Failed to push delta proposal");

    let commitment = proposal_result.commitment.clone();

    // Verify 0 signatures initially (proposer didn't include signature in payload)
    match &proposal_result.delta.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => {
            assert_eq!(cosigner_sigs.len(), 0);
        }
        _ => panic!("Expected Pending status"),
    }

    // Cosigner 2 signs
    let dummy_sig = format!("0x{}", "b".repeat(666));
    let sign_params2 = SignDeltaProposalParams {
        account_id: account_id.clone(),
        commitment: commitment.clone(),
        signature: ProposalSignature::Falcon {
            signature: dummy_sig.clone(),
        },
        credentials: Credentials::signature(pubkey2_hex.clone(), signature2_hex.clone()),
    };

    let sign_result2 = sign_delta_proposal(&state, sign_params2)
        .await
        .expect("Failed to sign proposal with cosigner 2");

    match &sign_result2.delta.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => {
            assert_eq!(cosigner_sigs.len(), 1);
        }
        _ => panic!("Expected Pending status"),
    }

    // Cosigner 3 signs
    let dummy_sig = format!("0x{}", "c".repeat(666));
    let sign_params3 = SignDeltaProposalParams {
        account_id: account_id.clone(),
        commitment: commitment.clone(),
        signature: ProposalSignature::Falcon {
            signature: dummy_sig,
        },
        credentials: Credentials::signature(pubkey3_hex.clone(), signature3_hex.clone()),
    };

    let sign_result3 = sign_delta_proposal(&state, sign_params3)
        .await
        .expect("Failed to sign proposal with cosigner 3");

    match &sign_result3.delta.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => {
            assert_eq!(
                cosigner_sigs.len(),
                2,
                "Expected 2 cosigners to have signed"
            );
        }
        _ => panic!("Expected Pending status"),
    }

    // Verify all proposals are returned
    let get_proposals_params = GetDeltaProposalsParams {
        account_id: account_id.clone(),
        credentials: Credentials::signature(pubkey1_hex, signature1_hex),
    };

    let proposals_result = get_delta_proposals(&state, get_proposals_params)
        .await
        .expect("Failed to get proposals");

    assert_eq!(proposals_result.proposals.len(), 1);
    match &proposals_result.proposals[0].status {
        DeltaStatus::Pending { cosigner_sigs, .. } => {
            assert_eq!(
                cosigner_sigs.len(),
                2,
                "Expected 2 signatures in retrieved proposal"
            );
        }
        _ => panic!("Expected Pending status"),
    }
}

#[tokio::test]
async fn test_proposal_cleanup_after_canonicalization_optimistic() {
    let mut state = create_test_app_state().await;
    // Set to optimistic mode (immediate canonicalization)
    state.canonicalization = None;

    let account_json: serde_json::Value =
        serde_json::from_str(fixtures::ACCOUNT_JSON).expect("Failed to parse account.json");
    let commitments_json: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("Failed to parse commitments.json");

    let account_id = commitments_json["account_id"]
        .as_str()
        .expect("Missing account_id")
        .to_string();

    let (pubkey1_hex, commitment1_hex, signature1_hex) = generate_falcon_signature(&account_id);

    // Configure account
    let configure_params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment1_hex.clone()],
        },
        initial_state: account_json.clone(),
        credential: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    configure_account(&state, configure_params)
        .await
        .expect("Failed to configure account");

    // Load delta fixture - the fixture has delta_payload which is the TransactionSummary
    let delta_fixture = crate::testing::helpers::load_fixture_delta(1);

    // Wrap it in the expected format for push_delta_proposal (with tx_summary field)
    let delta_payload = serde_json::json!({
        "tx_summary": delta_fixture.get("delta_payload").expect("Missing delta_payload in fixture"),
        "signatures": []
    });

    // Create delta proposal
    let proposal_params = PushDeltaProposalParams {
        account_id: account_id.clone(),
        nonce: 1,
        delta_payload,
        credentials: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    let proposal_result = push_delta_proposal(&state, proposal_params)
        .await
        .expect("Failed to push delta proposal");

    // Verify proposal exists
    let get_proposals_params = GetDeltaProposalsParams {
        account_id: account_id.clone(),
        credentials: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    let proposals_before = get_delta_proposals(&state, get_proposals_params.clone())
        .await
        .expect("Failed to get proposals");
    assert_eq!(proposals_before.proposals.len(), 1);

    // Now push the delta (which should canonicalize immediately in optimistic mode)
    // Use the fixture's delta_payload directly (which is the TransactionSummary)
    let delta = DeltaObject {
        account_id: account_id.clone(),
        nonce: 1,
        prev_commitment: proposal_result.delta.prev_commitment.clone(),
        new_commitment: None,
        delta_payload: delta_fixture
            .get("delta_payload")
            .expect("Missing delta_payload")
            .clone(),
        ack_sig: None,
        status: DeltaStatus::Pending {
            timestamp: state.clock.now_rfc3339(),
            proposer_id: commitment1_hex.clone(),
            cosigner_sigs: vec![],
        },
    };

    let push_params = PushDeltaParams {
        delta,
        credentials: Credentials::signature(pubkey1_hex.clone(), signature1_hex.clone()),
    };

    let push_result = push_delta(&state, push_params)
        .await
        .expect("Failed to push delta");

    // Verify delta is canonical
    assert!(push_result.delta.status.is_canonical());

    // Verify proposal was cleaned up
    let proposals_after = get_delta_proposals(&state, get_proposals_params)
        .await
        .expect("Failed to get proposals");

    // The proposal should be gone since the delta is now canonical
    assert_eq!(
        proposals_after.proposals.len(),
        0,
        "Proposal should be deleted after delta becomes canonical"
    );
}
