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
   
    if existing.is_some() {
        return Err(PsmError::AccountAlreadyExists(params.account_id.clone()));
    }

    let commitment = {
        let mut client = state.network_client.lock().await;

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

    // Extract expected auth from initial_state to validate against provided params
    let expected_auth = client
    .should_update_auth(&params.initial_state)
    .await
    .map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to extract auth from initial_state"
        );
        PsmError::AuthenticationFailed(e)
    })?;


    // Compare with client-provided auth
    if let Some(expected_auth) = expected_auth {
        if expected_auth != params.auth {

            return Err(PsmError::AuthenticationFailed(
                "auth.cosigner_commitments do not match account initial state".to_string(),
            ));
        }
    }


    // calculates the commitment of the account state.
    client
        .get_state_commitment(&params.account_id, &params.initial_state)
        .map_err(PsmError::NetworkError)?
    };

    let now = state.clock.now_rfc3339();
    let account_state = StateObject {
        account_id: params.account_id.clone(),
        state_json: params.initial_state,
        commitment,
        created_at: now.clone(),
        updated_at: now,
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

    // Create and store metadata
    let metadata_entry = AccountMetadata {
        account_id: params.account_id.clone(),
        auth: params.auth,
        created_at: account_state.created_at.clone(),
        updated_at: account_state.updated_at.clone(),
        has_pending_candidate: false,
        last_auth_timestamp: None,
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
        ack_pubkey: state.ack.pubkey(),
    })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::{Acknowledger, MidenFalconRpoSigner};
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

        let signer = MidenFalconRpoSigner::new(keystore_dir).expect("Failed to create signer");
        let ack = Acknowledger::FilesystemMidenFalconRpo(signer);

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
    async fn test_configure_rejects_auth_mismatch() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x2133663d171cfa1023a7ad0ea95163";

        let fixture_commitment = "0x4801592850367928503679285036792850367928503679285036792850367928"; 

     
        let network_client = MockNetworkClient::new()
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![fixture_commitment.to_string()],
            })))
            // Bypass the network credential check so we can test the auth mismatch logic
            .with_validate_credential(Ok(())) 
            .with_get_state_commitment(Ok("0x1234".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));
        let state = create_test_app_state(network_client, storage_backend, metadata_store);

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

      
        let (malicious_pubkey, malicious_commitment, malicious_sig, timestamp) = 
            generate_falcon_signature(account_id_hex);

        let credential = Credentials::signature(malicious_pubkey, malicious_sig, timestamp);

       
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![malicious_commitment], // Valid crypto, but wrong for this account
            },
            initial_state,
            credential,
        };

        // 5. Run Service
        let result = configure_account(&state, params).await;

        assert!(result.is_err(), "Service should reject mismatched auth commitment");
        
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("match") || err_msg.contains("cosigner"),
            "Expected mismatch error, but got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_configure_account_already_exists() {
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
            last_auth_timestamp: None,
        };

        let network_client = MockNetworkClient::new();
        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(Some(existing_metadata)));

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
            PsmError::AccountAlreadyExists(_) => {}
            e => panic!("Expected AccountAlreadyExists error, got: {:?}", e),
        }
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
