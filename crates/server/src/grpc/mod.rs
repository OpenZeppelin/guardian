use crate::state::AppState;
use crate::storage::DeltaObject;
use tonic::{Request, Response, Status};
// use crate::services;

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
        _request: Request<ConfigureRequest>,
    ) -> Result<Response<ConfigureResponse>, Status> {
        // TODO: Implement
        Ok(Response::new(ConfigureResponse {
            success: false,
            message: "Not implemented".to_string(),
        }))
    }

    async fn push_delta(
        &self,
        _request: Request<PushDeltaRequest>,
    ) -> Result<Response<PushDeltaResponse>, Status> {
        // TODO: Implement
        Ok(Response::new(PushDeltaResponse {
            success: false,
            message: "Not implemented".to_string(),
            delta: None,
        }))
    }

    async fn get_delta(
        &self,
        _request: Request<GetDeltaRequest>,
    ) -> Result<Response<GetDeltaResponse>, Status> {
        // TODO: Implement
        Ok(Response::new(GetDeltaResponse {
            success: false,
            message: "Not implemented".to_string(),
            delta: None,
        }))
    }

    async fn get_delta_head(
        &self,
        _request: Request<GetDeltaHeadRequest>,
    ) -> Result<Response<GetDeltaHeadResponse>, Status> {
        // TODO: Implement
        Ok(Response::new(GetDeltaHeadResponse {
            success: false,
            message: "Not implemented".to_string(),
            latest_nonce: None,
        }))
    }

    async fn get_state(
        &self,
        _request: Request<GetStateRequest>,
    ) -> Result<Response<GetStateResponse>, Status> {
        // TODO: Implement
        Ok(Response::new(GetStateResponse {
            success: false,
            message: "Not implemented".to_string(),
            state: None,
        }))
    }
}

// Helper functions to convert between internal types and protobuf types
fn _delta_to_proto(delta: &DeltaObject) -> state_manager::DeltaObject {
    state_manager::DeltaObject {
        account_id: delta.account_id.clone(),
        nonce: delta.nonce,
        prev_commitment: delta.prev_commitment.clone(),
        delta_hash: delta.delta_hash.clone(),
        delta_payload: delta.delta_payload.to_string(),
        ack_sig: delta.ack_sig.clone(),
        publisher_pubkey: delta.publisher_pubkey.clone(),
        publisher_sig: delta.publisher_sig.clone(),
        candidate_at: delta.candidate_at.clone(),
        canonical_at: delta.canonical_at.clone(),
        discarded_at: delta.discarded_at.clone(),
    }
}

fn _state_to_proto(state: &crate::storage::AccountState) -> state_manager::AccountState {
    state_manager::AccountState {
        account_id: state.account_id.clone(),
        state_json: state.state_json.to_string(),
        commitment: state.commitment.clone(),
        created_at: state.created_at.clone(),
        updated_at: state.updated_at.clone(),
    }
}
