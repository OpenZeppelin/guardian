use crate::delta_object::DeltaObject;
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::state::AppState;

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

#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.account_id, from_nonce = params.from_nonce)
)]
pub async fn get_delta_since(
    state: &AppState,
    params: GetDeltaSinceParams,
) -> Result<GetDeltaSinceResult> {
    tracing::info!(
        account_id = %params.account_id,
        from_nonce = params.from_nonce,
        "Getting delta since"
    );

    let resolved = resolve_account(state, &params.account_id, &params.credentials).await?;

    let all_deltas = resolved
        .storage
        .pull_deltas_after(&params.account_id, params.from_nonce)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to fetch deltas: {e}")))?;

    // Only include canonical deltas to avoid surfacing candidates that may be discarded later
    let deltas: Vec<DeltaObject> = all_deltas
        .into_iter()
        .filter(|delta| delta.status.is_canonical())
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

    // Merged result is a materialized snapshot; mark as canonical by default
    let merged_delta = DeltaObject {
        account_id: params.account_id,
        nonce: last_delta.nonce,
        prev_commitment: deltas.first().unwrap().prev_commitment.clone(),
        new_commitment: last_delta.new_commitment.clone(),
        delta_payload: merged_payload,
        ack_sig: last_delta.ack_sig.clone(),
        ack_pubkey: last_delta.ack_pubkey.clone(),
        ack_scheme: last_delta.ack_scheme.clone(),
        status: last_delta.status.clone(),
    };

    Ok(GetDeltaSinceResult { merged_delta })
}
