use crate::auth::Credentials;
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::DeltaObject;

#[derive(Debug, Clone)]
pub struct GetDeltaSinceParams {
    pub account_id: String,
    pub from_nonce: u64,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaSinceResult {
    pub merged_delta: DeltaObject,
}

pub async fn get_delta_since(
    state: &AppState,
    params: GetDeltaSinceParams,
) -> Result<GetDeltaSinceResult> {
    let account_metadata = state
        .metadata
        .get(&params.account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to check account: {e}")))?
        .ok_or_else(|| PsmError::AccountNotFound(params.account_id.clone()))?;

    account_metadata
        .auth
        .verify(&params.account_id, &params.credentials)
        .map_err(PsmError::AuthenticationFailed)?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    let all_deltas = storage_backend
        .pull_deltas_after(&params.account_id, params.from_nonce)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to fetch deltas: {e}")))?;

    let deltas: Vec<DeltaObject> = all_deltas
        .into_iter()
        .filter(|delta| !delta.status.is_discarded())
        .collect();

    if deltas.is_empty() {
        return Err(PsmError::DeltaNotFound {
            account_id: params.account_id.clone(),
            nonce: params.from_nonce,
        });
    }

    let delta_payloads: Vec<serde_json::Value> =
        deltas.iter().map(|d| d.delta_payload.clone()).collect();

    let merged_payload = {
        let client = state.network_client.lock().await;
        client
            .merge_deltas(delta_payloads)
            .map_err(PsmError::InvalidDelta)?
    };

    let last_delta = deltas.last().unwrap();

    let merged_delta = DeltaObject {
        account_id: params.account_id,
        nonce: last_delta.nonce,
        prev_commitment: deltas.first().unwrap().prev_commitment.clone(),
        new_commitment: last_delta.new_commitment.clone(),
        delta_payload: merged_payload,
        ack_sig: last_delta.ack_sig.clone(),
        status: last_delta.status.clone(),
    };

    Ok(GetDeltaSinceResult { merged_delta })
}
