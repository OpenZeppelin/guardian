use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::candidate_chain::{self, CandidateChain};
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use crate::services::{normalize_payload, resolve_account};
use guardian_shared::DeltaSignature;

const DEFAULT_MAX_PENDING_PROPOSALS_PER_ACCOUNT: usize = 20;
const MAX_PENDING_PROPOSALS_ENV_VAR: &str = "GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT";

fn max_pending_proposals_per_account() -> usize {
    std::env::var(MAX_PENDING_PROPOSALS_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_PENDING_PROPOSALS_PER_ACCOUNT)
}

#[derive(Debug, Clone)]
pub struct PushDeltaProposalParams {
    pub account_id: String,
    pub nonce: u64,
    pub delta_payload: serde_json::Value,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaProposalResult {
    pub delta: DeltaObject,
    pub commitment: String,
}

#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(
        account_id = %params.account_id,
        nonce = params.nonce,
        proposer_id = tracing::field::Empty,
        commitment = tracing::field::Empty,
        signer_count = tracing::field::Empty
    )
)]
pub async fn push_delta_proposal(
    state: &AppState,
    params: PushDeltaProposalParams,
) -> Result<PushDeltaProposalResult> {
    tracing::debug!("Pushing delta proposal");

    if crate::metadata::network::is_evm_account_id(&params.account_id)
        || params
            .delta_payload
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind == "evm")
    {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "delta_proposal".to_string(),
        });
    }

    let PushDeltaProposalParams {
        account_id,
        nonce,
        delta_payload,
        credentials,
    } = params;

    let delta_payload = normalize_payload(delta_payload)?;

    let resolved = resolve_account(state, &account_id, &credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "delta_proposal".to_string(),
        });
    }

    // Fetch current state to validate delta
    let mut current_state = resolved
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|_| GuardianError::StateNotFound(account_id.clone()))?;

    // Queue admission (issue #17): a proposal is pinned to the tail of
    // the account's candidate chain — the state the eventual delta must
    // build on. A proposal whose delta could never be admitted there is
    // refused up front rather than after cosigners have signed it:
    // - while the queue is full (with depth 1 this is the historical
    //   "one in-flight candidate" refusal);
    // - when a queue exists and its nonce is not the tail's plus one.
    //   Both SDKs label a proposal with the account's next nonce, so one
    //   built on the tail carries exactly that. At or below the tail's
    //   nonce the summary was built on an older state: this is the
    //   cosigner that synced the canonical state (all `/state` serves)
    //   and proposes on it while another device's candidate is queued;
    //   its delta is refused while the queue holds the slot and collides
    //   with the promoted candidate once it drains. Past the tail's nonce
    //   plus one the label was not derived from the tail either (a
    //   timestamp, the TypeScript SDK's default through 0.18.0, or
    //   any client-chosen value): nothing here can tell which state such
    //   a summary was built on, and recorded against the tail it would be
    //   signed only to fail at execution, holding a proposal slot for as
    //   long as the tail stays. The one client that can build on the
    //   tail, the device that pushed it, labels with its nonce plus one.
    // - when the tail changes who may act on the account (a queued
    //   signer-set or guardian change): nothing chains behind it until it
    //   promotes (`ensure_tail_keeps_auth`, judged on the replayed tail).
    let chain = CandidateChain::load_for_admission(
        resolved.storage.as_ref(),
        &account_id,
        &mut current_state,
        None,
    )
    .await?;
    let max_pending_candidates = candidate_chain::max_pending_candidates(state);
    if chain.len() >= max_pending_candidates {
        tracing::info!(
            account_id = %account_id,
            nonce,
            queued = chain.len(),
            max_pending_candidates,
            "Candidate queue is full; refusing proposal as pending-delta conflict"
        );
        return Err(GuardianError::ConflictPendingDelta);
    }
    let tail_nonce = chain.tail_nonce();
    if let Some(tail_nonce) = tail_nonce
        && tail_nonce.checked_add(1) != Some(nonce)
    {
        tracing::info!(
            account_id = %account_id,
            nonce,
            tail_nonce,
            "Proposal nonce is not the candidate queue tail's plus one; refusing as pending-delta conflict"
        );
        return Err(GuardianError::ConflictPendingDelta);
    }
    let tail_commitment = chain.tail_commitment(&current_state.commitment).to_string();

    let pending_proposals = resolved
        .storage
        .pull_pending_proposals(&account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to load pending proposals in push_delta_proposal"
            );
            GuardianError::StorageError(format!("Failed to load pending proposals: {e}"))
        })?;

    // Only viable proposals consume capacity. A proposal built on a
    // superseded commitment can never become a candidate, so counting it
    // would let dead proposals accumulate until the account is permanently
    // locked out with PendingProposalsLimit (#337). Non-viable proposals
    // stay in storage and remain visible via pull_pending_proposals.
    // Viability is measured against the chain tail: that commitment is
    // what promotion drives the canonical state towards, and a proposal
    // pinned to it whose nonce does not exceed the tail's is as dead as
    // one on a superseded commitment (see the refusal above). One pinned
    // to the tail past its nonce plus one can no longer be recorded, but
    // one an earlier release recorded there is still executable from the
    // tail, so it counts. The limit is checked before the tail replay,
    // which costs one delta application per queued candidate.
    let viable_pending = pending_proposals
        .iter()
        .filter(|record| {
            record.proposal.prev_commitment == tail_commitment
                && tail_nonce.is_none_or(|tail_nonce| record.proposal.nonce > tail_nonce)
        })
        .count();

    let max_pending_proposals = max_pending_proposals_per_account();
    if viable_pending >= max_pending_proposals {
        return Err(GuardianError::PendingProposalsLimit {
            limit: max_pending_proposals,
        });
    }

    let tail = chain.reconstruct_tail(state, &current_state).await?;
    candidate_chain::ensure_tail_keeps_auth(state, &current_state, &tail).await?;

    // Extract tx_summary and signatures from delta_payload
    let tx_summary = delta_payload
        .get("tx_summary")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;

    let signatures = delta_payload
        .get("signatures")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    // Validate delta using network client (check validity but don't apply)
    // and compute the delta commitment
    let commitment = {
        let client = &state.network_client;
        client
            .verify_delta(&tail.commitment, &tail.state_json, tx_summary)
            .map_err(GuardianError::InvalidDelta)?;

        // Compute the delta proposal ID from the tx_summary
        client
            .delta_proposal_id(&account_id, nonce, tx_summary)
            .map_err(GuardianError::InvalidDelta)?
    };
    tracing::Span::current().record("commitment", tracing::field::display(&commitment));

    let proposer_id = resolved.signer_commitment.clone();
    tracing::Span::current().record("proposer_id", tracing::field::display(&proposer_id));

    // At creation only the proposer's own approval may be attached; every
    // other cosigner approves through the signing endpoint.
    let signature_timestamp = state.clock.now_rfc3339();
    let mut cosigner_sigs = Vec::new();
    for sig_value in signatures {
        let parsed: DeltaSignature = serde_json::from_value(sig_value).map_err(|e| {
            GuardianError::InvalidDelta(format!("Invalid signature entry in payload: {e}"))
        })?;

        if !cosigner_sigs.is_empty() || !parsed.signer_id.eq_ignore_ascii_case(&proposer_id) {
            return Err(GuardianError::InvalidProposalSignature(
                "Proposal creation accepts only the proposer's own signature".to_string(),
            ));
        }
        let tx_summary = proposal_tx_summary(&delta_payload)?;
        let approval_signer = verify_proposal_signature(&tx_summary, &parsed.signature)?;
        if !approval_signer.eq_ignore_ascii_case(&proposer_id) {
            return Err(GuardianError::InvalidProposalSignature(
                "Signature does not belong to the proposer".to_string(),
            ));
        }

        cosigner_sigs.push(CosignerSignature {
            signature: parsed.signature,
            timestamp: signature_timestamp.clone(),
            signer_id: proposer_id.clone(),
        });
    }
    tracing::Span::current().record("signer_count", cosigner_sigs.len());
    // Create delta object with Pending status including any provided signatures
    let timestamp = state.clock.now_rfc3339();
    let delta_proposal = DeltaObject {
        account_id: account_id.clone(),
        nonce,
        prev_commitment: tail.commitment.clone(),
        new_commitment: None,
        delta_payload,
        ack_sig: String::new(),
        ack_pubkey: String::new(),
        ack_scheme: String::new(),
        status: DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        },
        metadata: None,
    };

    // Store the delta proposal in the proposals directory using the commitment as ID
    resolved
        .storage
        .submit_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(GuardianError::StorageError)?;
    metrics::counter!(
        crate::metrics::names::PROPOSALS_TOTAL,
        crate::metrics::names::LABEL_EVENT =>
            crate::metrics::labels::ProposalEvent::Created.as_str()
    )
    .increment(1);
    tracing::info!("Delta proposal created");

    Ok(PushDeltaProposalResult {
        delta: delta_proposal.clone(),
        commitment: commitment.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
    use crate::testing::fixtures;
    use crate::testing::helpers::{TestSigner, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::TimeZone;
    use guardian_shared::{EcdsaMessageFormat, FromJson, ProposalSignature};
    use miden_protocol::Word;
    use miden_protocol::transaction::TransactionSummary;
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

    fn create_state_object(
        account_id: String,
        commitment: String,
        state_json: serde_json::Value,
    ) -> StateObject {
        StateObject {
            account_id,
            commitment,
            nonce: None,
            state_json,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_pending_proposal(account_id: &str, nonce: u64) -> DeltaObject {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();

        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: None,
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"].clone(),
                "signatures": []
            }),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                proposer_id: "0xproposer".to_string(),
                cosigner_sigs: vec![],
            },
            metadata: None,
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_success() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        // Generate valid Falcon signature
        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(
                test_pubkey.clone(),
                test_signature.clone(),
                test_timestamp,
            ),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();
        assert_eq!(
            result.commitment,
            "0xabababababababababababababababababababababababababababababababab"
        );
        assert_eq!(result.delta.account_id, account_id);
        assert_eq!(result.delta.nonce, 1);

        match &result.delta.status {
            DeltaStatus::Pending {
                proposer_id,
                cosigner_sigs,
                ..
            } => {
                assert_eq!(*proposer_id, test_commitment_hex);
                assert_eq!(cosigner_sigs.len(), 0);
            }
            _ => panic!("Expected Pending status"),
        }

        let submit_calls = storage.get_submit_delta_proposal_calls();
        assert_eq!(submit_calls.len(), 1);
        assert_eq!(
            submit_calls[0].0,
            "0xabababababababababababababababababababababababababababababababab"
        );
    }

    #[tokio::test]
    async fn test_push_delta_proposal_with_eip712_proposer() {
        use crate::metadata::auth::RequestAuthFormat;
        use guardian_shared::auth_request_eip712::request_digest;
        use guardian_shared::auth_request_message::AuthRequestMessage;
        use guardian_shared::auth_request_payload::AuthRequestPayload;
        use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
        use miden_protocol::utils::serde::Serializable;

        let (state, storage, network, metadata) = create_test_state();
        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let key = SigningKey::new();
        let public_key = key.public_key();
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        let proposer_id = format!("0x{}", hex::encode(public_key.to_commitment().to_bytes()));
        let summary = TransactionSummary::from_json(&delta_fixture["delta_payload"]).unwrap();
        let approval = format!(
            "0x{}",
            hex::encode(
                key.sign_prehash(summary.eip712_hash().into_bytes())
                    .to_bytes()
            )
        );

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenEcdsa {
                cosigner_commitments: vec![proposer_id.clone()],
            },
        ))));
        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));
        let _network = network.with_verify_delta(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"],
            "signatures": [{
                "signer_id": proposer_id,
                "signature": {
                    "scheme": "ecdsa",
                    "signature": approval,
                    "public_key": public_key_hex,
                    "message_format": "eip712"
                }
            }],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [proposer_id]
            }
        });
        let body = serde_json::json!({
            "account_id": account_id,
            "nonce": 1,
            "delta_payload": delta_payload,
        });
        let payload = AuthRequestPayload::from_json_serializable(&body).unwrap();
        let timestamp = state.clock.now().timestamp_millis();
        let request_hash =
            AuthRequestMessage::from_account_id_hex(&account_id, timestamp, payload.clone())
                .unwrap()
                .to_word();
        let signature = key.sign_prehash(request_digest(request_hash));

        let result = push_delta_proposal(
            &state,
            PushDeltaProposalParams {
                account_id,
                nonce: 1,
                delta_payload,
                credentials: Credentials::signature(
                    public_key_hex.clone(),
                    format!("0x{}", hex::encode(signature.to_bytes())),
                    timestamp,
                )
                .with_auth_format(RequestAuthFormat::Eip712)
                .with_request_payload(payload),
            },
        )
        .await
        .unwrap();

        match result.delta.status {
            DeltaStatus::Pending {
                proposer_id: actual,
                cosigner_sigs,
                ..
            } => {
                assert_eq!(actual, proposer_id);
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(
                    cosigner_sigs[0].signature,
                    ProposalSignature::Ecdsa {
                        signature: approval,
                        public_key: Some(public_key_hex),
                        message_format: EcdsaMessageFormat::Eip712,
                    }
                );
            }
            _ => panic!("expected pending proposal"),
        }
        assert_eq!(storage.get_submit_delta_proposal_calls().len(), 1);
    }

    #[tokio::test]
    async fn test_push_delta_proposal_accepts_custom_proposal_type() {
        // Issue #266: a proposal_type outside the first-party set (e.g. an
        // agglayer bridge note) must be accepted at ingress — the old
        // VALID_PROPOSAL_TYPES allowlist used to reject it here.
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "b2agg",
                "description": "agglayer bridge note"
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(
                test_pubkey.clone(),
                test_signature.clone(),
                test_timestamp,
            ),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(
            result.is_ok(),
            "custom proposal_type should be accepted, got: {:?}",
            result
        );
        let result = result.unwrap();
        assert_eq!(result.delta.proposal_type(), Some("b2agg"));
        assert_eq!(storage.get_submit_delta_proposal_calls().len(), 1);
    }

    #[tokio::test]
    async fn test_push_delta_proposal_success_for_ecdsa() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::auth_request_payload::AuthRequestPayload;

        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";
        let signer = TestEcdsaSigner::new();
        let approval = signer.sign_word(fixture_summary_commitment());

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenEcdsa {
                cosigner_commitments: vec![signer.commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 2,
                "required_signatures": 2,
                "signer_commitments": [signer.commitment_hex.clone()]
            },
            "signatures": [{
                "signer_id": signer.commitment_hex.clone(),
                "signature": {
                    "scheme": "ecdsa",
                    "signature": approval,
                    "public_key": signer.pubkey_hex.clone()
                }
            }]
        });
        let request_body = serde_json::json!({
            "account_id": account_id.clone(),
            "nonce": 1,
            "delta_payload": delta_payload.clone(),
        });
        let request_payload = AuthRequestPayload::from_json_serializable(&request_body).unwrap();
        let (test_signature, test_timestamp) = signer.sign_request(&account_id, &request_payload);

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(
                signer.pubkey_hex.clone(),
                test_signature,
                test_timestamp,
            )
            .with_request_payload(request_payload),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending {
                proposer_id,
                cosigner_sigs,
                ..
            } => {
                assert_eq!(*proposer_id, signer.commitment_hex);
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, signer.commitment_hex);
                assert_eq!(
                    cosigner_sigs[0].signature,
                    ProposalSignature::Ecdsa {
                        signature: approval,
                        public_key: Some(signer.pubkey_hex.clone()),
                        message_format: EcdsaMessageFormat::Raw,
                    }
                );
            }
            _ => panic!("Expected Pending status"),
        }

        let submit_calls = storage.get_submit_delta_proposal_calls();
        assert_eq!(submit_calls.len(), 1);
        assert_eq!(
            submit_calls[0].0,
            "0xabababababababababababababababababababababababababababababababab"
        );
    }

    /// The commitment the fixture proposal's approvals sign.
    fn fixture_summary_commitment() -> Word {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        TransactionSummary::from_json(&delta_fixture["delta_payload"])
            .unwrap()
            .to_commitment()
    }

    /// A `signatures[]` entry as the Rust SDK attaches it.
    fn falcon_signature_entry(signer_id: &str, signature: String) -> serde_json::Value {
        serde_json::json!({
            "signer_id": signer_id,
            "signature": { "scheme": "falcon", "signature": signature }
        })
    }

    /// Creates the fixture proposal as `proposer`, attaching `signatures`.
    async fn push_with_signatures(
        proposer: &TestSigner,
        cosigner: &TestSigner,
        signatures: Vec<serde_json::Value>,
    ) -> (Result<PushDeltaProposalResult>, MockStorageBackend) {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let cosigners = vec![
            proposer.commitment_hex.clone(),
            cosigner.commitment_hex.clone(),
        ];

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: cosigners.clone(),
            },
        ))));
        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392".to_string(),
            account_json,
        )));
        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let (request_signature, timestamp) = proposer.sign(&account_id);
        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"].clone(),
                "signatures": signatures,
                "metadata": {
                    "proposal_type": "change_threshold",
                    "target_threshold": 1,
                    "signer_commitments": cosigners
                }
            }),
            credentials: Credentials::signature(
                proposer.pubkey_hex.clone(),
                request_signature,
                timestamp,
            ),
        };
        (push_delta_proposal(&state, params).await, storage)
    }

    #[tokio::test]
    async fn test_push_delta_proposal_with_proposer_signature() {
        let proposer = TestSigner::new();
        let cosigner = TestSigner::new();
        let approval = proposer.sign_word(fixture_summary_commitment());
        let entry = falcon_signature_entry(&proposer.commitment_hex, approval.clone());

        let (result, _storage) = push_with_signatures(&proposer, &cosigner, vec![entry]).await;

        let result = result.unwrap();
        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, proposer.commitment_hex);
                assert_eq!(
                    cosigner_sigs[0].signature,
                    ProposalSignature::Falcon {
                        signature: approval
                    }
                );
            }
            _ => panic!("Expected Pending status"),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_rejects_signatures_other_than_the_proposers() {
        let proposer = TestSigner::new();
        let cosigner = TestSigner::new();
        let summary_commitment = fixture_summary_commitment();
        let proposer_approval = || {
            falcon_signature_entry(
                &proposer.commitment_hex,
                proposer.sign_word(summary_commitment),
            )
        };
        let cases = [
            (
                "another cosigner's valid approval",
                vec![falcon_signature_entry(
                    &cosigner.commitment_hex,
                    cosigner.sign_word(summary_commitment),
                )],
            ),
            (
                "malformed signature under the proposer's id",
                vec![falcon_signature_entry(
                    &proposer.commitment_hex,
                    format!("0x{}", "a".repeat(666)),
                )],
            ),
            (
                "another cosigner's signature under the proposer's id",
                vec![falcon_signature_entry(
                    &proposer.commitment_hex,
                    cosigner.sign_word(summary_commitment),
                )],
            ),
            (
                "proposer's signature over another message",
                vec![falcon_signature_entry(
                    &proposer.commitment_hex,
                    proposer.sign_word(Word::default()),
                )],
            ),
            (
                "duplicate proposer approval",
                vec![proposer_approval(), proposer_approval()],
            ),
        ];

        for (label, signatures) in cases {
            let (result, storage) = push_with_signatures(&proposer, &cosigner, signatures).await;

            assert!(
                matches!(result, Err(GuardianError::InvalidProposalSignature(_))),
                "{label}: expected InvalidProposalSignature, got: {result:?}"
            );
            assert!(storage.get_submit_delta_proposal_calls().is_empty());
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_missing_tx_summary() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let delta_payload = serde_json::json!({
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::InvalidDelta(msg) => {
                assert!(msg.contains("tx_summary"));
            }
            e => panic!("Expected InvalidDelta error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_invalid_delta() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let _network = network.with_verify_delta(Err("Invalid delta".to_string()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::InvalidDelta(msg) => {
                assert_eq!(msg, "Invalid delta");
            }
            e => panic!("Expected InvalidDelta error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_state_not_found() {
        let (state, storage, _network, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let _storage = storage.with_pull_state(Err("State not found".to_string()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StateNotFound(id) => {
                assert_eq!(id, account_id);
            }
            e => panic!("Expected StateNotFound error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_blocked_by_pending_candidate() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        // Mock pull_deltas_after to return a candidate delta (this triggers has_pending_candidate)
        let candidate_delta = DeltaObject {
            account_id: account_id.clone(),
            nonce: 1,
            prev_commitment: test_commitment.to_string(),
            new_commitment: Some("0xnewcommitment".to_string()),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Candidate {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                retry_count: 0,
                divergence_count: 0,
                abandon_requested_at: None,
                abandon_confirm_count: 0,
            },
            metadata: None,
        };
        let _storage = storage.with_pull_deltas_after(Ok(vec![candidate_delta]));

        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 2,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::ConflictPendingDelta => {
                // Expected - proposal creation blocked because there's a pending candidate
            }
            e => panic!("Expected ConflictPendingDelta error, got: {:?}", e),
        }
    }

    /// Issue #17: with queue depth to spare, a proposal is pinned to the
    /// replayed queue tail — the state its delta will have to build on —
    /// rather than refused or pinned to the canonical state.
    #[tokio::test]
    async fn test_push_delta_proposal_is_pinned_to_the_queue_tail() {
        let (state, storage, network, metadata) = create_test_state();
        let mut state = state;
        state.canonicalization = Some(
            crate::canonicalization::CanonicalizationConfig::default()
                .with_max_pending_candidates_per_account(4),
        );

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let canonical_commitment =
            "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));
        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                canonical_commitment.to_string(),
                account_json.clone(),
            )))
            .with_pull_deltas_after(Ok(vec![DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: canonical_commitment.to_string(),
                new_commitment: Some("0xtail".to_string()),
                delta_payload: serde_json::json!({}),
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string()),
                metadata: None,
            }]));
        // The queue replay reproduces the stored tail commitment.
        let _network = network
            .with_validate_credential(Ok(()))
            .with_apply_delta(Ok((account_json, "0xtail".to_string())));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "custom",
                "description": "queued behind an in-flight candidate"
            }
        });
        let result = push_delta_proposal(
            &state,
            PushDeltaProposalParams {
                account_id: account_id.clone(),
                nonce: 2,
                delta_payload,
                credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
            },
        )
        .await
        .expect("the proposal is accepted against the queue tail");
        assert_eq!(result.delta.prev_commitment, "0xtail");
        assert!(result.delta.status.is_pending());
    }

    /// Issue #17: a proposal is refused up front once the queue is full —
    /// its delta could not be admitted anyway, and refusing before
    /// cosigners sign is cheaper than refusing after.
    #[tokio::test]
    async fn test_push_delta_proposal_refused_when_the_queue_is_full() {
        let (state, storage, network, metadata) = create_test_state();
        let mut state = state;
        state.canonicalization = Some(
            crate::canonicalization::CanonicalizationConfig::default()
                .with_max_pending_candidates_per_account(2),
        );

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let canonical_commitment =
            "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));
        let queued = |nonce: u64, prev: &str, new: &str| DeltaObject {
            account_id: account_id.clone(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: Some(new.to_string()),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string()),
            metadata: None,
        };
        let storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                canonical_commitment.to_string(),
                account_json,
            )))
            .with_pull_deltas_after(Ok(vec![
                queued(1, canonical_commitment, "0xc1"),
                queued(2, "0xc1", "0xc2"),
            ]));
        // A replay response is registered so the assertion below can
        // prove the refusal happened before any reconstruction.
        let network = network
            .with_validate_credential(Ok(()))
            .with_apply_delta(Ok((serde_json::json!({}), "0xc2".to_string())));

        let result = push_delta_proposal(
            &state,
            PushDeltaProposalParams {
                account_id: account_id.clone(),
                nonce: 3,
                delta_payload: serde_json::json!({
                    "tx_summary": delta_fixture["delta_payload"].clone(),
                    "signatures": [],
                    "metadata": {
                        "proposal_type": "custom",
                        "description": "refused while the queue is full"
                    }
                }),
                credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
            },
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert_eq!(
            network.apply_delta_responses.lock().unwrap().len(),
            1,
            "refused before any replay"
        );
        assert!(
            storage.get_submit_delta_proposal_calls().is_empty(),
            "refused before any write"
        );
    }

    /// Issue #17 fixtures: a Falcon 1-of-1 fixture account in candidate
    /// mode at `depth`, its canonical state at `CANONICAL`, and `queue` as
    /// its candidate queue.
    const CANONICAL: &str = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

    struct QueuedProposalSetup {
        state: AppState,
        storage: MockStorageBackend,
        network: MockNetworkClient,
        account_id: String,
        credentials: Credentials,
        tx_summary: serde_json::Value,
    }

    fn queued_proposal_setup(
        depth: usize,
        queue: Vec<DeltaObject>,
        pending: Vec<DeltaObject>,
    ) -> QueuedProposalSetup {
        queued_proposal_setup_with(depth, queue, pending, |network| network)
    }

    /// [`queued_proposal_setup`] with extra canned network answers, for
    /// the checks that read the replayed tail.
    fn queued_proposal_setup_with(
        depth: usize,
        queue: Vec<DeltaObject>,
        pending: Vec<DeltaObject>,
        configure_network: impl FnOnce(MockNetworkClient) -> MockNetworkClient,
    ) -> QueuedProposalSetup {
        let (state, storage, network, metadata) = create_test_state();
        let mut state = state;
        state.canonicalization = Some(
            crate::canonicalization::CanonicalizationConfig::default()
                .with_max_pending_candidates_per_account(depth),
        );
        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let (pubkey, commitment_hex, signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
        ))));
        let storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                CANONICAL.to_string(),
                account_json.clone(),
            )))
            .with_pull_deltas_after(Ok(queue))
            .with_pull_all_delta_proposals(Ok(pending));
        // One replay response: the assertions below tell from it whether
        // the tail was ever materialized.
        let network = configure_network(
            network
                .with_validate_credential(Ok(()))
                .with_apply_delta(Ok((account_json, "0xtail".to_string()))),
        );
        QueuedProposalSetup {
            state,
            storage,
            network,
            account_id,
            credentials: Credentials::signature(pubkey, signature, timestamp),
            tx_summary: delta_fixture["delta_payload"].clone(),
        }
    }

    fn fixture_account_id() -> String {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        delta_fixture["account_id"].as_str().unwrap().to_string()
    }

    fn queued_candidate(account_id: &str, nonce: u64, prev: &str, new: &str) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: Some(new.to_string()),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string()),
            metadata: None,
        }
    }

    fn pending_on(account_id: &str, nonce: u64, prev: &str) -> DeltaObject {
        let mut proposal = create_pending_proposal(account_id, nonce);
        proposal.prev_commitment = prev.to_string();
        proposal
    }

    async fn propose(setup: &QueuedProposalSetup, nonce: u64) -> Result<PushDeltaProposalResult> {
        push_delta_proposal(
            &setup.state,
            PushDeltaProposalParams {
                account_id: setup.account_id.clone(),
                nonce,
                delta_payload: serde_json::json!({
                    "tx_summary": setup.tx_summary.clone(),
                    "signatures": [],
                    "metadata": {
                        "proposal_type": "custom",
                        "description": "proposed against the queue"
                    }
                }),
                credentials: setup.credentials.clone(),
            },
        )
        .await
    }

    /// Issue #17: while a queue exists, only a proposal labelled with the
    /// tail's nonce plus one — what a client that built on the tail
    /// labels it with — is recorded. At or below the tail's nonce the
    /// proposal is doomed (its delta is refused while the tail's
    /// candidate holds the slot and collides with it once promoted): the
    /// cosigner that synced the canonical state and proposed on it while
    /// another device's candidate was queued. Past the tail's nonce plus
    /// one the label did not come from the tail either (a timestamp, the
    /// TypeScript SDK's default through 0.18.0): refused the same
    /// way, before any cosigner signs it.
    #[tokio::test]
    async fn test_push_delta_proposal_refused_unless_it_extends_the_tail_by_one() {
        let account_id = &fixture_account_id();
        for nonce in [4, 5, 7, 1_790_941_600_874] {
            let setup = queued_proposal_setup(
                4,
                vec![queued_candidate(account_id, 5, CANONICAL, "0xtail")],
                vec![],
            );
            let result = propose(&setup, nonce).await;
            assert!(
                matches!(result, Err(GuardianError::ConflictPendingDelta)),
                "nonce {nonce}: {result:?}"
            );
            assert!(
                setup.storage.get_submit_delta_proposal_calls().is_empty(),
                "nothing is stored for cosigners to sign"
            );
            assert_eq!(
                setup.network.apply_delta_responses.lock().unwrap().len(),
                1,
                "refused before any replay"
            );
            assert!(
                setup
                    .storage
                    .pull_all_delta_proposals_calls
                    .lock()
                    .unwrap()
                    .is_empty(),
                "refused before the pending set is read"
            );
        }

        // The next nonce extends the queue and is pinned to its tail.
        let setup = queued_proposal_setup(
            4,
            vec![queued_candidate(account_id, 5, CANONICAL, "0xtail")],
            vec![],
        );
        let accepted = propose(&setup, 6).await.expect("nonce 6 extends the queue");
        assert_eq!(accepted.delta.prev_commitment, "0xtail");
    }

    /// Proposals pinned to the tail at or below its nonce are as dead as
    /// proposals on a superseded commitment: they never consume capacity.
    #[tokio::test]
    async fn test_push_delta_proposal_doomed_tail_proposals_do_not_consume_capacity() {
        let account_id = &fixture_account_id();
        let doomed = (0..20)
            .map(|_| pending_on(account_id, 1, "0xtail"))
            .collect();
        let setup = queued_proposal_setup(
            4,
            vec![queued_candidate(account_id, 1, CANONICAL, "0xtail")],
            doomed,
        );
        let accepted = propose(&setup, 2)
            .await
            .expect("doomed proposals leave the capacity free");
        assert_eq!(accepted.delta.prev_commitment, "0xtail");
    }

    /// The pending-proposal limit is checked against the tail commitment
    /// before the tail is replayed: a refused proposal costs no delta
    /// applications.
    #[tokio::test]
    async fn test_push_delta_proposal_limit_is_checked_before_the_tail_replay() {
        let account_id = &fixture_account_id();
        // Recorded against the tail by an earlier release with timestamp
        // labels: no longer admissible, still executable from the tail,
        // so they hold their slots.
        let viable = (0..20)
            .map(|i| pending_on(account_id, 1_790_941_600_874 + i, "0xtail"))
            .collect();
        let setup = queued_proposal_setup(
            4,
            vec![queued_candidate(account_id, 1, CANONICAL, "0xtail")],
            viable,
        );
        let result = propose(&setup, 2).await;
        assert!(
            matches!(
                result,
                Err(GuardianError::PendingProposalsLimit { limit: 20 })
            ),
            "{result:?}"
        );
        assert_eq!(
            setup.network.apply_delta_responses.lock().unwrap().len(),
            1,
            "refused before any replay"
        );
    }

    /// The request read the state; the worker then promoted the only
    /// queued candidate before the queue read, which came back empty. The
    /// proposal must be pinned to the promoted state — the real tail — not
    /// to the stale read, where it would be non-viable on arrival (and the
    /// TypeScript SDK, which pushes the stored proposal's base, could
    /// never push it).
    #[tokio::test]
    async fn test_push_delta_proposal_pinned_to_the_state_a_mid_request_promotion_reached() {
        let setup = queued_proposal_setup(4, vec![], vec![]);
        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        // Reads pop LIFO: the request's first read (queued by the setup)
        // must be the stale one, so the promoted state goes underneath it.
        let stale = setup
            .storage
            .pull_state_responses
            .lock()
            .unwrap()
            .pop()
            .expect("setup state");
        {
            let mut responses = setup.storage.pull_state_responses.lock().unwrap();
            responses.push(Ok(create_state_object(
                setup.account_id.clone(),
                "0xpromoted".to_string(),
                account_json,
            )));
            responses.push(stale);
        }
        let accepted = propose(&setup, 2)
            .await
            .expect("the proposal is admitted against the promoted state");
        assert_eq!(
            accepted.delta.prev_commitment, "0xpromoted",
            "pinned to the state the promotion reached"
        );
    }

    #[tokio::test]
    async fn test_push_delta_proposal_blocked_by_pending_proposal_limit() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let mut pending = Vec::new();
        for nonce in 1..=20u64 {
            pending.push(create_pending_proposal(&account_id, nonce));
        }

        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
                account_json,
            )))
            .with_pull_all_delta_proposals(Ok(pending));

        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 21,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::PendingProposalsLimit { limit } => {
                assert_eq!(limit, 20);
            }
            e => panic!("Expected PendingProposalsLimit error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_allows_when_pending_proposals_below_limit() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let mut pending = Vec::new();
        for nonce in 1..20u64 {
            pending.push(create_pending_proposal(&account_id, nonce));
        }

        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
                account_json,
            )))
            .with_pull_all_delta_proposals(Ok(pending));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 20,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
    }

    fn create_stale_proposal(account_id: &str, nonce: u64) -> DeltaObject {
        let mut proposal = create_pending_proposal(account_id, nonce);
        proposal.prev_commitment = "0xsuperseded".to_string();
        proposal
    }

    #[tokio::test]
    async fn test_push_delta_proposal_stale_proposals_do_not_consume_capacity() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        // A full cap's worth of proposals built on a superseded commitment:
        // none of them can ever canonicalize, so none may consume capacity.
        let mut pending = Vec::new();
        for nonce in 1..=20u64 {
            pending.push(create_stale_proposal(&account_id, nonce));
        }

        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
                account_json,
            )))
            .with_pull_all_delta_proposals(Ok(pending));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 21,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
    }

    #[tokio::test]
    async fn test_push_delta_proposal_limit_counts_only_viable_proposals() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        // Stale proposals are ignored, but a full cap of viable proposals
        // (prev_commitment matching current state) still blocks the push.
        let mut pending = Vec::new();
        for nonce in 1..=20u64 {
            pending.push(create_stale_proposal(&account_id, nonce));
        }
        for nonce in 21..=40u64 {
            pending.push(create_pending_proposal(&account_id, nonce));
        }

        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
                account_json,
            )))
            .with_pull_all_delta_proposals(Ok(pending));

        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 41,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::PendingProposalsLimit { limit } => {
                assert_eq!(limit, 20);
            }
            e => panic!("Expected PendingProposalsLimit error, got: {:?}", e),
        }
    }

    /// Pause-gate guard: a paused account must be rejected before
    /// any proposal-side effects fire — but only AFTER authentication
    /// succeeds, so unauthenticated probes cannot leak pause state.
    /// Asserts AccountPaused and that `submit_delta_proposal` was
    /// never called.
    #[tokio::test]
    async fn paused_account_rejected_before_proposal_submitted() {
        let (state, storage, _network, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let mut paused = create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        );
        paused.paused_at = Some(
            chrono::Utc
                .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                .unwrap(),
        );
        paused.paused_reason = Some("compliance".to_string());
        let _metadata = metadata.with_get(Ok(Some(paused)));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let err = push_delta_proposal(&state, params)
            .await
            .expect_err("paused account must be rejected");
        assert!(
            matches!(err, GuardianError::AccountPaused { ref paused_reason, .. }
                if paused_reason.as_deref() == Some("compliance")),
            "unexpected error: {err:?}"
        );

        assert!(
            storage.get_submit_delta_proposal_calls().is_empty(),
            "no proposal should be submitted when the account is paused"
        );
    }

    /// Pause-gate ordering: unauthenticated callers MUST get an auth
    /// error, NOT `AccountPaused`. Defends against reintroducing the
    /// pre-auth chokepoint that leaked pause state to probes.
    #[tokio::test]
    async fn paused_account_returns_auth_error_for_unauthenticated_caller() {
        let (state, _storage, _network, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

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

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": ["0xc1"]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(String::new(), String::new(), 0),
        };

        let err = push_delta_proposal(&state, params)
            .await
            .expect_err("unauthenticated paused account must be rejected with auth error");
        assert!(
            matches!(err, GuardianError::AuthenticationFailed(_)),
            "unauthenticated caller must not learn pause state; got: {err:?}"
        );
    }

    /// Nothing is recorded behind a candidate that changes who may act on
    /// the account: judged on the replayed tail, so the refusal comes after
    /// the replay and before anything is stored. The decision matrix lives
    /// in `candidate_chain`; this proves the wiring.
    #[tokio::test]
    async fn test_push_delta_proposal_refused_behind_a_signer_changing_candidate() {
        let account_id = &fixture_account_id();
        let binding = |signers: &[&str]| crate::network::AuthBinding {
            signers: signers.iter().map(|s| s.to_string()).collect(),
            guardian: Some("0xguardian".to_string()),
        };
        let setup = queued_proposal_setup_with(
            4,
            vec![queued_candidate(account_id, 5, CANONICAL, "0xtail")],
            vec![],
            // Bindings pop LIFO: the canonical state is read first.
            |network| {
                network
                    .with_account_auth_binding(Ok(Some(binding(&["0xaa", "0xbb"]))))
                    .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))))
            },
        );
        let result = propose(&setup, 6).await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert!(
            setup.storage.get_submit_delta_proposal_calls().is_empty(),
            "nothing is stored for cosigners to sign"
        );
        assert!(
            setup
                .network
                .apply_delta_responses
                .lock()
                .unwrap()
                .is_empty(),
            "judged on the replayed tail"
        );
    }
}
