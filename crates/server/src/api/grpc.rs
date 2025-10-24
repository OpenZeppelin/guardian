use crate::metadata::auth::{Auth, ExtractCredentials};
use crate::services::{
    self, ConfigureAccountParams, GetDeltaParams, GetStateParams, PushDeltaParams,
};
use crate::state::AppState;
use crate::storage::{DeltaObject, StorageType};
use tonic::{Request, Response, Status};

// Include the generated protobuf code
pub mod state_manager {
    tonic::include_proto!("state_manager");

    // Include the file descriptor set for reflection
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        include_bytes!("../../proto/state_manager_descriptor.bin");
}

use state_manager::state_manager_server::StateManager;
use state_manager::*;

pub struct StateManagerService {
    pub app_state: AppState,
}

#[tonic::async_trait]
impl StateManager for StateManagerService {
    async fn configure(
        &self,
        request: Request<ConfigureRequest>,
    ) -> Result<Response<ConfigureResponse>, Status> {
        // Extract credentials from metadata
        let credential = request.metadata().extract_credentials()?;

        let req = request.into_inner();

        // Parse auth from proto AuthConfig
        let auth_config = req
            .auth
            .ok_or_else(|| Status::invalid_argument("Missing auth configuration"))?;

        let auth = Auth::try_from(auth_config)
            .map_err(|e| Status::invalid_argument(format!("Invalid auth config: {e}")))?;

        // Parse storage_type
        let storage_type: StorageType = serde_json::from_str(&format!("\"{}\"", req.storage_type))
            .map_err(|e| Status::invalid_argument(format!("Invalid storage type: {e}")))?;

        // Parse initial_state JSON
        let initial_state: serde_json::Value = serde_json::from_str(&req.initial_state)
            .map_err(|e| Status::invalid_argument(format!("Invalid initial_state JSON: {e}")))?;

        let params = ConfigureAccountParams {
            account_id: req.account_id.clone(),
            auth,
            initial_state,
            storage_type,
            credential,
        };

        // Call service layer
        match services::configure_account(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", response.account_id),
                ack_pubkey: response.ack_pubkey,
            })),
            Err(e) => Ok(Response::new(ConfigureResponse {
                success: false,
                message: e.to_string(),
                ack_pubkey: String::new(),
            })),
        }
    }

    async fn push_delta(
        &self,
        request: Request<PushDeltaRequest>,
    ) -> Result<Response<PushDeltaResponse>, Status> {
        // Extract authentication data from metadata
        let auth = request.metadata().extract_credentials()?;

        let req = request.into_inner();

        // Parse delta_payload JSON
        let delta_payload: serde_json::Value = serde_json::from_str(&req.delta_payload)
            .map_err(|e| Status::invalid_argument(format!("Invalid delta_payload JSON: {e}")))?;

        let delta = DeltaObject {
            account_id: req.account_id,
            nonce: req.nonce,
            prev_commitment: req.prev_commitment,
            new_commitment: String::new(),
            delta_payload,
            ack_sig: None,
            status: Default::default(),
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
                ack_sig: response.delta.ack_sig,
            })),
            Err(e) => Ok(Response::new(PushDeltaResponse {
                success: false,
                message: e.to_string(),
                delta: None,
                ack_sig: None,
            })),
        }
    }

    async fn get_delta(
        &self,
        request: Request<GetDeltaRequest>,
    ) -> Result<Response<GetDeltaResponse>, Status> {
        // Extract authentication data from metadata
        let auth = request.metadata().extract_credentials()?;

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
            })),
            Err(e) => Ok(Response::new(GetDeltaResponse {
                success: false,
                message: e.to_string(),
                delta: None,
            })),
        }
    }

    async fn get_delta_since(
        &self,
        request: Request<GetDeltaSinceRequest>,
    ) -> Result<Response<GetDeltaSinceResponse>, Status> {
        // Extract authentication data from metadata
        let auth = request.metadata().extract_credentials()?;

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
            })),
            Err(e) => Ok(Response::new(GetDeltaSinceResponse {
                success: false,
                message: e.to_string(),
                merged_delta: None,
            })),
        }
    }

    async fn get_state(
        &self,
        request: Request<GetStateRequest>,
    ) -> Result<Response<GetStateResponse>, Status> {
        // Extract authentication data from metadata
        let auth = request.metadata().extract_credentials()?;

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
            })),
            Err(e) => Ok(Response::new(GetStateResponse {
                success: false,
                message: e.to_string(),
                state: None,
            })),
        }
    }
}

// Helper functions to convert between internal types and protobuf types
fn delta_to_proto(delta: &DeltaObject) -> state_manager::DeltaObject {
    let (candidate_at, canonical_at, discarded_at) = match &delta.status {
        crate::storage::DeltaStatus::Candidate { timestamp } => {
            (Some(timestamp.clone()), None, None)
        }
        crate::storage::DeltaStatus::Canonical { timestamp } => {
            (Some(timestamp.clone()), Some(timestamp.clone()), None)
        }
        crate::storage::DeltaStatus::Discarded { timestamp } => {
            (None, None, Some(timestamp.clone()))
        }
    };

    state_manager::DeltaObject {
        account_id: delta.account_id.clone(),
        nonce: delta.nonce,
        prev_commitment: delta.prev_commitment.clone(),
        new_commitment: delta.new_commitment.clone(),
        delta_payload: delta.delta_payload.to_string(),
        ack_sig: delta.ack_sig.clone().unwrap_or_default(),
        candidate_at: candidate_at.unwrap_or_default(),
        canonical_at,
        discarded_at,
    }
}

fn state_to_proto(state: &crate::storage::AccountState) -> state_manager::AccountState {
    state_manager::AccountState {
        account_id: state.account_id.clone(),
        state_json: state.state_json.to_string(),
        commitment: state.commitment.clone(),
        created_at: state.created_at.clone(),
        updated_at: state.updated_at.clone(),
    }
}
