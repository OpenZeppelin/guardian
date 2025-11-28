use crate::auth::Auth;
use crate::error::{ClientError, ClientResult};
use crate::proto::state_manager_client::StateManagerClient;
use crate::proto::{
    AuthConfig, ConfigureRequest, ConfigureResponse, GetDeltaProposalsRequest,
    GetDeltaProposalsResponse, GetDeltaRequest, GetDeltaResponse, GetDeltaSinceRequest,
    GetDeltaSinceResponse, GetPubkeyRequest, GetStateRequest, GetStateResponse,
    ProposalSignature as ProtoProposalSignature, PushDeltaProposalRequest,
    PushDeltaProposalResponse, PushDeltaRequest, PushDeltaResponse, SignDeltaProposalRequest,
    SignDeltaProposalResponse,
};
use miden_objects::account::AccountId;
use private_state_manager_shared::ProposalSignature as JsonProposalSignature;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

/// A client for interacting with Private State Manager (PSM) servers.
///
/// `PsmClient` provides methods for managing off-chain account state, including:
/// - Account configuration
/// - Delta (state change) management
/// - Multi-party proposal workflows
///
/// All methods that interact with account data require authentication via [`Auth`].
pub struct PsmClient {
    client: StateManagerClient<Channel>,
    auth: Option<Auth>,
}

impl PsmClient {
    /// Creates a new client connected to the specified PSM server endpoint.
    ///
    /// # Arguments
    /// * `endpoint` - The gRPC endpoint URL (e.g., "http://localhost:50051")
    pub async fn connect(endpoint: impl Into<String>) -> ClientResult<Self> {
        let endpoint = endpoint.into();
        let client = StateManagerClient::connect(endpoint).await?;
        Ok(Self { client, auth: None })
    }

    /// Configures authentication for this client.
    ///
    /// Authentication is required for all account operations.
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Returns the hex-encoded public key of the configured auth, if any.
    pub fn auth_pubkey_hex(&self) -> Result<String, ClientError> {
        self.auth
            .as_ref()
            .map(|auth| auth.public_key_hex())
            .ok_or_else(|| {
                ClientError::InvalidResponse("PSM client has no auth configured".to_string())
            })
    }

    fn add_auth_metadata(
        &self,
        request: &mut tonic::Request<impl std::fmt::Debug>,
        account_id: &AccountId,
    ) -> ClientResult<()> {
        if let Some(auth) = &self.auth {
            let pubkey_hex = auth.public_key_hex();
            let signature_hex = auth.sign_account_id(account_id);

            let pubkey_metadata = MetadataValue::try_from(&pubkey_hex)
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid pubkey: {e}")))?;
            let signature_metadata = MetadataValue::try_from(&signature_hex)
                .map_err(|e| ClientError::InvalidResponse(format!("Invalid signature: {e}")))?;

            request.metadata_mut().insert("x-pubkey", pubkey_metadata);
            request
                .metadata_mut()
                .insert("x-signature", signature_metadata);
        }
        Ok(())
    }

    /// Configure a new account
    ///
    /// # Arguments
    /// * `storage_type` - Storage backend type (e.g., "Filesystem")
    pub async fn configure(
        &mut self,
        account_id: &AccountId,
        auth: AuthConfig,
        initial_state: impl serde::Serialize,
        storage_type: impl Into<String>,
    ) -> ClientResult<ConfigureResponse> {
        let initial_state_json = serde_json::to_string(&initial_state)?;

        let mut request = tonic::Request::new(ConfigureRequest {
            account_id: account_id.to_string(),
            auth: Some(auth),
            initial_state: initial_state_json,
            storage_type: storage_type.into(),
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

    /// Retrieves the PSM server's public key (commitment hex).
    pub async fn get_pubkey(&mut self) -> ClientResult<String> {
        let request = tonic::Request::new(GetPubkeyRequest {});
        let response = self.client.get_pubkey(request).await?;
        let inner = response.into_inner();
        Ok(inner.pubkey)
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
        },
    }
}
