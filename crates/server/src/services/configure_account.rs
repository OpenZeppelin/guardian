use crate::error::{PsmError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::auth::{Auth, Credentials};
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone)]
pub struct ConfigureAccountParams {
    pub account_id: String,
    pub auth: Auth,
    pub initial_state: serde_json::Value,
    pub credential: Credentials,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountResult {
    pub account_id: String,
    pub ack_pubkey: String,
    pub ack_commitment: String,
}

/// Configure a new account
#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> Result<ConfigureAccountResult> {
    tracing::info!(account_id = %params.account_id, "Configuring account");

    let existing = state.metadata.get(&params.account_id).await.map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to check existing account in configure_account"
        );
        PsmError::StorageError(format!("Failed to check existing account: {e}"))
    })?;

    let commitment = {
        let client = state.network_client.lock().await;

        // Validates that the credential is valid for the account state.
        client
            .validate_credential(&params.initial_state, &params.credential)
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to validate credential"
                );
                PsmError::NetworkError(format!("Failed to validate credential: {e}"))
            })?;

        // Verifies the credential authorization.
        params
            .auth
            .verify(&params.account_id, &params.credential)
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Signature verification failed in configure_account"
                );
                PsmError::AuthenticationFailed(format!("Signature verification failed: {e}"))
            })?;

        // calculates the commitment of the account state.
        client
            .get_state_commitment(&params.account_id, &params.initial_state)
            .map_err(PsmError::NetworkError)?
    };

    let now = state.clock.now_rfc3339();
    let created_at = existing
        .as_ref()
        .map(|m| m.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let account_state = StateObject {
        account_id: params.account_id.clone(),
        state_json: params.initial_state,
        commitment,
        created_at: created_at.clone(),
        updated_at: now.clone(),
        auth_scheme: String::new(),
    };

    state
        .storage
        .submit_state(&account_state)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.account_id,
                error = %e,
                "Failed to submit initial state"
            );
            PsmError::StorageError(format!("Failed to submit initial state: {e}"))
        })?;

    // Create and store metadata (preserving created_at and replay protection on reconfigure)
    let scheme = params.auth.scheme();
    let metadata_entry = AccountMetadata {
        account_id: params.account_id.clone(),
        auth: params.auth,
        created_at,
        updated_at: now,
        has_pending_candidate: existing
            .as_ref()
            .map(|m| m.has_pending_candidate)
            .unwrap_or(false),
        last_auth_timestamp: existing.and_then(|m| m.last_auth_timestamp),
    };

    state.metadata.set(metadata_entry).await.map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to store metadata"
        );
        PsmError::StorageError(format!("Failed to store metadata: {e}"))
    })?;

    Ok(ConfigureAccountResult {
        account_id: params.account_id,
        ack_pubkey: state.ack.pubkey(&scheme),
        ack_commitment: state.ack.commitment(&scheme),
    })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::storage::StorageBackend;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn create_test_app_state(
        network_client: MockNetworkClient,
        storage_backend: MockStorageBackend,
        metadata_store: MockMetadataStore,
    ) -> AppState {
        let storage = Arc::new(storage_backend) as Arc<dyn StorageBackend>;

        let keystore_dir =
            std::env::temp_dir().join(format!("test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");

        let ack = AckRegistry::new(keystore_dir).expect("Failed to create ack registry");

        AppState {
            storage,
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(Mutex::new(network_client)),
            ack,
            canonicalization: None, // Optimistic mode for tests
            clock: Arc::new(crate::clock::test::MockClock::default()),
        }
    }

    #[tokio::test]
    async fn test_configure_account_success() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x069cde0ebf59f29063051ad8a3d32d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x1234".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        // Use a valid account JSON fixture
        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
        assert!(
            !result.ack_pubkey.is_empty(),
            "ack_pubkey should not be empty"
        );
        assert!(
            result.ack_pubkey.starts_with("0x"),
            "ack_pubkey should be hex format"
        );
    }

    #[tokio::test]
    async fn test_configure_account_already_exists_reconfigures() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x069cde0ebf59f29063051ad8a3d32d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: Some(1000),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x5678".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok(), "Reconfiguration should succeed");
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
    }

    #[tokio::test]
    async fn test_configure_account_network_error() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x069cde0ebf59f29063051ad8a3d32d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Err("Network connection failed".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::NetworkError(_) => {}
            e => panic!("Expected NetworkError, got: {:?}", e),
        }
    }
}
