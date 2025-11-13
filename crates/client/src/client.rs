use crate::auth::Auth;
use crate::error::{ClientError, ClientResult};
use crate::proto::state_manager_client::StateManagerClient;
use crate::proto::{
    AuthConfig, ConfigureRequest, ConfigureResponse, GetDeltaProposalsRequest,
    GetDeltaProposalsResponse, GetDeltaRequest, GetDeltaResponse, GetDeltaSinceRequest,
    GetDeltaSinceResponse, GetPubkeyRequest, GetStateRequest, GetStateResponse,
    PushDeltaProposalRequest, PushDeltaProposalResponse, PushDeltaRequest, PushDeltaResponse,
    SignDeltaProposalRequest, SignDeltaProposalResponse,
};
use miden_objects::account::AccountId;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

pub struct PsmClient {
    client: StateManagerClient<Channel>,
    auth: Option<Auth>,
}

impl PsmClient {
    pub async fn connect(endpoint: impl Into<String>) -> ClientResult<Self> {
        let endpoint = endpoint.into();
        let client = StateManagerClient::connect(endpoint).await?;
        Ok(Self { client, auth: None })
    }

    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

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
        signature_scheme: impl Into<String>,
        signature: impl Into<String>,
    ) -> ClientResult<SignDeltaProposalResponse> {
        let mut request = tonic::Request::new(SignDeltaProposalRequest {
            account_id: account_id.to_string(),
            commitment: commitment.into(),
            signature_scheme: signature_scheme.into(),
            signature: signature.into(),
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
