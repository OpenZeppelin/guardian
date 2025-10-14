use crate::auth::AuthType;
use crate::services::{self, AuthData, ConfigureAccountParams, PushDeltaParams, GetDeltaParams, GetDeltaHeadParams, GetStateParams};
use crate::state::AppState;
use crate::storage::DeltaObject;
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

/// Extract authentication data from gRPC metadata
fn extract_auth(metadata: &tonic::metadata::MetadataMap) -> Result<AuthData, Status> {
    let pubkey = metadata
        .get("x-pubkey")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Status::invalid_argument("Missing or invalid x-pubkey metadata"))?
        .to_string();

    let signature = metadata
        .get("x-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Status::invalid_argument("Missing or invalid x-signature metadata"))?
        .to_string();

    Ok(AuthData { pubkey, signature })
}

#[tonic::async_trait]
impl StateManager for StateManagerService {
    async fn configure(
        &self,
        request: Request<ConfigureRequest>,
    ) -> Result<Response<ConfigureResponse>, Status> {
        let req = request.into_inner();

        // Parse auth_type
        let auth_type: AuthType = serde_json::from_str(&format!("\"{}\"", req.auth_type))
            .map_err(|e| Status::invalid_argument(format!("Invalid auth_type: {}", e)))?;

        // Parse initial_state JSON
        let initial_state: serde_json::Value = serde_json::from_str(&req.initial_state)
            .map_err(|e| Status::invalid_argument(format!("Invalid initial_state JSON: {}", e)))?;

        let params = ConfigureAccountParams {
            account_id: req.account_id.clone(),
            auth_type,
            initial_state,
            storage_type: req.storage_type,
            cosigner_pubkeys: req.cosigner_pubkeys,
        };

        // Call service layer
        match services::configure_account(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", response.account_id),
            })),
            Err(e) => Ok(Response::new(ConfigureResponse {
                success: false,
                message: e.message,
            })),
        }
    }

    async fn push_delta(
        &self,
        request: Request<PushDeltaRequest>,
    ) -> Result<Response<PushDeltaResponse>, Status> {
        // Extract authentication data from metadata
        let auth = extract_auth(request.metadata())?;

        let req = request.into_inner();

        // Parse delta_payload JSON
        let delta_payload: serde_json::Value = serde_json::from_str(&req.delta_payload)
            .map_err(|e| Status::invalid_argument(format!("Invalid delta_payload JSON: {}", e)))?;

        // Convert proto request to internal DeltaObject
        let delta = DeltaObject {
            account_id: req.account_id,
            nonce: req.nonce,
            prev_commitment: req.prev_commitment,
            delta_hash: req.delta_hash,
            delta_payload,
            ack_sig: req.ack_sig,
            candidate_at: req.candidate_at,
            canonical_at: req.canonical_at,
            discarded_at: req.discarded_at,
        };

        let params = PushDeltaParams { delta, auth };

        // Call service layer
        match services::push_delta(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(PushDeltaResponse {
                success: true,
                message: "Delta pushed successfully".to_string(),
                delta: Some(delta_to_proto(&response.delta)),
            })),
            Err(e) => Ok(Response::new(PushDeltaResponse {
                success: false,
                message: e.message,
                delta: None,
            })),
        }
    }

    async fn get_delta(
        &self,
        request: Request<GetDeltaRequest>,
    ) -> Result<Response<GetDeltaResponse>, Status> {
        // Extract authentication data from metadata
        let auth = extract_auth(request.metadata())?;

        let req = request.into_inner();

        let params = GetDeltaParams {
            account_id: req.account_id,
            nonce: req.nonce,
            auth,
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
                message: e.message,
                delta: None,
            })),
        }
    }

    async fn get_delta_head(
        &self,
        request: Request<GetDeltaHeadRequest>,
    ) -> Result<Response<GetDeltaHeadResponse>, Status> {
        // Extract authentication data from metadata
        let auth = extract_auth(request.metadata())?;

        let req = request.into_inner();

        let params = GetDeltaHeadParams {
            account_id: req.account_id,
            auth,
        };

        // Call service layer
        match services::get_delta_head(&self.app_state, params).await {
            Ok(response) => Ok(Response::new(GetDeltaHeadResponse {
                success: true,
                message: "Latest delta retrieved successfully".to_string(),
                latest_nonce: Some(response.delta.nonce),
            })),
            Err(e) => Ok(Response::new(GetDeltaHeadResponse {
                success: false,
                message: e.message,
                latest_nonce: None,
            })),
        }
    }

    async fn get_state(
        &self,
        request: Request<GetStateRequest>,
    ) -> Result<Response<GetStateResponse>, Status> {
        // Extract authentication data from metadata
        let auth = extract_auth(request.metadata())?;

        let req = request.into_inner();

        let params = GetStateParams {
            account_id: req.account_id,
            auth,
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
                message: e.message,
                state: None,
            })),
        }
    }
}

// Helper functions to convert between internal types and protobuf types
fn delta_to_proto(delta: &DeltaObject) -> state_manager::DeltaObject {
    state_manager::DeltaObject {
        account_id: delta.account_id.clone(),
        nonce: delta.nonce,
        prev_commitment: delta.prev_commitment.clone(),
        delta_hash: delta.delta_hash.clone(),
        delta_payload: delta.delta_payload.to_string(),
        ack_sig: delta.ack_sig.clone(),
        candidate_at: delta.candidate_at.clone(),
        canonical_at: delta.canonical_at.clone(),
        discarded_at: delta.discarded_at.clone(),
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
