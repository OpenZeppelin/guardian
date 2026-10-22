use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus, ProposalSignature};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::proposal_signature::{
    ensure_cosigner_approval, proposal_tx_summary, verify_proposal_signature,
};
use crate::services::resolve_account_allowing_session;
use crate::utils::normalize_commitment_hex;
use guardian_shared::DeltaSignature;

#[derive(Debug, Clone)]
pub struct SignDeltaProposalParams {
    pub account_id: String,
    pub commitment: String,
    pub signature: ProposalSignature,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct SignDeltaProposalResult {
    pub delta: DeltaObject,
}

#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(
        account_id = %params.account_id,
        commitment = %params.commitment,
        signer_commitment = tracing::field::Empty,
        total_signatures = tracing::field::Empty
    )
)]
pub async fn sign_delta_proposal(
    state: &AppState,
    params: SignDeltaProposalParams,
) -> Result<SignDeltaProposalResult> {
    let SignDeltaProposalParams {
        account_id,
        commitment,
        signature,
        credentials,
    } = params;

    let normalized_commitment = normalize_commitment_hex(&commitment)?;

    // Resolve account and verify authentication
    let resolved = resolve_account_allowing_session(state, &account_id, &credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "delta_proposal".to_string(),
        });
    }

    let mut delta_proposal =
        load_pending_proposal(&resolved, &account_id, &normalized_commitment).await?;

    // The approval's signer is whoever the signature cryptographically
    // verifies for, never the caller: a cosigner who signed the summary
    // off channel (an exported proposal signed on another machine) has
    // authorized the transaction whoever relays the document, exactly as
    // on chain, where the executed transaction carries every collected
    // signature. The request itself is still authenticated above, so only
    // a configured signer can relay, and the approval must belong to one
    // of the account's cosigners, under the scheme the account verifies
    // that cosigner with. The summary is fixed for a proposal id, so the
    // signature is verified once, before any retry.
    let tx_summary = proposal_tx_summary(&delta_proposal.delta_payload)?;
    let approval_signer = verify_proposal_signature(&tx_summary, &signature)?;
    tracing::Span::current().record(
        "signer_commitment",
        tracing::field::display(&approval_signer),
    );
    ensure_cosigner_approval(&resolved.metadata.auth, &approval_signer, &signature)?;
    if !approval_signer.eq_ignore_ascii_case(&resolved.signer_commitment) {
        tracing::info!(
            relayer = %resolved.signer_commitment,
            "Proposal approval relayed on behalf of another cosigner"
        );
    }

    // The approval is appended to the row as read and committed only if
    // no other write changed it meanwhile; otherwise the row is read again
    // and the approval appended to that. A plain write would let two
    // concurrent signings each commit from the same read and drop the
    // other's approval, which the multisig push gate then never counts.
    // Every lost race means another write committed, so the attempts are
    // bounded by how many cosigners sign at once.
    for _ in 0..MAX_SIGN_ATTEMPTS {
        let read_status = delta_proposal.status.clone();
        let signed = with_approval(
            delta_proposal,
            &normalized_commitment,
            &approval_signer,
            signature.clone(),
            state.clock.now_rfc3339(),
        )?;
        let committed = resolved
            .storage
            .update_delta_proposal_if_status(&normalized_commitment, &signed, &read_status)
            .await
            .map_err(GuardianError::StorageError)?;
        if committed {
            if let DeltaStatus::Pending { cosigner_sigs, .. } = &signed.status {
                tracing::Span::current().record("total_signatures", cosigner_sigs.len());
            }
            metrics::counter!(
                crate::metrics::names::PROPOSALS_TOTAL,
                crate::metrics::names::LABEL_EVENT =>
                    crate::metrics::labels::ProposalEvent::Signed.as_str()
            )
            .increment(1);
            tracing::info!("Delta proposal signed");
            return Ok(SignDeltaProposalResult { delta: signed });
        }
        tracing::debug!("Delta proposal changed while signing; retrying on the current row");
        delta_proposal =
            load_pending_proposal(&resolved, &account_id, &normalized_commitment).await?;
    }
    Err(GuardianError::StorageError(format!(
        "Delta proposal {normalized_commitment} kept changing while signing; retry the request"
    )))
}

/// More attempts than any account has cosigners signing at once.
const MAX_SIGN_ATTEMPTS: usize = 16;

