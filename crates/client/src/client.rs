use crate::auth::Auth;
use crate::error::{ClientError, ClientResult};
use crate::keystore::Signer;
use crate::proto::state_manager_client::StateManagerClient;
use crate::proto::{
    AuthConfig, ConfigureRequest, ConfigureResponse, GetDeltaProposalRequest,
    GetDeltaProposalResponse, GetDeltaProposalsRequest, GetDeltaProposalsResponse, GetDeltaRequest,
    GetDeltaResponse, GetDeltaSinceRequest, GetDeltaSinceResponse, GetPubkeyRequest,
    GetStateRequest, GetStateResponse, ProposalSignature as ProtoProposalSignature,
    PushDeltaProposalRequest, PushDeltaProposalResponse, PushDeltaRequest, PushDeltaResponse,
    SignDeltaProposalRequest, SignDeltaProposalResponse,
};
use chrono::Utc;
use miden_protocol::account::AccountId;
use private_state_manager_shared::ProposalSignature as JsonProposalSignature;
use private_state_manager_shared::auth_request_message::AuthRequestMessage;
use private_state_manager_shared::auth_request_payload::AuthRequestPayload;
use private_state_manager_shared::hex::IntoHex;
use std::sync::Arc;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

/// A client for interacting with Private State Manager (PSM) servers.
///
/// `PsmClient` provides methods for managing off-chain account state, including:
/// - Account configuration
/// - Delta (state change) management
/// - Multi-party proposal workflows
///
/// All methods that interact with account data require authentication via a configured signer.
pub struct PsmClient {
    client: StateManagerClient<Channel>,
    auth: Option<Auth>,
    signer: Option<Arc<dyn Signer>>,
}

impl PsmClient {
    /// Creates a new client connected to the specified PSM server endpoint.
    ///
    /// # Arguments
    /// * `endpoint` - The gRPC endpoint URL (e.g., "http://localhost:50051")
    pub async fn connect(endpoint: impl Into<String>) -> ClientResult<Self> {
        let endpoint = endpoint.into();
        let client = StateManagerClient::connect(endpoint).await?;
        Ok(Self {
            client,
            auth: None,
            signer: None,
        })
    }

