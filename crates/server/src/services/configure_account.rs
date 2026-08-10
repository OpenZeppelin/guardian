use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials};
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone)]
pub struct ConfigureAccountParams {
    pub account_id: String,
    pub auth: Auth,
    pub network_config: NetworkConfig,
    pub initial_state: serde_json::Value,
    pub credential: Credentials,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountResult {
    pub account_id: String,
    pub ack_pubkey: String,
    pub ack_commitment: String,
}

/// Requires each declared cosigner commitment to be canonical — `0x` plus
/// 64 lowercase hex digits, the only form the server itself emits — and the
/// list to be non-empty and duplicate-free. The list is later compared by
/// exact string equality against the signer set extracted from
/// `initial_state`, so a non-canonical spelling could only ever fail that
/// comparison; rejecting it here names the offending entry instead of
/// surfacing a confusing mismatch. An empty list can never match a real
/// signer map and would leave the account unable to authorize any request.
fn validate_commitment_list(commitments: &[String]) -> Result<()> {
    if commitments.is_empty() {
        return Err(GuardianError::InvalidInput(
            "cosigner_commitments must not be empty".to_string(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for entry in commitments {
        let hex_digits = entry.strip_prefix("0x").unwrap_or("");
        if hex_digits.len() != 64
            || !hex_digits
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(GuardianError::InvalidInput(format!(
                "cosigner_commitments entry '{entry}' is not a canonical commitment \
                 (expected 0x followed by 64 lowercase hex digits)"
            )));
        }
        if !seen.insert(entry) {
            return Err(GuardianError::InvalidInput(format!(
                "cosigner_commitments contains duplicate entry '{entry}'"
            )));
        }
    }
    Ok(())
}

/// Configure a new account
#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> Result<ConfigureAccountResult> {
    tracing::debug!("Configuring account");

    let network_config = params
        .network_config
        .validate_for_account(&params.account_id)
        .map_err(GuardianError::InvalidNetworkConfig)?;

    if network_config.is_evm() || matches!(params.auth, Auth::EvmEcdsa { .. }) {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "configure".to_string(),
        });
    }

    validate_commitment_list(params.auth.cosigner_commitments())?;

    let existing = state.metadata.get(&params.account_id).await.map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to check existing account in configure_account"
        );
        GuardianError::StorageError(format!("Failed to check existing account: {e}"))
    })?;
    let scheme = params.auth.scheme();

    let commitment = {
        let client = &state.network_client;
        let expected_guardian_commitment = state.ack.commitment(&scheme);

        // Validates that the credential is valid for the account state.
        client
            .validate_credential(&params.initial_state, &params.credential, &params.auth)
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to validate credential"
                );
                GuardianError::NetworkError(format!("Failed to validate credential: {e}"))
            })?;

        client
            .validate_guardian_commitment(&params.initial_state, &expected_guardian_commitment)
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    expected_guardian_commitment = %expected_guardian_commitment,
                    error = %e,
                    "Unauthorized account configuration: invalid GUARDIAN public key binding"
                );
                GuardianError::AuthorizationFailed(format!(
                    "Unauthorized account configuration: {e}"
                ))
            })?;

        // The stored cosigner list is the authorization source of truth for
        // every later request (`Auth::verify`), so the full client-declared
        // set must match the signer set actually stored in the submitted
        // account state — `validate_credential` above only proves the
        // *requesting* key is a signer, not the rest of the list (#102).
        // `None` means the state carries no extractable signer set, matching
        // the canonicalization-side semantics of `should_update_auth`.
        let extracted_auth = client
            .should_update_auth(&params.initial_state, &params.auth)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to extract signer commitments from initial state"
                );
                GuardianError::NetworkError(format!(
                    "Failed to extract signer commitments from initial state: {e}"
                ))
            })?;
        if let Some(expected_auth) = extracted_auth
            && expected_auth != params.auth
        {
            tracing::error!(
                account_id = %params.account_id,
                expected = ?expected_auth,
                provided = ?params.auth,
                "Cosigner commitments do not match account state in configure_account"
            );
            return Err(GuardianError::InvalidInput(format!(
                "cosigner_commitments do not match the signer set in initial_state: expected {:?}, provided {:?}",
                expected_auth.cosigner_commitments(),
                params.auth.cosigner_commitments()
            )));
        }

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
                GuardianError::AuthenticationFailed(format!("Signature verification failed: {e}"))
            })?;

        // calculates the commitment of the account state.
        client
            .get_state_commitment(&params.account_id, &params.initial_state)
            .map_err(GuardianError::NetworkError)?
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
        auth_scheme: scheme.to_string(),
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
            GuardianError::StorageError(format!("Failed to submit initial state: {e}"))
        })?;

    // configure_account is an admin/setup path and intentionally NOT
    // gated by the pause chokepoint. Pause must not block account
    // reconfiguration — do not add the chokepoint here. Pause state
    // is carried forward from `existing` so a new storage backend
    // cannot accidentally clear it (the field is only mutated by
    // `set_pause`/`clear_pause`).
    let was_released = existing.as_ref().and_then(|m| m.released_at).is_some();
    let metadata_entry = AccountMetadata {
        account_id: params.account_id.clone(),
        auth: params.auth,
        network_config,
        created_at,
        updated_at: now,
        has_pending_candidate: existing
            .as_ref()
            .map(|m| m.has_pending_candidate)
            .unwrap_or(false),
        last_auth_timestamp: existing.as_ref().and_then(|m| m.last_auth_timestamp),
        paused_at: existing.as_ref().and_then(|m| m.paused_at),
        paused_reason: existing.as_ref().and_then(|m| m.paused_reason.clone()),
        // `set` never touches released state; the explicit
        // `clear_released` below performs the reactivation.
        released_at: existing.as_ref().and_then(|m| m.released_at),
    };

    state.metadata.set(metadata_entry).await.map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to store metadata"
        );
        GuardianError::StorageError(format!("Failed to store metadata: {e}"))
    })?;

    // Deliberate asymmetry with pause: `released` means "a guardian
    // switch moved this account away from this server", and this very
    // path just re-validated (validate_guardian_commitment above) that
    // the submitted state binds the account to this server again — so
    // re-onboarding is exactly the reactivation event. Pause, an
    // operator decision, stays in force across reconfiguration.
    if was_released {
        tracing::info!(
            account_id = %params.account_id,
            "Reactivating released account via /configure re-onboarding"
        );
        state
            .metadata
            .clear_released(&params.account_id)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to clear released state during re-onboarding"
                );
                GuardianError::StorageError(format!("Failed to clear released state: {e}"))
            })?;
    }

    // Count only first-time creations — /configure also serves
    // reconfiguration of existing accounts.
    if existing.is_none() {
        metrics::counter!(
            crate::metrics::names::ACCOUNTS_CREATED_TOTAL,
            crate::metrics::names::LABEL_KIND =>
                crate::metrics::labels::AccountKind::Miden.as_str()
        )
        .increment(1);
    }

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

    async fn create_test_app_state(
        network_client: MockNetworkClient,
        storage_backend: MockStorageBackend,
        metadata_store: MockMetadataStore,
    ) -> AppState {
        let storage = Arc::new(storage_backend) as Arc<dyn StorageBackend>;

        let keystore_dir =
            std::env::temp_dir().join(format!("test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");

        let ack = AckRegistry::new(keystore_dir)
            .await
            .expect("Failed to create ack registry");

        AppState {
            storage,
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(network_client),
            ack,
            canonicalization: None, // Optimistic mode for tests
            clock: Arc::new(crate::clock::test::MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    #[tokio::test]
    async fn test_configure_account_success() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            })))
            .with_get_state_commitment(Ok("0x1234".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        // Use a valid account JSON fixture
        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
        let ack_pubkey = result.ack_pubkey;
        let ack_commitment = result.ack_commitment;
        assert!(!ack_pubkey.is_empty(), "ack_pubkey should not be empty");
        assert!(
            ack_pubkey.starts_with("0x"),
            "ack_pubkey should be hex format"
        );
        assert!(
            ack_commitment.starts_with("0x"),
            "ack_commitment should be hex format"
        );
    }

    #[tokio::test]
    async fn test_configure_account_success_for_ecdsa() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::auth_request_payload::AuthRequestPayload;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let signer = TestEcdsaSigner::new();

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x1234".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let auth = Auth::MidenEcdsa {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        };
        let request_body = serde_json::json!({
            "account_id": account_id_hex,
            "auth": auth.clone(),
            "network_config": crate::metadata::NetworkConfig::miden_default(),
            "initial_state": initial_state.clone(),
        });
        let request_payload = AuthRequestPayload::from_json_serializable(&request_body).unwrap();
        let (signature_hex, timestamp) = signer.sign_request(account_id_hex, &request_payload);

        let credential =
            Credentials::signature(signer.pubkey_hex.clone(), signature_hex, timestamp)
                .with_request_payload(request_payload);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
        let ack_pubkey = result.ack_pubkey;
        let ack_commitment = result.ack_commitment;
        assert!(ack_pubkey.starts_with("0x"));
        assert!(ack_commitment.starts_with("0x"));
        assert_eq!(ack_commitment.len(), 66);
        assert!(ack_pubkey.len() > 66);
    }

    #[tokio::test]
    async fn test_configure_account_already_exists_reconfigures() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: Some(1000),
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x5678".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok(), "Reconfiguration should succeed");
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
    }

    /// Regression: reconfiguring a paused account must NOT clear
    /// `paused_at` / `paused_reason`. Pause state can only be
    /// transitioned by `set_pause`/`clear_pause` (FR-019).
    #[tokio::test]
    async fn test_configure_account_preserves_existing_pause_state() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        use chrono::TimeZone;
        let paused_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
            .unwrap();
        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: Some(1000),
            paused_at: Some(paused_at),
            paused_reason: Some("compliance".to_string()),
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x5678".to_string()));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state =
            create_test_app_state(network_client, storage_backend, metadata_store.clone()).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        configure_account(&state, params)
            .await
            .expect("Reconfiguration should succeed");

        let set_calls = metadata_store.get_set_calls();
        assert_eq!(set_calls.len(), 1);
        assert_eq!(set_calls[0].paused_at, Some(paused_at));
        assert_eq!(set_calls[0].paused_reason.as_deref(), Some("compliance"));
        assert!(
            metadata_store
                .clear_released_calls
                .lock()
                .unwrap()
                .is_empty(),
            "no release to clear when the account was not released"
        );
    }

    #[tokio::test]
    async fn test_configure_account_reonboarding_clears_released_state() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        use chrono::TimeZone;
        let released_at = chrono::Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: Some(1000),
            paused_at: None,
            paused_reason: None,
            released_at: Some(released_at),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x5678".to_string()));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state =
            create_test_app_state(network_client, storage_backend, metadata_store.clone()).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        configure_account(&state, params)
            .await
            .expect("Re-onboarding a released account should succeed");

        // Re-onboarding (with the guardian binding re-validated) is the
        // reactivation event: released state must be explicitly cleared.
        assert_eq!(
            metadata_store.clear_released_calls.lock().unwrap().clone(),
            vec![account_id_hex.to_string()],
            "re-onboarding must clear the released state"
        );
    }

    #[tokio::test]
    async fn test_configure_account_reonboarding_fails_closed_when_clear_released_fails() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        use chrono::TimeZone;
        let released_at = chrono::Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: Some(1000),
            paused_at: None,
            paused_reason: None,
            released_at: Some(released_at),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Ok("0x5678".to_string()));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()))
            .with_clear_released(Err("disk on fire".to_string()));

        let state =
            create_test_app_state(network_client, storage_backend, metadata_store.clone()).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        // Fail-closed: the account stays released (mutations still refused)
        // and the caller sees the storage failure so the wallet retries
        // the re-onboarding.
        let err = configure_account(&state, params)
            .await
            .expect_err("clear_released failure must surface");
        assert!(
            matches!(err, GuardianError::StorageError(_)),
            "expected StorageError, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_configure_account_network_error() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_commitment(Err("Network connection failed".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::NetworkError(_) => {}
            e => panic!("Expected NetworkError, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_configure_account_unauthorized_guardian_commitment() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_validate_guardian_commitment(Err(
                "OpenZeppelin slot 'openzeppelin::guardian::public_key' mismatch".to_string(),
            ));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthorizationFailed(msg) => {
                assert!(msg.contains("Unauthorized account configuration"));
                assert!(msg.contains("openzeppelin::guardian::public_key"));
            }
            e => panic!("Expected AuthorizationFailed, got: {:?}", e),
        }

        assert!(
            storage_backend.get_submit_state_calls().is_empty(),
            "state should not be persisted on unauthorized configuration"
        );
    }

    #[tokio::test]
    async fn test_configure_account_rejects_mismatched_cosigner_commitments() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, _commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!("0x{}", "aa".repeat(32))],
            })));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!("0x{}", "bb".repeat(32))],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        match result.unwrap_err() {
            GuardianError::InvalidInput(msg) => {
                assert!(msg.contains("cosigner_commitments"), "got: {msg}");
            }
            e => panic!("Expected InvalidInput, got: {:?}", e),
        }

        assert!(
            storage_backend.get_submit_state_calls().is_empty(),
            "state should not be persisted on mismatched cosigner commitments"
        );
    }

    /// The declared list must match the state's signer set exactly:
    /// appending an extra (non-signer) commitment to an otherwise
    /// correct list is rejected — the injected key would otherwise be
    /// authorized for every later request against this account.
    #[tokio::test]
    async fn test_configure_account_rejects_injected_extra_cosigner() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            })));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex, "0xinjected_commitment".to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(
            matches!(result, Err(GuardianError::InvalidInput(_))),
            "expected InvalidInput, got: {result:?}"
        );
        assert!(
            storage_backend.get_submit_state_calls().is_empty(),
            "state should not be persisted when an extra cosigner is injected"
        );
    }

    #[tokio::test]
    async fn test_configure_account_cosigner_extraction_error() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Err("failed to deserialize account".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(
            matches!(result, Err(GuardianError::NetworkError(_))),
            "expected NetworkError, got: {result:?}"
        );
    }

    /// The comparison is deliberately order-sensitive: the signer map's
    /// index order is the canonical order, and the SDK emits it verbatim.
    #[tokio::test]
    async fn test_rejects_reordered_commitments() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let other_hex = format!("0x{}", "aa".repeat(32));

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone(), other_hex.clone()],
            })));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![other_hex, commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(
            matches!(result, Err(GuardianError::InvalidInput(_))),
            "same set in a different order must be rejected, got: {result:?}"
        );
        assert!(storage_backend.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_rejects_non_canonical_empty_and_duplicate_commitment_lists() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let canonical = format!("0x{}", "ab".repeat(32));

        let cases: Vec<(Vec<String>, &str)> = vec![
            (vec![], "empty list"),
            (
                vec![canonical.clone(), canonical.clone()],
                "duplicate entry",
            ),
            (vec![format!("0x{}", "AB".repeat(32))], "uppercase hex"),
            (vec!["ab".repeat(32)], "missing 0x prefix"),
            (vec![format!("0x{}", "ab".repeat(31))], "wrong length"),
            (vec!["0xnot_hex".to_string()], "non-hex characters"),
        ];

        for (list, label) in cases {
            let (pubkey_hex, _, signature_hex, timestamp) =
                generate_falcon_signature(account_id_hex);
            let state = create_test_app_state(
                MockNetworkClient::new(),
                MockStorageBackend::new(),
                MockMetadataStore::new(),
            )
            .await;

            let params = ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: list,
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state: serde_json::json!({"balance": 100}),
                credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
            };

            let result = configure_account(&state, params).await;
            assert!(
                matches!(result, Err(GuardianError::InvalidInput(_))),
                "{label}: expected InvalidInput, got: {result:?}"
            );
        }
    }

    /// `should_update_auth` returning `None` means the state carries no
    /// extractable signer set (e.g. non-multisig layouts); the declared
    /// list is then accepted as-is, matching canonicalization semantics.
    #[tokio::test]
    async fn test_configure_account_skips_validation_without_extractable_signers() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(None))
            .with_get_state_commitment(Ok("0x1234".to_string()));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok(), "got: {result:?}");
    }
}