/// The account's proposal at `commitment`, while it is still pending.
async fn load_pending_proposal(
    resolved: &crate::services::ResolvedAccount,
    account_id: &str,
    commitment: &str,
) -> Result<DeltaObject> {
    let not_found = || GuardianError::ProposalNotFound {
        account_id: account_id.to_string(),
        commitment: commitment.to_string(),
    };
    let proposal = resolved
        .storage
        .pull_delta_proposal(account_id, commitment)
        .await
        .map_err(|_| not_found())?;
    if !proposal.account_id.eq_ignore_ascii_case(account_id) || !proposal.status.is_pending() {
        return Err(not_found());
    }
    Ok(proposal)
}

/// `proposal` with `signer`'s approval appended to its cosigner signatures
/// and to the payload's `signatures` list, or `proposal_already_signed`
/// when it already holds one from `signer`.
fn with_approval(
    mut proposal: DeltaObject,
    commitment: &str,
    signer: &str,
    signature: ProposalSignature,
    now: String,
) -> Result<DeltaObject> {
    let DeltaStatus::Pending {
        timestamp,
        proposer_id,
        mut cosigner_sigs,
    } = proposal.status
    else {
        return Err(GuardianError::ProposalNotFound {
            account_id: proposal.account_id,
            commitment: commitment.to_string(),
        });
    };
    if cosigner_sigs
        .iter()
        .any(|sig| sig.signer_id.eq_ignore_ascii_case(signer))
    {
        return Err(GuardianError::ProposalAlreadySigned {
            signer_id: signer.to_string(),
        });
    }

    let new_sig = DeltaSignature {
        signer_id: signer.to_string(),
        signature: signature.clone(),
    };
    cosigner_sigs.push(CosignerSignature {
        signature,
        timestamp: now,
        signer_id: signer.to_string(),
    });
    let new_sig = serde_json::to_value(new_sig)
        .map_err(|e| GuardianError::InvalidDelta(format!("Failed to serialize signature: {e}")))?;
    if let Some(signatures) = proposal
        .delta_payload
        .get_mut("signatures")
        .and_then(|v| v.as_array_mut())
    {
        signatures.push(new_sig);
    } else {
        proposal
            .delta_payload
            .as_object_mut()
            .ok_or_else(|| {
                GuardianError::InvalidDelta("delta_payload must be an object".to_string())
            })?
            .insert(
                "signatures".to_string(),
                serde_json::Value::Array(vec![new_sig]),
            );
    }

    proposal.status = DeltaStatus::Pending {
        timestamp,
        proposer_id,
        cosigner_sigs,
    };
    Ok(proposal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::testing::helpers::{
        TestEcdsaSigner, TestSigner, create_test_app_state_with_mocks, create_test_delta_payload,
    };
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::TimeZone;
    use guardian_shared::{EcdsaMessageFormat, FromJson};
    use miden_protocol::Word;
    use miden_protocol::transaction::TransactionSummary;
    use miden_protocol::utils::serde::Serializable;
    use miden_standards::account::auth::Eip712TransactionSummary;
    use std::sync::Arc;

    fn create_test_state() -> (
        AppState,
        MockStorageBackend,
        MockNetworkClient,
        MockMetadataStore,
    ) {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new();

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        (state, storage, network, metadata)
    }

    fn create_account_metadata(account_id: String, auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn create_pending_proposal(
        account_id: String,
        nonce: u64,
        proposer_id: String,
        cosigner_sigs: Vec<CosignerSignature>,
    ) -> DeltaObject {
        DeltaObject {
            account_id: account_id.clone(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: None,
            delta_payload: serde_json::json!({
                "tx_summary": create_test_delta_payload(&account_id),
                "signatures": []
            }),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                proposer_id,
                cosigner_sigs,
            },
            metadata: None,
        }
    }

    /// Returns the summary every pending test proposal carries and its commitment.
    fn proposal_summary(account_id: &str) -> (TransactionSummary, String) {
        let summary =
            TransactionSummary::from_json(&create_test_delta_payload(account_id)).unwrap();
        let commitment = format!("0x{}", hex::encode(summary.to_commitment().as_bytes()));
        (summary, commitment)
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_credits_verified_falcon_signer() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (summary, commitment) = proposal_summary(&account_id);

        let (proposer_pubkey, proposer_commitment, _proposer_signature, _proposer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let signer = TestSigner::new();
        let signer_commitment = signer.commitment_hex.clone();
        let (signer_signature, signer_timestamp) = signer.sign(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![proposer_commitment.clone(), signer_commitment.clone()],
            },
        ))));

        let pending_proposal =
            create_pending_proposal(account_id.clone(), 1, proposer_commitment.clone(), vec![]);

        let storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal_if_status(Ok(true));

        let approval = signer.sign_word(summary.to_commitment());
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: approval.clone(),
            },
            credentials: Credentials::signature(
                proposer_pubkey,
                signer_signature.clone(),
                signer_timestamp,
            ),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, signer_commitment);
                assert_eq!(
                    cosigner_sigs[0].signature,
                    ProposalSignature::Falcon {
                        signature: approval
                    }
                );
            }
            _ => panic!("Expected Pending status"),
        }

        let payload_sigs = result
            .delta
            .delta_payload
            .get("signatures")
            .and_then(|v| v.as_array())
            .expect("signatures must exist in delta_payload");
        assert_eq!(payload_sigs.len(), 1);

        let update_calls = storage.get_update_delta_proposal_calls();
        assert_eq!(update_calls.len(), 1);
        assert_eq!(update_calls[0].0, commitment);
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_credits_verified_ecdsa_signer() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::auth_request_payload::AuthRequestPayload;

        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (summary, commitment) = proposal_summary(&account_id);

        let proposer = TestEcdsaSigner::new();
        let signer = TestEcdsaSigner::new();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenEcdsa {
                cosigner_commitments: vec![
                    proposer.commitment_hex.clone(),
                    signer.commitment_hex.clone(),
                ],
            },
        ))));

        let pending_proposal = create_pending_proposal(
            account_id.clone(),
            1,
            proposer.commitment_hex.clone(),
            vec![],
        );

        let storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal_if_status(Ok(true));

        let approval = signer.sign_word(summary.to_commitment());
        let proposal_signature = ProposalSignature::Ecdsa {
            signature: approval.clone(),
            public_key: Some(signer.pubkey_hex.clone()),
            message_format: EcdsaMessageFormat::Raw,
        };
        let request_body = serde_json::json!({
            "account_id": account_id.clone(),
            "commitment": commitment.clone(),
            "signature": proposal_signature.clone(),
        });
        let request_payload = AuthRequestPayload::from_json_serializable(&request_body).unwrap();
        let (signer_signature, signer_timestamp) =
            signer.sign_request(&account_id, &request_payload);
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: proposal_signature,
            credentials: Credentials::signature(
                proposer.pubkey_hex.clone(),
                signer_signature,
                signer_timestamp,
            )
            .with_request_payload(request_payload),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, signer.commitment_hex);
                match &cosigner_sigs[0].signature {
                    ProposalSignature::Ecdsa {
                        signature,
                        public_key,
                        ..
                    } => {
                        assert_eq!(*signature, approval);
                        assert_eq!(public_key.as_deref(), Some(signer.pubkey_hex.as_str()));
                    }
                    ProposalSignature::Falcon { .. } => {
                        panic!("expected ECDSA signature")
                    }
                }
            }
            _ => panic!("Expected Pending status"),
        }

        let update_calls = storage.get_update_delta_proposal_calls();
        assert_eq!(update_calls.len(), 1);
        assert_eq!(update_calls[0].0, commitment);
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_with_eip712_approval_and_request_auth() {
        use guardian_shared::auth_request_eip712::request_digest;
        use guardian_shared::auth_request_message::AuthRequestMessage;
        use guardian_shared::auth_request_payload::AuthRequestPayload;
        use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;

        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let key = SigningKey::new();
        let public_key = key.public_key();
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        let signer_commitment = format!("0x{}", hex::encode(public_key.to_commitment().to_bytes()));
        let (summary, commitment) = proposal_summary(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenEcdsa {
                cosigner_commitments: vec![signer_commitment.clone()],
            },
        ))));
        let pending =
            create_pending_proposal(account_id.clone(), 1, signer_commitment.clone(), vec![]);
        let storage = storage
            .with_pull_delta_proposal(Ok(pending))
            .with_update_delta_proposal_if_status(Ok(true));

        let approval_signature = key.sign_prehash(summary.eip712_hash().into_bytes());
        let mut approval_bytes = approval_signature.to_bytes();
        approval_bytes[64] ^= 1;
        let proposal_signature = ProposalSignature::Ecdsa {
            signature: format!("0x{}", hex::encode(approval_bytes)),
            public_key: Some(public_key_hex.clone()),
            message_format: EcdsaMessageFormat::Eip712,
        };
        let body = serde_json::json!({
            "account_id": account_id,
            "commitment": commitment,
            "signature": proposal_signature,
        });
        let payload = AuthRequestPayload::from_json_serializable(&body).unwrap();
        let timestamp = chrono::Utc::now().timestamp_millis();
        let request_hash =
            AuthRequestMessage::from_account_id_hex(&account_id, timestamp, payload.clone())
                .unwrap()
                .to_word();
        let request_signature = key.sign_prehash(request_digest(request_hash));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: proposal_signature,
            credentials: Credentials::signature(
                public_key_hex,
                format!("0x{}", hex::encode(request_signature.to_bytes())),
                timestamp,
            )
            .with_auth_format(crate::metadata::auth::RequestAuthFormat::Eip712)
            .with_request_payload(payload),
        };

        let result = sign_delta_proposal(&state, params).await.unwrap();
        assert!(matches!(result.delta.status, DeltaStatus::Pending { .. }));
        assert_eq!(storage.get_update_delta_proposal_calls().len(), 1);

        let wrong_approval = ProposalSignature::Ecdsa {
            signature: format!("0x{}", hex::encode(key.sign_prehash([0u8; 32]).to_bytes())),
            public_key: Some(format!("0x{}", hex::encode(public_key.to_bytes()))),
            message_format: EcdsaMessageFormat::Eip712,
        };
        let wrong_body = serde_json::json!({
            "account_id": account_id,
            "commitment": commitment,
            "signature": wrong_approval,
        });
        let wrong_payload = AuthRequestPayload::from_json_serializable(&wrong_body).unwrap();
        let next_timestamp = timestamp + 1;
        let next_hash = AuthRequestMessage::from_account_id_hex(
            &account_id,
            next_timestamp,
            wrong_payload.clone(),
        )
        .unwrap()
        .to_word();
        let wrong_params = SignDeltaProposalParams {
            account_id,
            commitment,
            signature: wrong_approval,
            credentials: Credentials::signature(
                format!("0x{}", hex::encode(public_key.to_bytes())),
                format!(
                    "0x{}",
                    hex::encode(key.sign_prehash(request_digest(next_hash)).to_bytes())
                ),
                next_timestamp,
            )
            .with_auth_format(crate::metadata::auth::RequestAuthFormat::Eip712)
            .with_request_payload(wrong_payload),
        };
        assert!(sign_delta_proposal(&state, wrong_params).await.is_err());
        assert_eq!(storage.get_update_delta_proposal_calls().len(), 1);
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_second_signature() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (summary, commitment) = proposal_summary(&account_id);

        let (_proposer_pubkey, proposer_commitment, _proposer_signature, _proposer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (_first_signer_pubkey, first_signer_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let second_signer = TestSigner::new();
        let second_signer_commitment = second_signer.commitment_hex.clone();
        let (second_signer_signature, second_signer_timestamp) = second_signer.sign(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![
                    proposer_commitment.clone(),
                    first_signer_commitment.clone(),
                    second_signer_commitment.clone(),
                ],
            },
        ))));

        let first_sig = format!("0x{}", "a".repeat(666));
        let pending_proposal = create_pending_proposal(
            account_id.clone(),
            1,
            proposer_commitment.clone(),
            vec![CosignerSignature {
                signature: ProposalSignature::Falcon {
                    signature: first_sig,
                },
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                signer_id: first_signer_commitment.clone(),
            }],
        );

        let _storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal_if_status(Ok(true));

        let second_sig = second_signer.sign_word(summary.to_commitment());
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: second_sig.clone(),
            },
            credentials: Credentials::signature(
                second_signer.pubkey_hex.clone(),
                second_signer_signature.clone(),
                second_signer_timestamp,
            ),
        };

        let result = sign_delta_proposal(&state, params).await.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 2);
                assert_eq!(cosigner_sigs[0].signer_id, first_signer_commitment);
                assert_eq!(cosigner_sigs[1].signer_id, second_signer_commitment);
            }
            _ => panic!("Expected Pending status"),
        }
    }

    /// Signs the pending test proposal as `signer`; `proposer` is the other cosigner.
    async fn sign_as(
        signer: &TestSigner,
        proposer: &TestSigner,
        signature: ProposalSignature,
    ) -> (Result<SignDeltaProposalResult>, MockStorageBackend) {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (_summary, commitment) = proposal_summary(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![
                    proposer.commitment_hex.clone(),
                    signer.commitment_hex.clone(),
                ],
            },
        ))));
        let storage = storage
            .with_pull_delta_proposal(Ok(create_pending_proposal(
                account_id.clone(),
                1,
                proposer.commitment_hex.clone(),
                vec![],
            )))
            .with_update_delta_proposal_if_status(Ok(true));

        let (request_signature, timestamp) = signer.sign(&account_id);
        let params = SignDeltaProposalParams {
            account_id,
            commitment,
            signature,
            credentials: Credentials::signature(
                signer.pubkey_hex.clone(),
                request_signature,
                timestamp,
            ),
        };
        (sign_delta_proposal(&state, params).await, storage)
    }

    /// Signs as `signer` against a proposal whose reads return `reads` in
    /// order, and whose conditional writes answer `writes` in order.
    async fn sign_with_reads(
        signer: &TestSigner,
        cosigners: Vec<String>,
        reads: Vec<DeltaObject>,
        writes: Vec<std::result::Result<bool, String>>,
    ) -> (Result<SignDeltaProposalResult>, MockStorageBackend) {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (summary, commitment) = proposal_summary(&account_id);
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: cosigners,
            },
        ))));
        let storage = reads.into_iter().rev().fold(storage, |storage, read| {
            storage.with_pull_delta_proposal(Ok(read))
        });
        let storage = writes.into_iter().rev().fold(storage, |storage, write| {
            storage.with_update_delta_proposal_if_status(write)
        });
        let (request_signature, timestamp) = signer.sign(&account_id);
        let params = SignDeltaProposalParams {
            account_id,
            commitment,
            signature: ProposalSignature::Falcon {
                signature: signer.sign_word(summary.to_commitment()),
            },
            credentials: Credentials::signature(
                signer.pubkey_hex.clone(),
                request_signature,
                timestamp,
            ),
        };
        (sign_delta_proposal(&state, params).await, storage)
    }

    fn held_approval(signer_id: &str) -> CosignerSignature {
        CosignerSignature {
            signature: ProposalSignature::Falcon {
                signature: format!("0x{}", "b".repeat(64)),
            },
            timestamp: "2024-11-14T12:00:00Z".to_string(),
            signer_id: signer_id.to_string(),
        }
    }

    /// Another cosigner's approval committed between this request's read
    /// and its write: the write is refused, the row read again, and this
    /// approval appended to it, so neither approval is lost (issue #524's
    /// gate counts the stored ones).
    #[tokio::test]
    async fn test_sign_delta_proposal_keeps_a_concurrent_approval() {
        let signer = TestSigner::new();
        let proposer = TestSigner::new();
        let other = TestSigner::new();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let before = create_pending_proposal(
            account_id.clone(),
            1,
            proposer.commitment_hex.clone(),
            vec![],
        );
        let after = create_pending_proposal(
            account_id,
            1,
            proposer.commitment_hex.clone(),
            vec![held_approval(&other.commitment_hex)],
        );

        let (result, storage) = sign_with_reads(
            &signer,
            vec![
                proposer.commitment_hex.clone(),
                signer.commitment_hex.clone(),
                other.commitment_hex.clone(),
            ],
            vec![before, after],
            vec![Ok(false), Ok(true)],
        )
        .await;

        let result = result.expect("the retried signing commits");
        let DeltaStatus::Pending { cosigner_sigs, .. } = &result.delta.status else {
            panic!("Expected Pending status");
        };
        let signers: Vec<&str> = cosigner_sigs
            .iter()
            .map(|sig| sig.signer_id.as_str())
            .collect();
        assert_eq!(
            signers,
            vec![
                other.commitment_hex.as_str(),
                signer.commitment_hex.as_str()
            ]
        );
        let writes = storage.get_update_delta_proposal_calls();
        assert_eq!(writes.len(), 1, "only the committed write is recorded");
        assert_eq!(writes[0].1.status, result.delta.status);
    }

    /// The retry stops when the row reached on a retry already holds this
    /// signer's approval: the concurrent write was the same approval.
    #[tokio::test]
    async fn test_sign_delta_proposal_retry_reports_an_approval_committed_meanwhile() {
        let signer = TestSigner::new();
        let proposer = TestSigner::new();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let before = create_pending_proposal(
            account_id.clone(),
            1,
            proposer.commitment_hex.clone(),
            vec![],
        );
        let after = create_pending_proposal(
            account_id,
            1,
            proposer.commitment_hex.clone(),
            vec![held_approval(&signer.commitment_hex)],
        );

        let (result, storage) = sign_with_reads(
            &signer,
            vec![
                proposer.commitment_hex.clone(),
                signer.commitment_hex.clone(),
            ],
            vec![before, after],
            vec![Ok(false)],
        )
        .await;

        assert!(
            matches!(result, Err(GuardianError::ProposalAlreadySigned { .. })),
            "{result:?}"
        );
        assert!(storage.get_update_delta_proposal_calls().is_empty());
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_gives_up_after_bounded_retries() {
        let signer = TestSigner::new();
        let proposer = TestSigner::new();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let reads = (0..=MAX_SIGN_ATTEMPTS)
            .map(|_| {
                create_pending_proposal(
                    account_id.clone(),
                    1,
                    proposer.commitment_hex.clone(),
                    vec![],
                )
            })
            .collect();
        let (result, storage) = sign_with_reads(
            &signer,
            vec![
                proposer.commitment_hex.clone(),
                signer.commitment_hex.clone(),
            ],
            reads,
            vec![Ok(false); MAX_SIGN_ATTEMPTS],
        )
        .await;

        assert!(
            matches!(result, Err(GuardianError::StorageError(_))),
            "{result:?}"
        );
        assert!(storage.get_update_delta_proposal_calls().is_empty());
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_rejects_unverified_approval() {
        let signer = TestSigner::new();
        let proposer = TestSigner::new();
        let cases = [
            ("malformed signature", format!("0x{}", "a".repeat(666))),
            (
                "signature over another message",
                signer.sign_word(Word::default()),
            ),
        ];

        for (label, approval) in cases {
            let signature = ProposalSignature::Falcon {
                signature: approval,
            };
            let (result, storage) = sign_as(&signer, &proposer, signature).await;

            assert!(
                matches!(result, Err(GuardianError::InvalidProposalSignature(_))),
                "{label}: expected InvalidProposalSignature, got: {result:?}"
            );
            assert!(storage.get_update_delta_proposal_calls().is_empty());
        }
    }

    /// A cosigner who signed the summary off channel has authorized the
    /// transaction no matter who relays the document (issue #524's gate
    /// counts stored approvals, so the off-channel execute flow relays
    /// them here first). The approval is credited to the cosigner the
    /// signature verifies for, not to the relaying caller.
    #[tokio::test]
    async fn test_sign_delta_proposal_accepts_a_relayed_cosigner_approval() {
        let signer = TestSigner::new();
        let proposer = TestSigner::new();
        let (summary, _commitment) = proposal_summary("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b");
        let signature = ProposalSignature::Falcon {
            signature: proposer.sign_word(summary.to_commitment()),
        };

        let (result, storage) = sign_as(&signer, &proposer, signature).await;

        let result = result.expect("a relayed cosigner approval is accepted");
        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, proposer.commitment_hex);
            }
            _ => panic!("Expected Pending status"),
        }
        assert_eq!(storage.get_update_delta_proposal_calls().len(), 1);
    }

    /// A valid signature from a key outside the account's cosigner set
    /// buys nothing, whoever relays it.
    #[tokio::test]
    async fn test_sign_delta_proposal_rejects_an_approval_from_a_non_cosigner() {
        let signer = TestSigner::new();
        let proposer = TestSigner::new();
        let outsider = TestSigner::new();
        let (summary, _commitment) = proposal_summary("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b");
        let signature = ProposalSignature::Falcon {
            signature: outsider.sign_word(summary.to_commitment()),
        };

        let (result, storage) = sign_as(&signer, &proposer, signature).await;

        assert!(
            matches!(result, Err(GuardianError::InvalidProposalSignature(_))),
            "Expected InvalidProposalSignature, got: {result:?}"
        );
        assert!(storage.get_update_delta_proposal_calls().is_empty());
    }

    /// `caller` relays `signature` to the pending test proposal of an
    /// account authorized by `auth`.
    async fn relay_on(
        auth: Auth,
        caller: &TestSigner,
        signature: ProposalSignature,
    ) -> (Result<SignDeltaProposalResult>, MockStorageBackend) {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (_summary, commitment) = proposal_summary(&account_id);
        let _metadata =
            metadata.with_get(Ok(Some(create_account_metadata(account_id.clone(), auth))));
        let storage = storage
            .with_pull_delta_proposal(Ok(create_pending_proposal(
                account_id.clone(),
                1,
                caller.commitment_hex.clone(),
                vec![],
            )))
            .with_update_delta_proposal_if_status(Ok(true));
        let (request_signature, timestamp) = caller.sign(&account_id);
        let params = SignDeltaProposalParams {
            account_id,
            commitment,
            signature,
            credentials: Credentials::signature(
                caller.pubkey_hex.clone(),
                request_signature,
                timestamp,
            ),
        };
        (sign_delta_proposal(&state, params).await, storage)
    }

    fn ecdsa_approval(signer: &TestEcdsaSigner) -> ProposalSignature {
        let (summary, _commitment) = proposal_summary("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b");
        ProposalSignature::Ecdsa {
            signature: signer.sign_word(summary.to_commitment()),
            public_key: Some(signer.pubkey_hex.clone()),
            message_format: EcdsaMessageFormat::Raw,
        }
    }

    /// On a mixed account a Falcon cosigner relays an ECDSA cosigner's
    /// approval, made under the scheme the account registers it with.
    #[tokio::test]
    async fn test_sign_delta_proposal_accepts_a_mixed_accounts_approval_under_its_scheme() {
        use crate::metadata::auth::MidenCosigner;
        use guardian_shared::SignatureScheme;

        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let auth = Auth::Miden {
            cosigners: vec![
                MidenCosigner {
                    commitment: falcon.commitment_hex.clone(),
                    scheme: SignatureScheme::Falcon,
                },
                MidenCosigner {
                    commitment: ecdsa.commitment_hex.clone(),
                    scheme: SignatureScheme::Ecdsa,
                },
            ],
            guardian_scheme: SignatureScheme::Ecdsa,
        };

        let (result, storage) = relay_on(auth, &falcon, ecdsa_approval(&ecdsa)).await;

        let result = result.expect("an approval under the registered scheme is accepted");
        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, ecdsa.commitment_hex);
            }
            _ => panic!("Expected Pending status"),
        }
        assert_eq!(storage.get_update_delta_proposal_calls().len(), 1);
    }

    /// An ECDSA key the account registers under Falcon (a mis-built
    /// client's account): its ECDSA approval verifies to a cosigner's
    /// commitment, but the chain verifies that cosigner as Falcon and would
    /// refuse the transaction, so the approval is not stored.
    #[tokio::test]
    async fn test_sign_delta_proposal_rejects_an_approval_under_the_wrong_scheme() {
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec![falcon.commitment_hex.clone(), ecdsa.commitment_hex.clone()],
        };

        let (result, storage) = relay_on(auth, &falcon, ecdsa_approval(&ecdsa)).await;

        assert!(
            matches!(
                &result,
                Err(GuardianError::InvalidProposalSignature(message))
                    if message.contains("signed under ecdsa") && message.contains("under falcon")
            ),
            "{result:?}"
        );
        assert!(storage.get_update_delta_proposal_calls().is_empty());
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let commitment =
            "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment.clone()],
            },
        ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::ProposalNotFound {
                account_id: err_account_id,
                commitment: err_commitment,
            } => {
                assert_eq!(err_account_id, account_id);
                assert_eq!(err_commitment, commitment);
            }
            e => panic!("Expected ProposalNotFound error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_duplicate_signature() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (summary, commitment) = proposal_summary(&account_id);

        let proposer = TestSigner::new();
        let signer = TestSigner::new();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![
                    proposer.commitment_hex.clone(),
                    signer.commitment_hex.clone(),
                ],
            },
        ))));

        let pending_proposal = create_pending_proposal(
            account_id.clone(),
            1,
            proposer.commitment_hex.clone(),
            vec![CosignerSignature {
                signature: ProposalSignature::Falcon {
                    signature: signer.sign_word(summary.to_commitment()),
                },
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                signer_id: signer.commitment_hex.clone(),
            }],
        );

        let _storage = storage.with_pull_delta_proposal(Ok(pending_proposal.clone()));

        let (request_signature, timestamp) = signer.sign(&account_id);
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: signer.sign_word(summary.to_commitment()),
            },
            credentials: Credentials::signature(
                signer.pubkey_hex.clone(),
                request_signature,
                timestamp,
            ),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::ProposalAlreadySigned { signer_id } => {
                assert_eq!(signer_id, signer.commitment_hex);
            }
            e => panic!("Expected ProposalAlreadySigned error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_unauthorized_signer() {
        let (state, _storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let commitment =
            "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature, _proposer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (
            unauthorized_pubkey,
            _unauthorized_commitment,
            unauthorized_signature,
            unauthorized_timestamp,
        ) = crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![proposer_commitment.clone()],
            },
        ))));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
            credentials: Credentials::signature(
                unauthorized_pubkey,
                unauthorized_signature,
                unauthorized_timestamp,
            ),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthenticationFailed(_) => {}
            e => panic!("Expected AuthenticationFailed error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_storage_error() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (summary, commitment) = proposal_summary(&account_id);

        let (_proposer_pubkey, proposer_commitment, _proposer_signature, _proposer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let signer = TestSigner::new();
        let signer_commitment = signer.commitment_hex.clone();
        let (signer_signature, signer_timestamp) = signer.sign(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![proposer_commitment.clone(), signer_commitment.clone()],
            },
        ))));

        let pending_proposal =
            create_pending_proposal(account_id.clone(), 1, proposer_commitment.clone(), vec![]);

        let _storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal_if_status(Err("Storage write failed".to_string()));

        let approval = signer.sign_word(summary.to_commitment());
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: approval,
            },
            credentials: Credentials::signature(
                signer.pubkey_hex.clone(),
                signer_signature,
                signer_timestamp,
            ),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StorageError(_) => {}
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_invalid_commitment_rejected() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment],
            },
        ))));

        let params = SignDeltaProposalParams {
            account_id,
            commitment: "../../other_account/proposals/abc".to_string(),
            signature: ProposalSignature::Falcon {
                signature: format!("0x{}", "a".repeat(666)),
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let result = sign_delta_proposal(&state, params).await;
        assert!(matches!(result, Err(GuardianError::InvalidCommitment(_))));
        assert!(
            storage.get_pull_delta_proposal_calls().is_empty(),
            "storage should not be queried for invalid commitment input"
        );
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_rejects_mismatched_account() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let commitment =
            "0x1111111111111111111111111111111111111111111111111111111111111111".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature, _proposer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![proposer_commitment.clone(), signer_commitment],
            },
        ))));

        let mut pending_proposal =
            create_pending_proposal(account_id.clone(), 1, proposer_commitment, vec![]);
        pending_proposal.account_id =
            "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd".to_string();

        let _storage = storage.with_pull_delta_proposal(Ok(pending_proposal));

        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: format!("0x{}", "a".repeat(666)),
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let result = sign_delta_proposal(&state, params).await;
        assert!(matches!(
            result,
            Err(GuardianError::ProposalNotFound {
                account_id: ref err_account,
                commitment: ref err_commitment
            }) if err_account == &account_id && err_commitment == &commitment
        ));
    }

    /// Pause-gate guard: signing must be rejected for a paused
    /// account before any proposal storage is touched — but only
    /// AFTER authentication, so unauthenticated probes cannot learn
    /// pause state.
    #[tokio::test]
    async fn paused_account_rejected_before_proposal_lookup() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let mut paused = create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment.clone()],
            },
        );
        paused.paused_at = Some(
            chrono::Utc
                .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                .unwrap(),
        );
        paused.paused_reason = Some("compliance".to_string());
        let _metadata = metadata.with_get(Ok(Some(paused)));

        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            signature: ProposalSignature::Falcon {
                signature: format!("0x{}", "a".repeat(666)),
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let err = sign_delta_proposal(&state, params)
            .await
            .expect_err("paused account must be rejected");
        assert!(
            matches!(err, GuardianError::AccountPaused { ref paused_reason, .. }
                if paused_reason.as_deref() == Some("compliance")),
            "unexpected error: {err:?}"
        );

        assert!(
            storage.get_update_delta_proposal_calls().is_empty(),
            "no proposal update should fire when the account is paused"
        );
    }

    /// Pause-gate ordering: unauthenticated callers MUST get an auth
    /// error, NOT `AccountPaused`. Defends against reintroducing the
    /// pre-auth chokepoint that leaked pause state to probes.
    #[tokio::test]
    async fn paused_account_returns_auth_error_for_unauthenticated_caller() {
        let (state, _storage, _network, metadata) = create_test_state();

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let mut paused = create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
        );
        paused.paused_at = Some(
            chrono::Utc
                .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                .unwrap(),
        );
        paused.paused_reason = Some("compliance".to_string());
        let _metadata = metadata.with_get(Ok(Some(paused)));

        let params = SignDeltaProposalParams {
            account_id,
            commitment: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            signature: ProposalSignature::Falcon {
                signature: format!("0x{}", "a".repeat(666)),
            },
            credentials: Credentials::signature(String::new(), String::new(), 0),
        };

        let err = sign_delta_proposal(&state, params)
            .await
            .expect_err("unauthenticated paused account must be rejected with auth error");
        assert!(
            matches!(err, GuardianError::AuthenticationFailed(_)),
            "unauthenticated caller must not learn pause state; got: {err:?}"
        );
    }
}