    /// Configures scheme-aware authentication for authenticated PSM requests.
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Configures the signer used for authenticated PSM requests.
    pub fn with_signer(mut self, signer: Arc<dyn Signer>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Returns the hex-encoded public key of the configured auth or signer.
    pub fn auth_pubkey_hex(&self) -> Result<String, ClientError> {
        self.auth
            .as_ref()
            .map(|auth| auth.public_key_hex())
            .or_else(|| self.signer.as_ref().map(|signer| signer.public_key_hex()))
            .ok_or_else(|| {
                ClientError::InvalidResponse("PSM client has no signer configured".to_string())
            })
    }

    /// Returns the hex-encoded public key of the configured signer, if any.
    pub fn signer_pubkey_hex(&self) -> Result<String, ClientError> {
        self.auth_pubkey_hex()
    }

    fn add_auth_metadata(
        &self,
        request: &mut tonic::Request<impl prost::Message + std::fmt::Debug>,
        account_id: &AccountId,
    ) -> ClientResult<()> {
        if let Some(auth) = &self.auth {
            let pubkey_hex = auth.public_key_hex();
            let timestamp = Utc::now().timestamp_millis();
            let request_payload = AuthRequestPayload::from_protobuf_message(request.get_ref());
            let signature_hex = auth.sign_request_message(account_id, timestamp, request_payload);

            let pubkey_metadata = MetadataValue::try_from(&pubkey_hex)
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid pubkey: {e}")))?;
            let signature_metadata = MetadataValue::try_from(&signature_hex)
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid signature: {e}")))?;
            let timestamp_metadata = MetadataValue::try_from(timestamp.to_string())
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid timestamp: {e}")))?;

            request.metadata_mut().insert("x-pubkey", pubkey_metadata);
            request
                .metadata_mut()
                .insert("x-signature", signature_metadata);
            request
                .metadata_mut()
                .insert("x-timestamp", timestamp_metadata);
        } else if let Some(signer) = &self.signer {
            let pubkey_hex = signer.public_key_hex();
            let timestamp = Utc::now().timestamp_millis();
            let request_payload = AuthRequestPayload::from_protobuf_message(request.get_ref());
            let auth_message =
                AuthRequestMessage::new(*account_id, timestamp, request_payload).to_word();
            let signature_hex = signer.sign_word(auth_message).into_hex();

            let pubkey_metadata = MetadataValue::try_from(&pubkey_hex)
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid pubkey: {e}")))?;
            let signature_metadata = MetadataValue::try_from(&signature_hex)
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid signature: {e}")))?;
            let timestamp_metadata = MetadataValue::try_from(timestamp.to_string())
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid timestamp: {e}")))?;

            request.metadata_mut().insert("x-pubkey", pubkey_metadata);
            request
                .metadata_mut()
                .insert("x-signature", signature_metadata);
            request
                .metadata_mut()
                .insert("x-timestamp", timestamp_metadata);
        }
        Ok(())
    }

    /// Configure a new account
    ///
    /// # Arguments
    pub async fn configure(
        &mut self,
        account_id: &AccountId,
        auth: AuthConfig,
        initial_state: impl serde::Serialize,
    ) -> ClientResult<ConfigureResponse> {
        let initial_state_json = serde_json::to_string(&initial_state)?;

        let mut request = tonic::Request::new(ConfigureRequest {
            account_id: account_id.to_string(),
            auth: Some(auth),
            initial_state: initial_state_json,
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.configure(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Pushes a delta (state change) to the PSM server.
    ///
    /// This makes the delta canonical and triggers the canonicalization workflow.
    pub async fn push_delta(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
        prev_commitment: impl Into<String>,
        delta_payload: impl serde::Serialize,
    ) -> ClientResult<PushDeltaResponse> {
        let delta_payload_json = serde_json::to_string(&delta_payload)?;

        let mut request = tonic::Request::new(PushDeltaRequest {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: prev_commitment.into(),
            delta_payload: delta_payload_json,
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.push_delta(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves a specific delta by nonce.
    pub async fn get_delta(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
    ) -> ClientResult<GetDeltaResponse> {
        let mut request = tonic::Request::new(GetDeltaRequest {
            account_id: account_id.to_string(),
            nonce,
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.get_delta(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves all deltas starting from a given nonce.
    pub async fn get_delta_since(
        &mut self,
        account_id: &AccountId,
        from_nonce: u64,
    ) -> ClientResult<GetDeltaSinceResponse> {
        let mut request = tonic::Request::new(GetDeltaSinceRequest {
            account_id: account_id.to_string(),
            from_nonce,
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.get_delta_since(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves the current state for an account.
    pub async fn get_state(&mut self, account_id: &AccountId) -> ClientResult<GetStateResponse> {
        let mut request = tonic::Request::new(GetStateRequest {
            account_id: account_id.to_string(),
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.get_state(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves the PSM server's public key commitment (and optionally the raw public key).
    pub async fn get_pubkey(
        &mut self,
        scheme: Option<&str>,
    ) -> ClientResult<(String, Option<String>)> {
        let request = tonic::Request::new(GetPubkeyRequest {
            scheme: scheme.map(|s| s.to_string()),
        });
        let response = self.client.get_pubkey(request).await?;
        let inner = response.into_inner();
        Ok((inner.pubkey, inner.raw_pubkey))
    }

    /// Push a delta proposal
    pub async fn push_delta_proposal(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
        delta_payload: impl serde::Serialize,
    ) -> ClientResult<PushDeltaProposalResponse> {
        let delta_payload_json = serde_json::to_string(&delta_payload)?;

        let mut request = tonic::Request::new(PushDeltaProposalRequest {
            account_id: account_id.to_string(),
            nonce,
            delta_payload: delta_payload_json,
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.push_delta_proposal(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Get all delta proposals for an account
    pub async fn get_delta_proposals(
        &mut self,
        account_id: &AccountId,
    ) -> ClientResult<GetDeltaProposalsResponse> {
        let mut request = tonic::Request::new(GetDeltaProposalsRequest {
            account_id: account_id.to_string(),
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.get_delta_proposals(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Get a specific delta proposal for an account by commitment.
    pub async fn get_delta_proposal(
        &mut self,
        account_id: &AccountId,
        commitment: impl Into<String>,
    ) -> ClientResult<GetDeltaProposalResponse> {
        let mut request = tonic::Request::new(GetDeltaProposalRequest {
            account_id: account_id.to_string(),
            commitment: commitment.into(),
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.get_delta_proposal(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Sign a delta proposal
    pub async fn sign_delta_proposal(
        &mut self,
        account_id: &AccountId,
        commitment: impl Into<String>,
        signature: JsonProposalSignature,
    ) -> ClientResult<SignDeltaProposalResponse> {
        let proto_signature = Some(proto_signature_from_json(&signature));

        let mut request = tonic::Request::new(SignDeltaProposalRequest {
            account_id: account_id.to_string(),
            commitment: commitment.into(),
            signature: proto_signature,
        });

        self.add_auth_metadata(&mut request, account_id)?;

        let response = self.client.sign_delta_proposal(request).await?;
        let inner = response.into_inner();

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }
}

fn proto_signature_from_json(signature: &JsonProposalSignature) -> ProtoProposalSignature {
    match signature {
        JsonProposalSignature::Falcon { signature } => ProtoProposalSignature {
            scheme: "falcon".to_string(),
            signature: signature.clone(),
            public_key: None,
        },
        JsonProposalSignature::Ecdsa {
            signature,
            public_key,
        } => ProtoProposalSignature {
            scheme: "ecdsa".to_string(),
            signature: signature.clone(),
            public_key: public_key.clone(),
        },
    }
}
