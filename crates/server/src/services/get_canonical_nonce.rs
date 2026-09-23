//! Canonical nonce pre-check (issue #191): the nonce and commitment of the
//! state GUARDIAN currently holds as canonical, without the state blob.
//! A client whose local account is at or above this nonce has nothing to
//! pull and can skip the full `get_state` fetch.

use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::state::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct GetCanonicalNonceParams {
    pub account_id: String,
    pub credentials: Credentials,
}

/// Head of the canonical state: the nonce the account carries in that
/// state and the state's commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CanonicalNonceResponse {
    pub account_id: String,
    /// Account nonce of the latest canonical state.
    pub nonce: u64,
    /// Commitment of the latest canonical state.
    pub commitment: String,
}

#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn get_canonical_nonce(
    state: &AppState,
    params: GetCanonicalNonceParams,
) -> Result<CanonicalNonceResponse> {
    tracing::debug!("Getting canonical nonce");

    let resolved = resolve_account(state, &params.account_id, &params.credentials).await?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "get_canonical_nonce".to_string(),
        });
    }

    let account_state = resolved
        .storage
        .pull_state(&params.account_id)
        .await
        .map_err(|_e| GuardianError::StateNotFound(params.account_id.clone()))?;

    let nonce = state
        .network_client
        .extract_nonce(&account_state.state_json)
        .map_err(|e| {
            tracing::error!(
                account_id = %params.account_id,
                error = %e,
                "Canonical state blob does not decode to an account"
            );
            GuardianError::AccountDataUnavailable(params.account_id.clone())
        })?;

    Ok(CanonicalNonceResponse {
        account_id: account_state.account_id,
        nonce,
        commitment: account_state.commitment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::state_object::StateObject;
    use crate::testing::helpers::{TestSigner, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use axum::http::StatusCode;
    use guardian_shared::auth_request_payload::AuthRequestPayload;
    use std::sync::Arc;

    const ACCOUNT_ID: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

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
            network_config: NetworkConfig::miden_default(),
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

    /// Signed the way the HTTP layer signs `StateQuery`: the credentials
    /// carry the canonical query bytes the signature covers.
    fn params(signer: &TestSigner) -> GetCanonicalNonceParams {
        let payload = serde_json::json!({ "account_id": ACCOUNT_ID });
        let (signature, timestamp) = signer.sign_json_payload(ACCOUNT_ID, &payload);
        let bytes = serde_json::to_vec(&payload).expect("payload serializes");
        let credentials = Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
            .with_request_payload(AuthRequestPayload::from_bytes(&bytes))
            .with_request_payload_bytes(bytes);
        GetCanonicalNonceParams {
            account_id: ACCOUNT_ID.to_string(),
            credentials,
        }
    }

    #[tokio::test]
    async fn returns_nonce_and_commitment_of_the_canonical_state() {
        let (state, storage, network, metadata) = create_test_state();
        let signer = TestSigner::new();
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            ACCOUNT_ID.to_string(),
            vec![signer.commitment_hex.clone()],
        ))));
        let _storage = storage.with_pull_state(Ok(create_state_object(
            ACCOUNT_ID.to_string(),
            "0xabc".to_string(),
            serde_json::json!({ "data": "opaque" }),
        )));
        let _network = network.with_extract_nonce(Ok(7));

        let response = get_canonical_nonce(&state, params(&signer))
            .await
            .expect("canonical nonce should resolve");

        assert_eq!(
            response,
            CanonicalNonceResponse {
                account_id: ACCOUNT_ID.to_string(),
                nonce: 7,
                commitment: "0xabc".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn missing_state_is_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let signer = TestSigner::new();
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            ACCOUNT_ID.to_string(),
            vec![signer.commitment_hex.clone()],
        ))));
        let _storage = storage.with_pull_state(Err("missing".to_string()));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("missing state should fail");

        assert!(matches!(err, GuardianError::StateNotFound(_)));
        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn undecodable_state_is_account_data_unavailable() {
        let (state, storage, network, metadata) = create_test_state();
        let signer = TestSigner::new();
        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            ACCOUNT_ID.to_string(),
            vec![signer.commitment_hex.clone()],
        ))));
        let _storage = storage.with_pull_state(Ok(create_state_object(
            ACCOUNT_ID.to_string(),
            "0xabc".to_string(),
            serde_json::json!({ "balance": 0 }),
        )));
        let _network = network.with_extract_nonce(Err("no data field".to_string()));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("undecodable state should fail");

        assert!(matches!(err, GuardianError::AccountDataUnavailable(_)));
        assert_eq!(err.http_status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn unknown_account_is_rejected_before_storage() {
        let (state, _storage, _network, metadata) = create_test_state();
        let signer = TestSigner::new();
        let _metadata = metadata.with_get(Ok(None));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("unknown account should fail");

        assert!(matches!(err, GuardianError::AccountNotFound(_)));
    }
}
