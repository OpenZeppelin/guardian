use crate::auth::AuthType;
use crate::services;
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
fn extract_auth(metadata: &tonic::metadata::MetadataMap) -> Result<(String, String), Status> {
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

    Ok((pubkey, signature))
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

        // Call service layer
        match services::configure_account(
            &self.app_state,
            req.account_id.clone(),
            auth_type,
            initial_state,
            req.storage_type,
            req.cosigner_pubkeys,
        )
        .await
        {
            Ok(_) => Ok(Response::new(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", req.account_id),
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
        let (pubkey, signature) = extract_auth(request.metadata())?;

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

        // Call service layer
        match services::push_delta(&self.app_state, delta, pubkey, signature).await {
            Ok(delta) => Ok(Response::new(PushDeltaResponse {
                success: true,
                message: "Delta pushed successfully".to_string(),
                delta: Some(delta_to_proto(&delta)),
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
        let (pubkey, signature) = extract_auth(request.metadata())?;

        let req = request.into_inner();

        // Call service layer
        match services::get_delta(&self.app_state, &req.account_id, req.nonce, pubkey, signature).await {
            Ok(delta) => Ok(Response::new(GetDeltaResponse {
                success: true,
                message: "Delta retrieved successfully".to_string(),
                delta: Some(delta_to_proto(&delta)),
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
        let (pubkey, signature) = extract_auth(request.metadata())?;

        let req = request.into_inner();

        // Call service layer
        match services::get_latest_nonce(&self.app_state, &req.account_id, pubkey, signature).await {
            Ok(latest_nonce) => Ok(Response::new(GetDeltaHeadResponse {
                success: true,
                message: if latest_nonce.is_some() {
                    "Latest nonce retrieved successfully".to_string()
                } else {
                    "No deltas found for account".to_string()
                },
                latest_nonce,
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
        let (pubkey, signature) = extract_auth(request.metadata())?;

        let req = request.into_inner();

        // Call service layer
        match services::get_state(&self.app_state, &req.account_id, pubkey, signature).await {
            Ok(state) => Ok(Response::new(GetStateResponse {
                success: true,
                message: "State retrieved successfully".to_string(),
                state: Some(state_to_proto(&state)),
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
