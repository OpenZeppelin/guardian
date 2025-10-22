use crate::auth::Auth;
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::{AccountMetadata, AccountState, StorageType};

#[derive(Debug, Clone)]
pub struct ConfigureAccountParams {
    pub account_id: String,
    pub auth: Auth,
    pub initial_state: serde_json::Value,
    pub storage_type: StorageType,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountResult {
    pub account_id: String,
}

/// Configure a new account
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> Result<ConfigureAccountResult> {
    let existing =
        state.metadata.get(&params.account_id).await.map_err(|e| {
            PsmError::StorageError(format!("Failed to check existing account: {e}"))
        })?;

    if existing.is_some() {
        return Err(PsmError::AccountAlreadyExists(params.account_id.clone()));
    }

    let commitment = {
        let mut client = state.network_client.lock().await;
        client
            .verify_state(&params.account_id, &params.initial_state)
            .await
            .map_err(PsmError::NetworkError)?
    };

    let now = state.clock.now_rfc3339();
    let account_state = AccountState {
        account_id: params.account_id.clone(),
        state_json: params.initial_state,
        commitment,
        created_at: now.clone(),
        updated_at: now,
    };

    let storage_backend = state
        .storage
        .get(&params.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    storage_backend
        .submit_state(&account_state)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to submit initial state: {e}")))?;

    // Create and store metadata
    let metadata_entry = AccountMetadata {
        account_id: params.account_id.clone(),
        auth: params.auth,
        storage_type: params.storage_type,
        created_at: account_state.created_at.clone(),
        updated_at: account_state.updated_at.clone(),
    };

    state
        .metadata
        .set(metadata_entry)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to store metadata: {e}")))?;

    Ok(ConfigureAccountResult {
        account_id: params.account_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonicalization::CanonicalizationMode;
    use crate::storage::{StorageBackend, StorageRegistry};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn create_test_app_state(
        network_client: MockNetworkClient,
        storage_backend: MockStorageBackend,
        metadata_store: MockMetadataStore,
    ) -> AppState {
        let mut backends = HashMap::new();
        backends.insert(
            StorageType::Filesystem,
            Arc::new(storage_backend) as Arc<dyn StorageBackend>,
        );

        AppState {
            storage: StorageRegistry::new(backends),
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(Mutex::new(network_client)),
            canonicalization_mode: CanonicalizationMode::Optimistic,
            clock: Arc::new(crate::clock::test::MockClock::default()),
        }
    }

    #[tokio::test]
    async fn test_configure_account_success() {
        let network_client =
            MockNetworkClient::new().with_verify_state(Ok("commitment_hash_123".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        let params = ConfigureAccountParams {
            account_id: "0x123456789abcdef123456789abcdef".to_string(),
            auth: crate::auth::Auth::MidenFalconRpo {
                cosigner_pubkeys: vec!["pubkey1".to_string()],
            },
            initial_state: serde_json::json!({"balance": 100}),
            storage_type: StorageType::Filesystem,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, "0x123456789abcdef123456789abcdef");
    }

    #[tokio::test]
    async fn test_configure_account_already_exists() {
        let existing_metadata = AccountMetadata {
            account_id: "0x123456789abcdef123456789abcdef".to_string(),
            auth: crate::auth::Auth::MidenFalconRpo {
                cosigner_pubkeys: vec!["pubkey1".to_string()],
            },
            storage_type: StorageType::Filesystem,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };

        let network_client = MockNetworkClient::new();
        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(Some(existing_metadata)));

        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        let params = ConfigureAccountParams {
            account_id: "0x123456789abcdef123456789abcdef".to_string(),
            auth: crate::auth::Auth::MidenFalconRpo {
                cosigner_pubkeys: vec!["pubkey1".to_string()],
            },
            initial_state: serde_json::json!({"balance": 100}),
            storage_type: StorageType::Filesystem,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::AccountAlreadyExists(_) => {}
            e => panic!("Expected AccountAlreadyExists error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_configure_account_network_error() {
        let network_client = MockNetworkClient::new()
            .with_verify_state(Err("Network connection failed".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        let params = ConfigureAccountParams {
            account_id: "0x123456789abcdef123456789abcdef".to_string(),
            auth: crate::auth::Auth::MidenFalconRpo {
                cosigner_pubkeys: vec!["pubkey1".to_string()],
            },
            initial_state: serde_json::json!({"balance": 100}),
            storage_type: StorageType::Filesystem,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::NetworkError(_) => {}
            e => panic!("Expected NetworkError, got: {:?}", e),
        }
    }
}
