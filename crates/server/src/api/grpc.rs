use crate::delta_object::{DeltaObject, ProposalSignature};
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials, ExtractCredentials};
use crate::services::{
    self, ConfigureAccountParams, GetDeltaParams, GetDeltaProposalParams, GetHistoryParams,
    GetStateParams, LookupAccountParams, PushDeltaParams,
};
use crate::state::AppState;
use guardian_shared::SignatureScheme;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use prost::Message;
use tonic::{Request, Response, Status};

// Include the generated protobuf code
pub mod guardian {
    tonic::include_proto!("guardian");

    // Include the file descriptor set for reflection
    pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("../../proto/guardian_descriptor.bin");
}

use guardian::guardian_server::Guardian;
use guardian::*;

use guardian::DeltaStatus as DeltaStatusGrpc;

pub struct GuardianService {
    pub app_state: AppState,
}

#[tonic::async_trait]
impl Guardian for GuardianService {
    async fn configure(
        &self,
        request: Request<ConfigureRequest>,
    ) -> Result<Response<ConfigureResponse>, Status> {
        let credential = authenticated_request(&request)?;

        let req = request.into_inner();

        // Parse auth from proto AuthConfig
        let auth_config = req
            .auth
            .ok_or_else(|| Status::invalid_argument("Missing auth configuration"))?;

        let auth = Auth::try_from(auth_config)
            .map_err(|e| Status::invalid_argument(format!("Invalid auth config: {e}")))?;
        let network_config = match req.network_config {
            Some(network_config) => NetworkConfig::try_from(network_config)
                .map_err(|e| Status::invalid_argument(format!("Invalid network config: {e}")))?,
            None => NetworkConfig::miden_default(),
        };

        // Parse initial_state JSON
        let initial_state: serde_json::Value = serde_json::from_str(&req.initial_state)
            .map_err(|e| Status::invalid_argument(format!("Invalid initial_state JSON: {e}")))?;

        let params = ConfigureAccountParams {
            account_id: req.account_id.clone(),
            auth,
            network_config,
            initial_state,
            credential,
        };

        // Call service layer
        match services::configure_account(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", response.account_id),
                ack_pubkey: response.ack_pubkey,
                ack_commitment: response.ack_commitment,
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn push_delta(
        &self,
        request: Request<PushDeltaRequest>,
    ) -> Result<Response<PushDeltaResponse>, Status> {
        let auth = authenticated_request(&request)?;

        let req = request.into_inner();

        // Parse delta_payload JSON
        let delta_payload: serde_json::Value = serde_json::from_str(&req.delta_payload)
            .map_err(|e| Status::invalid_argument(format!("Invalid delta_payload JSON: {e}")))?;

        let delta = DeltaObject {
            account_id: req.account_id,
            nonce: req.nonce,
            prev_commitment: req.prev_commitment,
            new_commitment: None,
            delta_payload,
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: Default::default(),
            // Derived server-side at push time; never carried inbound.
            metadata: None,
        };

        let params = PushDeltaParams {
            delta,
            credentials: auth,
        };

        // Call service layer
        match services::push_delta(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(PushDeltaResponse {
                success: true,
                message: "Delta pushed successfully".to_string(),
                delta: Some(delta_to_proto(&response.delta)),
                ack_sig: Some(response.delta.ack_sig),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_delta(
        &self,
        request: Request<GetDeltaRequest>,
    ) -> Result<Response<GetDeltaResponse>, Status> {
        let auth = authenticated_request(&request)?;

        let req = request.into_inner();

        let params = GetDeltaParams {
            account_id: req.account_id,
            nonce: req.nonce,
            credentials: auth,
        };

        // Call service layer
        match services::get_delta(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(GetDeltaResponse {
                success: true,
                message: "Delta retrieved successfully".to_string(),
                delta: Some(delta_to_proto(&response.delta)),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_delta_since(
        &self,
        request: Request<GetDeltaSinceRequest>,
    ) -> Result<Response<GetDeltaSinceResponse>, Status> {
        let auth = authenticated_request(&request)?;

        let req = request.into_inner();

        let params = services::GetDeltaSinceParams {
            account_id: req.account_id,
            from_nonce: req.from_nonce,
            credentials: auth,
        };

        // Call service layer
        match services::get_delta_since(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(GetDeltaSinceResponse {
                success: true,
                message: "Merged delta retrieved successfully".to_string(),
                merged_delta: Some(delta_to_proto(&response.merged_delta)),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_history(
        &self,
        request: Request<GetHistoryRequest>,
    ) -> Result<Response<GetHistoryResponse>, Status> {
        let auth = authenticated_request(&request)?;

        let req = request.into_inner();

        let limit = services::validate_limit(req.limit).map_err(Status::from)?;
        let cursor = services::parse_cursor(
            req.cursor.as_deref(),
            self.app_state.dashboard.cursor_secret(),
            crate::dashboard::cursor::CursorKind::AccountHistory,
        )
        .map_err(Status::from)?;

        let params = GetHistoryParams {
            account_id: req.account_id,
            limit,
            cursor,
            credentials: auth,
        };

        // Call service layer
        match services::get_history(&self.app_state, params).await {
            Ok(page) => Ok(Response::new(GetHistoryResponse {
                success: true,
                message: "History retrieved successfully".to_string(),
                entries: page.items.into_iter().map(history_entry_to_proto).collect(),
                next_cursor: page.next_cursor,
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_state(
        &self,
        request: Request<GetStateRequest>,
    ) -> Result<Response<GetStateResponse>, Status> {
        let auth = authenticated_request(&request)?;

        let req = request.into_inner();

        let params = GetStateParams {
            account_id: req.account_id,
            credentials: auth,
        };

        // Call service layer
        match services::get_state(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(GetStateResponse {
                success: true,
                message: "State retrieved successfully".to_string(),
                state: Some(state_to_proto(&response.state)),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_pubkey(
        &self,
        request: Request<GetPubkeyRequest>,
    ) -> Result<Response<GetPubkeyResponse>, Status> {
        let req = request.into_inner();
        let scheme = match req.scheme.as_deref() {
            Some(s) if s.eq_ignore_ascii_case("ecdsa") => SignatureScheme::Ecdsa,
            _ => SignatureScheme::Falcon,
        };
        let commitment = self.app_state.ack.commitment(&scheme);
        let raw_pubkey = if matches!(scheme, SignatureScheme::Ecdsa) {
            Some(self.app_state.ack.pubkey(&scheme))
        } else {
            None
        };
        Ok(Response::new(GetPubkeyResponse {
            pubkey: commitment,
            raw_pubkey,
        }))
    }

    async fn push_delta_proposal(
        &self,
        request: Request<PushDeltaProposalRequest>,
    ) -> Result<Response<PushDeltaProposalResponse>, Status> {
        let credentials = authenticated_request(&request)?;
        let data = request.into_inner();

        let params = services::PushDeltaProposalParams {
            account_id: data.account_id,
            nonce: data.nonce,
            delta_payload: serde_json::from_str(&data.delta_payload)
                .map_err(|e| Status::invalid_argument(format!("Invalid delta payload: {e}")))?,
            credentials,
        };

        match services::push_delta_proposal(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(PushDeltaProposalResponse {
                success: true,
                message: "Delta proposal submitted successfully".to_string(),
                delta: Some(delta_to_proto(&response.delta)),
                commitment: response.commitment,
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_delta_proposals(
        &self,
        request: Request<GetDeltaProposalsRequest>,
    ) -> Result<Response<GetDeltaProposalsResponse>, Status> {
        let credentials = authenticated_request(&request)?;
        let data = request.into_inner();

        let params = services::GetDeltaProposalsParams {
            account_id: data.account_id,
            credentials,
        };

        match services::get_delta_proposals(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(GetDeltaProposalsResponse {
                success: true,
                message: "Delta proposals retrieved successfully".to_string(),
                proposals: response.proposals.iter().map(delta_to_proto).collect(),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn get_delta_proposal(
        &self,
        request: Request<GetDeltaProposalRequest>,
    ) -> Result<Response<GetDeltaProposalResponse>, Status> {
        let credentials = authenticated_request(&request)?;
        let data = request.into_inner();

        let params = GetDeltaProposalParams {
            account_id: data.account_id,
            commitment: data.commitment,
            credentials,
        };

        match services::get_delta_proposal(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(GetDeltaProposalResponse {
                success: true,
                message: "Delta proposal retrieved successfully".to_string(),
                proposal: Some(delta_to_proto(&response.proposal)),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    async fn sign_delta_proposal(
        &self,
        request: Request<SignDeltaProposalRequest>,
    ) -> Result<Response<SignDeltaProposalResponse>, Status> {
        let credentials = authenticated_request(&request)?;
        let data = request.into_inner();

        let signature = data
            .signature
            .ok_or_else(|| Status::invalid_argument("Missing signature payload"))?;

        let params = services::SignDeltaProposalParams {
            account_id: data.account_id,
            commitment: data.commitment,
            signature: proto_signature_to_internal(signature)?,
            credentials,
        };

        match services::sign_delta_proposal(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(SignDeltaProposalResponse {
                success: true,
                message: "Delta proposal signed successfully".to_string(),
                delta: Some(delta_to_proto(&response.delta)),
                error_code: String::new(),
            })),
            Err(e) => Err(Status::from(e)),
        }
    }

    /// Request abandonment of a pending canonicalization candidate
    /// (issue #319): records the intent; the worker resolves it after the
    /// abandon quarantine. Mirror of HTTP `POST /delta/candidate/abandon`.
    async fn abandon_delta_candidate(
        &self,
        request: Request<AbandonDeltaCandidateRequest>,
    ) -> Result<Response<AbandonDeltaCandidateResponse>, Status> {
        let credentials = authenticated_request(&request)?;
        let data = request.into_inner();

        let params = services::AbandonCandidateParams {
            account_id: data.account_id.clone(),
            nonce: data.nonce,
            credentials,
        };

        match services::abandon_candidate(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(AbandonDeltaCandidateResponse {
                success: true,
                message: "Abandon intent accepted".to_string(),
                account_id: response.account_id,
                nonce: response.nonce,
                state: response.state.as_str().to_string(),
                abandon_requested_at: response.abandon_requested_at.unwrap_or_default(),
                error_code: String::new(),
            })),
            Err(e) => Ok(Response::new(AbandonDeltaCandidateResponse {
                success: false,
                message: e.to_string(),
                account_id: data.account_id,
                nonce: data.nonce,
                state: String::new(),
                abandon_requested_at: String::new(),
                error_code: e.code().to_string(),
            })),
        }
    }

    /// Resolve a public-key commitment to the set of account IDs that
    /// authorize it. Mirror of HTTP `GET /state/lookup`.
    async fn get_account_by_key_commitment(
        &self,
        request: Request<GetAccountByKeyCommitmentRequest>,
    ) -> Result<Response<GetAccountByKeyCommitmentResponse>, Status> {
        let credentials = authenticated_request(&request)?;
        let req = request.into_inner();

        let params = LookupAccountParams {
            key_commitment: req.key_commitment,
            credentials,
        };

        let result = services::lookup_account(&self.app_state, params)
            .await
            .map_err(Status::from)?;

        Ok(Response::new(GetAccountByKeyCommitmentResponse {
            accounts: result
                .accounts
                .into_iter()
                .map(|account_id| AccountRef { account_id })
                .collect(),
        }))
    }
}

fn authenticated_request<T: Message>(request: &Request<T>) -> Result<Credentials, Status> {
    let request_bytes = request.get_ref().encode_to_vec();
    let request_payload = AuthRequestPayload::from_bytes(&request_bytes);
    Ok(request
        .metadata()
        .extract_credentials()?
        .with_request_payload(request_payload)
        .with_request_payload_bytes(request_bytes))
}

// Helper functions to convert between internal types and protobuf types

/// Project a service-layer history entry into its proto shape. Enum
/// labels (`tag`, `kind`, `section`) cross the wire as the same stable
/// snake_case strings the JSON serialization uses.
fn history_entry_to_proto(entry: services::HistoryEntry) -> HistoryEntry {
    let note_to_proto = |note: crate::delta_summary::DecodedNote| HistoryNote {
        note_id: note.note_id,
        tag: note.tag.as_str().to_string(),
        assets: note
            .assets
            .into_iter()
            .map(|asset| HistoryNoteAsset {
                asset_id: asset.asset_id,
                kind: asset.kind.as_str().to_string(),
                amount: asset.amount,
            })
            .collect(),
        sender: note.sender,
        recipient: note.recipient,
    };
    HistoryEntry {
        nonce: entry.nonce,
        timestamp: entry.timestamp,
        new_commitment: entry.new_commitment,
        input_notes: entry.input_notes.into_iter().map(note_to_proto).collect(),
        output_notes: entry.output_notes.into_iter().map(note_to_proto).collect(),
        decode_warnings: entry
            .decode_warnings
            .into_iter()
            .map(|warning| HistoryDecodeWarning {
                section: warning.section.as_str().to_string(),
                reason: warning.reason,
            })
            .collect(),
    }
}
fn delta_to_proto(delta: &DeltaObject) -> guardian::DeltaObject {
    let (candidate_at, canonical_at, discarded_at) = match &delta.status {
        crate::delta_object::DeltaStatus::Pending { timestamp, .. } => {
            (Some(timestamp.clone()), None, None)
        }
        crate::delta_object::DeltaStatus::Candidate { timestamp, .. } => {
            (Some(timestamp.clone()), None, None)
        }
        crate::delta_object::DeltaStatus::Canonical { timestamp } => {
            (Some(timestamp.clone()), Some(timestamp.clone()), None)
        }
        // No legacy timestamp column carries retention; consumers of the
        // typed status oneof see `retained_at` below.
        crate::delta_object::DeltaStatus::Retained { .. } => (None, None, None),
        crate::delta_object::DeltaStatus::Discarded { timestamp, .. } => {
            (None, None, Some(timestamp.clone()))
        }
    };

    // Build the new status field
    let proto_status = match &delta.status {
        crate::delta_object::DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        } => {
            let proto_cosigner_sigs = cosigner_sigs
                .iter()
                .map(|sig| guardian::CosignerSignature {
                    signer_id: sig.signer_id.clone(),
                    signature: Some(proposal_signature_to_proto(&sig.signature)),
                    timestamp: sig.timestamp.clone(),
                })
                .collect();

            Some(DeltaStatusGrpc {
                status: Some(guardian::delta_status::Status::Pending(
                    guardian::PendingStatus {
                        timestamp: timestamp.clone(),
                        proposer_id: proposer_id.clone(),
                        cosigner_sigs: proto_cosigner_sigs,
                    },
                )),
                discard_reason: String::new(),
                retain_reason: String::new(),
            })
        }
        crate::delta_object::DeltaStatus::Candidate { timestamp, .. } => Some(DeltaStatusGrpc {
            status: Some(guardian::delta_status::Status::CandidateAt(
                timestamp.clone(),
            )),
            discard_reason: String::new(),
            retain_reason: String::new(),
        }),
        crate::delta_object::DeltaStatus::Canonical { timestamp } => Some(DeltaStatusGrpc {
            status: Some(guardian::delta_status::Status::CanonicalAt(
                timestamp.clone(),
            )),
            discard_reason: String::new(),
            retain_reason: String::new(),
        }),
        crate::delta_object::DeltaStatus::Retained { timestamp, reason } => Some(DeltaStatusGrpc {
            status: Some(guardian::delta_status::Status::RetainedAt(
                timestamp.clone(),
            )),
            discard_reason: String::new(),
            retain_reason: match reason {
                Some(crate::delta_object::RetainReason::RetryExhausted) => {
                    "retry_exhausted".to_string()
                }
                Some(crate::delta_object::RetainReason::Diverged) => "diverged".to_string(),
                None => String::new(),
            },
        }),
        crate::delta_object::DeltaStatus::Discarded { timestamp, reason } => {
            Some(DeltaStatusGrpc {
                status: Some(guardian::delta_status::Status::DiscardedAt(
                    timestamp.clone(),
                )),
                discard_reason: match reason {
                    Some(crate::delta_object::DiscardReason::ClientAbandoned) => {
                        "client_abandoned".to_string()
                    }
                    None => String::new(),
                },
                retain_reason: String::new(),
            })
        }
    };

    guardian::DeltaObject {
        account_id: delta.account_id.clone(),
        nonce: delta.nonce,
        prev_commitment: delta.prev_commitment.clone(),
        new_commitment: delta.new_commitment.clone().unwrap_or_default(),
        delta_payload: delta.delta_payload.to_string(),
        ack_sig: delta.ack_sig.clone(),
        candidate_at: candidate_at.unwrap_or_default(),
        canonical_at,
        discarded_at,
        status: proto_status,
        ack_pubkey: Some(delta.ack_pubkey.clone()),
        ack_scheme: Some(delta.ack_scheme.clone()),
    }
}

fn state_to_proto(state: &crate::state_object::StateObject) -> guardian::AccountState {
    guardian::AccountState {
        account_id: state.account_id.clone(),
        state_json: state.state_json.to_string(),
        commitment: state.commitment.clone(),
        created_at: state.created_at.clone(),
        updated_at: state.updated_at.clone(),
        auth_scheme: Some(state.auth_scheme.clone()),
    }
}

fn proposal_signature_to_proto(signature: &ProposalSignature) -> guardian::ProposalSignature {
    match signature {
        ProposalSignature::Falcon { signature } => guardian::ProposalSignature {
            scheme: "falcon".to_string(),
            signature: signature.clone(),
            public_key: None,
        },
        ProposalSignature::Ecdsa {
            signature,
            public_key,
        } => guardian::ProposalSignature {
            scheme: "ecdsa".to_string(),
            signature: signature.clone(),
            public_key: public_key.clone(),
        },
    }
}

#[allow(clippy::result_large_err)]
fn proto_signature_to_internal(
    signature: guardian::ProposalSignature,
) -> Result<ProposalSignature, Status> {
    match signature.scheme.as_str() {
        "falcon" => Ok(ProposalSignature::Falcon {
            signature: signature.signature,
        }),
        "ecdsa" => Ok(ProposalSignature::Ecdsa {
            signature: signature.signature,
            public_key: signature.public_key,
        }),
        other => Err(Status::invalid_argument(format!(
            "Unknown signature scheme: {other}"
        ))),
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
    use crate::testing::fixtures;
    use crate::testing::helpers::{TestSigner, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    use tonic::Request;

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

    fn create_account_metadata(
        account_id: String,
        cosigner_commitments: Vec<String>,
    ) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments,
            },
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
            state_json,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_request_with_auth<T: prost::Message>(
        req: T,
        signer: &TestSigner,
        account_id: &str,
    ) -> Request<T> {
        let request_payload = AuthRequestPayload::from_protobuf_message(&req);
        let (signature, timestamp) = signer.sign_request(account_id, &request_payload);
        let mut request = Request::new(req);
        request
            .metadata_mut()
            .insert("x-pubkey", signer.pubkey_hex.parse().unwrap());
        request
            .metadata_mut()
            .insert("x-signature", signature.parse().unwrap());
        request
            .metadata_mut()
            .insert("x-timestamp", timestamp.to_string().parse().unwrap());
        request
    }

    fn create_service(state: AppState) -> GuardianService {
        GuardianService { app_state: state }
    }

    #[tokio::test]
    async fn test_grpc_get_pubkey() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let service = create_service(state);

        let request = Request::new(guardian::GetPubkeyRequest { scheme: None });
        let response = service.get_pubkey(request).await.unwrap();
        let inner = response.into_inner();

        assert!(!inner.pubkey.is_empty());
        assert!(inner.pubkey.starts_with("0x"));
    }

    #[tokio::test]
    async fn test_grpc_get_pubkey_for_ecdsa() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let service = create_service(state);

        let request = Request::new(guardian::GetPubkeyRequest {
            scheme: Some("ecdsa".to_string()),
        });
        let response = service.get_pubkey(request).await.unwrap();
        let inner = response.into_inner();

        assert!(inner.pubkey.starts_with("0x"));
        assert_eq!(inner.pubkey.len(), 66);
        let raw_pubkey = inner
            .raw_pubkey
            .expect("ecdsa raw pubkey should be present");
        assert!(!raw_pubkey.is_empty());
        assert!(raw_pubkey.starts_with("0x"));
    }

    #[tokio::test]
    async fn test_grpc_configure_success() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let request = guardian::ConfigureRequest {
            account_id: account_id.clone(),
            auth: Some(guardian::AuthConfig {
                auth_type: Some(guardian::auth_config::AuthType::MidenFalconRpo(
                    guardian::MidenFalconRpoAuth {
                        cosigner_commitments: vec![commitment],
                    },
                )),
            }),
            network_config: None,
            initial_state: serde_json::to_string(&account_json).unwrap(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let response = service.configure(request).await.unwrap();
        let inner = response.into_inner();

        assert!(inner.success);
        assert!(!inner.ack_pubkey.is_empty());
        assert!(inner.message.contains("configured successfully"));
    }

    #[tokio::test]
    async fn test_grpc_push_delta_proposal_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();

        // The flow reads metadata more than once; stack one get response per read.
        let _metadata = metadata
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment.clone()],
            ))))
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment],
            ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392".to_string(),
            account_json,
        )));

        let request = guardian::PushDeltaProposalRequest {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload: serde_json::to_string(&serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"],
                "signatures": [],
                "metadata": {
                    "proposal_type": "change_threshold",
                    "target_threshold": 1,
                    "signer_commitments": [signer.commitment_hex.clone()]
                }
            }))
            .unwrap(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let response = service.push_delta_proposal(request).await.unwrap();
        let inner = response.into_inner();

        assert!(inner.success);
        assert!(inner.delta.is_some());
        assert!(!inner.commitment.is_empty());
        assert_eq!(inner.delta.unwrap().nonce, 1);
    }

    fn abandon_grpc_fixtures(
        storage: &MockStorageBackend,
        network: &MockNetworkClient,
        metadata: &MockMetadataStore,
        account_id: &str,
        signer: &TestSigner,
        landed: bool,
    ) {
        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let candidate = crate::delta_object::DeltaObject {
            account_id: account_id.to_string(),
            nonce: 1,
            prev_commitment: "0x123".to_string(),
            new_commitment: Some("0x456".to_string()),
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string()),
            metadata: None,
        };

        let _ = metadata.clone().with_get(Ok(Some(create_account_metadata(
            account_id.to_string(),
            vec![signer.commitment_hex.clone()],
        ))));
        let _ = storage
            .clone()
            .with_pull_delta(Ok(candidate))
            .with_pull_state(Ok(create_state_object(
                account_id.to_string(),
                "0x123".to_string(),
                account_json,
            )));
        let verify = if landed {
            Ok(crate::network::StateVerification::Match)
        } else {
            Ok(crate::network::StateVerification::Mismatch {
                on_chain: "0x123".to_string(),
            })
        };
        let _ = network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({"new": true}), "0x456".to_string())))
            .with_verify_commitment(verify);
    }

    #[tokio::test]
    async fn test_grpc_abandon_delta_candidate_success() {
        let (state, storage, network, metadata) = create_test_state();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        abandon_grpc_fixtures(&storage, &network, &metadata, &account_id, &signer, false);
        let service = create_service(state);

        let request = create_request_with_auth(
            AbandonDeltaCandidateRequest {
                account_id: account_id.clone(),
                nonce: 1,
            },
            &signer,
            &account_id,
        );
        let response = service
            .abandon_delta_candidate(request)
            .await
            .expect("gRPC call should succeed")
            .into_inner();

        assert!(response.success, "expected success: {}", response.message);
        assert_eq!(response.account_id, account_id);
        assert_eq!(response.nonce, 1);
        assert_eq!(response.state, "pending");
        assert!(!response.abandon_requested_at.is_empty());
        assert!(response.error_code.is_empty());
        // Intent only: nothing is deleted at request time.
        assert!(storage.get_delete_delta_calls().is_empty());
        let _ = account_id;
    }

    #[tokio::test]
    async fn test_grpc_abandon_delta_candidate_landed_error_code() {
        let (state, storage, network, metadata) = create_test_state();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        abandon_grpc_fixtures(&storage, &network, &metadata, &account_id, &signer, true);
        let service = create_service(state);

        let request = create_request_with_auth(
            AbandonDeltaCandidateRequest {
                account_id: account_id.clone(),
                nonce: 1,
            },
            &signer,
            &account_id,
        );
        let response = service
            .abandon_delta_candidate(request)
            .await
            .expect("gRPC transport should not error")
            .into_inner();

        assert!(!response.success);
        assert_eq!(response.error_code, "GUARDIAN_CANDIDATE_LANDED");
        assert!(response.state.is_empty());
        assert!(storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_grpc_push_delta_proposal_missing_tx_summary() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let request = guardian::PushDeltaProposalRequest {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload: serde_json::to_string(&serde_json::json!({
                "signatures": [],
                "metadata": {
                    "proposal_type": "change_threshold",
                    "target_threshold": 1,
                    "signer_commitments": [signer.commitment_hex.clone()]
                }
            }))
            .unwrap(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let status = service
            .push_delta_proposal(request)
            .await
            .expect_err("missing tx summary must surface as a gRPC Status");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("Status.details is JSON");
        assert!(details["code"].is_string());
        assert!(details["message"].is_string());
        assert!(details["meta"]["retryable"].is_boolean());
    }

    #[tokio::test]
    async fn test_grpc_get_delta_proposals_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        // The flow reads metadata more than once; stack one get response per read.
        let _metadata = metadata
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment.clone()],
            ))))
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment.clone()],
            ))));

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let pending_delta = DeltaObject {
            account_id: account_id.clone(),
            nonce: 1,
            prev_commitment: "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392"
                .to_string(),
            new_commitment: None,
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::pending(
                "2024-11-14T12:00:00Z".to_string(),
                signer.pubkey_hex.clone(),
            ),
            metadata: None,
        };

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![pending_delta]));

        let request = guardian::GetDeltaProposalsRequest {
            account_id: account_id.clone(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let response = service.get_delta_proposals(request).await.unwrap();
        let inner = response.into_inner();

        assert!(inner.success);
        assert_eq!(inner.proposals.len(), 1);
    }

    #[tokio::test]
    async fn test_grpc_get_delta_proposals_empty() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        // The flow reads metadata more than once; stack one get response per read.
        let _metadata = metadata
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment.clone()],
            ))))
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment],
            ))));

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![]));

        let request = guardian::GetDeltaProposalsRequest {
            account_id: account_id.clone(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let response = service.get_delta_proposals(request).await.unwrap();
        let inner = response.into_inner();

        assert!(inner.success);
        assert_eq!(inner.proposals.len(), 0);
    }

    #[tokio::test]
    async fn test_grpc_get_delta_proposal_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment.clone()],
            ))))
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment],
            ))));

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let pending_delta = DeltaObject {
            account_id: account_id.clone(),
            nonce: 1,
            prev_commitment: "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392"
                .to_string(),
            new_commitment: None,
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::pending(
                "2024-11-14T12:00:00Z".to_string(),
                signer.pubkey_hex.clone(),
            ),
            metadata: None,
        };

        let _storage = storage.with_pull_delta_proposal(Ok(pending_delta));

        let request = guardian::GetDeltaProposalRequest {
            account_id: account_id.clone(),
            commitment: "0xproposal".to_string(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let response = service.get_delta_proposal(request).await.unwrap();
        let inner = response.into_inner();

        assert!(inner.success);
        assert!(inner.proposal.is_some());
        assert_eq!(inner.proposal.unwrap().nonce, 1);
    }

    #[tokio::test]
    async fn test_grpc_get_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment.clone()],
            ))))
            .with_get(Ok(Some(create_account_metadata(
                account_id.clone(),
                vec![commitment],
            ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let request = guardian::GetDeltaProposalRequest {
            account_id: account_id.clone(),
            commitment: "0xmissing".to_string(),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let status = service
            .get_delta_proposal(request)
            .await
            .expect_err("unknown proposal must surface as a gRPC Status");
        assert_eq!(status.code(), tonic::Code::NotFound);
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("Status.details is JSON");
        assert_eq!(details["code"], "proposal_not_found");
        assert!(details["message"].is_string());
        assert_eq!(details["meta"]["retryable"], serde_json::Value::Bool(false));
    }

    #[tokio::test]
    async fn test_grpc_get_delta_proposal_unauthorized() {
        let (state, _storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer.commitment_hex.clone()],
        ))));

        let request = guardian::GetDeltaProposalRequest {
            account_id: account_id.clone(),
            commitment: "0xproposal".to_string(),
        };

        let mut request = create_request_with_auth(request, &signer, &account_id);
        request
            .metadata_mut()
            .insert("x-signature", "0xdeadbeef".parse().unwrap());
        let status = service
            .get_delta_proposal(request)
            .await
            .expect_err("bad signature must surface as a gRPC Status");
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("Status.details is JSON");
        assert_eq!(details["code"], "authentication_failed");
        assert!(details["message"].is_string());
        assert_eq!(details["meta"]["retryable"], serde_json::Value::Bool(false));
    }

    #[tokio::test]
    async fn test_grpc_sign_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let service = create_service(state);

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let request = guardian::SignDeltaProposalRequest {
            account_id: account_id.clone(),
            commitment: "nonexistent_proposal".to_string(),
            signature: Some(guardian::ProposalSignature {
                scheme: "falcon".to_string(),
                signature: dummy_sig,
                public_key: None,
            }),
        };

        let request = create_request_with_auth(request, &signer, &account_id);
        let status = service
            .sign_delta_proposal(request)
            .await
            .expect_err("unknown proposal must surface as a gRPC Status");
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("Status.details is JSON");
        assert!(details["code"].is_string());
        assert!(details["message"].is_string());
        assert!(details["meta"]["retryable"].is_boolean());
    }

    #[test]
    fn delta_to_proto_encodes_retained_status_and_reason() {
        // The wire contract for issue #345: retained rows ride the typed
        // status oneof (`retained_at` + `retain_reason`) and deliberately
        // populate none of the legacy timestamp columns, so pre-retained
        // consumers see an in-progress delta rather than a wrong terminal
        // state.
        let delta = DeltaObject {
            account_id: "0xtest_account".to_string(),
            nonce: 7,
            prev_commitment: "0xprev".to_string(),
            new_commitment: Some("0xnew".to_string()),
            delta_payload: serde_json::json!({"test": "payload"}),
            ack_sig: "0xsig".to_string(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::retained(
                "2026-07-23T00:00:00Z".to_string(),
                crate::delta_object::RetainReason::Diverged,
            ),
            metadata: None,
        };

        let proto = delta_to_proto(&delta);

        let status = proto.status.expect("typed status present");
        assert_eq!(
            status.status,
            Some(guardian::delta_status::Status::RetainedAt(
                "2026-07-23T00:00:00Z".to_string()
            ))
        );
        assert_eq!(status.retain_reason, "diverged");
        assert_eq!(status.discard_reason, "");
        assert_eq!(proto.candidate_at, "");
        assert_eq!(proto.canonical_at, None);
        assert_eq!(proto.discarded_at, None);

        // A reasonless retained row encodes an empty reason string.
        let mut reasonless = delta;
        reasonless.status = DeltaStatus::Retained {
            timestamp: "2026-07-23T00:00:00Z".to_string(),
            reason: None,
        };
        let proto = delta_to_proto(&reasonless);
        assert_eq!(
            proto.status.expect("typed status present").retain_reason,
            ""
        );
    }
}
