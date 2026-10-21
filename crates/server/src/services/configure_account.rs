use crate::error::{GuardianError, Result};
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials};
use crate::metadata::{AccountMetadata, ClearTransition};
use crate::services::{consume_auth_timestamp, validate_request_timestamp};
use crate::state::AppState;
use crate::state_object::StateObject;
use guardian_shared::SignatureScheme;

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

    if params.credential.auth_format() == crate::metadata::auth::RequestAuthFormat::Session {
        validate_request_timestamp(state, &params.account_id, &params.credential)?;
        let message = super::auth_request_word(&params.account_id, &params.credential)?;
        return Err(super::reject_session_credentials(state, &params.credential, message).await);
    }

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

    params
        .auth
        .validate_canonical_signer_set()
        .map_err(GuardianError::InvalidInput)?;

    let request_timestamp =
        validate_request_timestamp(state, &params.account_id, &params.credential)?;

    let existing = state.metadata.get(&params.account_id).await.map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to check existing account in configure_account"
        );
        GuardianError::StorageError(format!("Failed to check existing account: {e}"))
    })?;
    // Metadata without a stored state is a first configuration that claimed
    // the account below and then failed before writing its state (a storage
    // error after the claim). Its retry finishes that first configuration:
    // it is held to the scheme policy for new accounts and counted as a
    // creation. A failed state read keeps the reconfiguration treatment, so
    // it can never block an account that does have state.
    let unfinished_claim = match &existing {
        Some(_) => match state
            .storage
            .pull_state_commitment(&params.account_id)
            .await
        {
            Ok(_) => false,
            Err(e) if crate::storage::is_storage_not_found(&e) => true,
            Err(e) => {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "Could not check for a stored state; treating the account as configured"
                );
                false
            }
        },
        None => false,
    };
    let scheme = params.auth.scheme();
    if existing.is_none() || unfinished_claim {
        reject_disallowed_scheme(state, scheme)?;
    }

    let (head, signer_commitment) = {
        let client = &state.network_client;
        let expected_guardian_commitment = state.ack.commitment(&scheme);

        // Validates that the credential is valid for the account state.
        client
            .validate_credential(&params.initial_state, &params.credential, &params.auth)
            .map_err(|e| {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to validate credential"
                );
                GuardianError::NetworkError(format!("Failed to validate credential: {e}"))
            })?;

        client
            .validate_guardian_commitment(&params.initial_state, &expected_guardian_commitment)
            .map_err(|e| {
                tracing::warn!(
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
            tracing::warn!(
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
        let signer_commitment = params
            .auth
            .verify(&params.account_id, &params.credential)
            .map_err(|e| {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "Signature verification failed in configure_account"
                );
                GuardianError::AuthenticationFailed(format!("Signature verification failed: {e}"))
            })?;

        // Commitment and nonce of the account state, from one decode; both
        // are stored with the state.
        let head = client
            .get_state_head(&params.account_id, &params.initial_state)
            .map_err(GuardianError::NetworkError)?;
        (head, signer_commitment)
    };

    let now = state.clock.now_rfc3339();

    // A first configuration claims the account before anything else: of two
    // concurrent first configurations (a client retry, or a replay of the
    // signed request) exactly one creates the metadata row, and the other
    // is judged below as a reconfiguration of the account it lost to. Its
    // replay timestamp is then consumed before it writes anything, so a
    // duplicate is refused instead of storing the initial state over
    // whatever the account has moved to since.
    let created = existing.is_none()
        && state
            .metadata
            .create(AccountMetadata {
                account_id: params.account_id.clone(),
                auth: params.auth.clone(),
                network_config: network_config.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            })
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to create account metadata"
                );
                GuardianError::StorageError(format!("Failed to create account metadata: {e}"))
            })?;
    let existing = if existing.is_none() && !created {
        let claimed = state.metadata.get(&params.account_id).await.map_err(|e| {
            GuardianError::StorageError(format!("Failed to re-read account metadata: {e}"))
        })?;
        Some(claimed.ok_or_else(|| {
            GuardianError::StorageError(
                "Account metadata vanished after a concurrent first configuration".to_string(),
            )
        })?)
    } else {
        existing
    };

    // The replay-state row has an FK to account metadata, which the claim
    // above guarantees, so every configuration consumes its timestamp
    // before it writes any state.
    consume_auth_timestamp(
        state,
        &params.account_id,
        &signer_commitment,
        request_timestamp,
    )
    .await?;

    let created_at = existing
        .as_ref()
        .map(|m| m.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let account_state = StateObject {
        account_id: params.account_id.clone(),
        state_json: params.initial_state,
        commitment: head.commitment,
        nonce: head.nonce,
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
    if let Some(existing) = &existing {
        let metadata_entry = AccountMetadata {
            account_id: params.account_id.clone(),
            auth: params.auth,
            network_config,
            created_at,
            updated_at: now,
            has_pending_candidate: existing.has_pending_candidate,
            paused_at: existing.paused_at,
            paused_reason: existing.paused_reason.clone(),
            // `set` never touches released state; the explicit
            // `clear_released_if_state` below performs the reactivation.
            released_at: existing.released_at,
        };

        state.metadata.set(metadata_entry).await.map_err(|e| {
            tracing::error!(
                account_id = %params.account_id,
                error = %e,
                "Failed to store metadata"
            );
            GuardianError::StorageError(format!("Failed to store metadata: {e}"))
        })?;
    }

    // Deliberate asymmetry with pause: `released` means "a guardian
    // switch moved this account away from this server", and this very
    // path just re-validated (validate_guardian_commitment above) that
    // the submitted state binds the account to this server again — so
    // re-onboarding is exactly the reactivation event. Pause, an
    // operator decision, stays in force across reconfiguration.
    //
    // The clear runs for every existing account, after `submit_state`,
    // not only when `existing` showed a release: a release written
    // between that read and the state write (the release sweep proving
    // a switch against the previous state) would otherwise survive the
    // re-onboarding. Both the release and this clear are conditional on
    // the stored state, atomically, so whichever state is stored last
    // decides: a release proved against an older state is refused or
    // undone here, and a switch delta that replaced this state after
    // `submit_state` keeps its release.
    if !created {
        let cleared = state
            .metadata
            .clear_released_if_state(
                &params.account_id,
                &account_state.commitment,
                state.storage.as_ref(),
            )
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to clear released state during re-onboarding"
                );
                GuardianError::StorageError(format!("Failed to clear released state: {e}"))
            })?;
        match cleared {
            ClearTransition::Cleared => tracing::info!(
                account_id = %params.account_id,
                "Reactivated released account via /configure re-onboarding"
            ),
            ClearTransition::NotReleased => {}
            ClearTransition::StateMoved => tracing::warn!(
                account_id = %params.account_id,
                "A delta replaced the re-onboarded state before its release was cleared; \
                 the release of that later state stands"
            ),
        }
    }

    // Count only first-time creations, including the retry that finishes an
    // unfinished claim: /configure also serves reconfiguration of existing
    // accounts.
    let created = created || unfinished_claim;
    if created {
        metrics::counter!(
            crate::metrics::names::ACCOUNTS_CREATED_TOTAL,
            crate::metrics::names::LABEL_KIND =>
                crate::metrics::labels::AccountKind::Miden.as_str()
        )
        .increment(1);
    }

    tracing::info!(reconfiguration = !created, "Account configured");

    Ok(ConfigureAccountResult {
        account_id: params.account_id,
        ack_pubkey: state.ack.pubkey(&scheme),
        ack_commitment: state.ack.commitment(&scheme),
    })
}

fn reject_disallowed_scheme(state: &AppState, scheme: SignatureScheme) -> Result<()> {
    let allowed = state.ack.account_schemes();
    if allowed.allows(scheme) {
        return Ok(());
    }
    tracing::warn!(
        scheme = scheme.as_str(),
        allowed_schemes = %allowed,
        "Rejected account registration: signature scheme not allowed"
    );
    Err(GuardianError::SignatureSchemeNotAllowed {
        scheme: scheme.as_str().to_string(),
        allowed_schemes: allowed.names(),
    })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::state_object::StateHead;
    use crate::storage::StorageBackend;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    /// The head the mock network client reports for the configured state.
    fn head(commitment: &str, nonce: u64) -> StateHead {
        StateHead {
            commitment: commitment.to_string(),
            nonce: Some(nonce),
        }
    }

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
            miden_sessions: std::sync::Arc::new(crate::session::MidenSessions::default()),
            storage,
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(network_client),
            ack,
            canonicalization: None, // Optimistic mode for tests
            release_sweep: None,
            clock: Arc::new(crate::clock::SystemClock),
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
        let verified_signer_commitment = commitment_hex.clone();
        let other_signer_commitment = format!("0x{}", "ab".repeat(32));
        let configured_commitments =
            vec![other_signer_commitment, verified_signer_commitment.clone()];

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: configured_commitments.clone(),
            })))
            .with_get_state_head(Ok(head("0x1234", 3)));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let observed_storage = storage_backend.clone();

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));
        let observed_metadata = metadata_store.clone();

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        // Use a valid account JSON fixture
        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: configured_commitments,
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
        // The state is stored with the head the network client decoded.
        let stored = observed_storage.get_submit_state_calls();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].commitment, "0x1234");
        assert_eq!(stored[0].nonce, Some(3));
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
        assert_eq!(
            observed_metadata.get_update_timestamp_cas_calls(),
            vec![(
                account_id_hex.to_string(),
                verified_signer_commitment,
                timestamp,
            )]
        );
    }

    #[tokio::test]
    async fn configure_rejects_new_account_whose_scheme_is_not_allowed() {
        use crate::config::account_schemes::AllowedAccountSchemes;
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let mut state = create_test_app_state(
            MockNetworkClient::new(),
            MockStorageBackend::new(),
            MockMetadataStore::new().with_get(Ok(None)),
        )
        .await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        match configure_account(&state, params).await {
            Err(GuardianError::SignatureSchemeNotAllowed {
                scheme,
                allowed_schemes,
            }) => {
                assert_eq!(scheme, "falcon");
                assert_eq!(allowed_schemes, vec!["ecdsa".to_string()]);
            }
            other => panic!("expected SignatureSchemeNotAllowed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configure_keeps_serving_existing_accounts_whose_scheme_is_no_longer_allowed() {
        use crate::config::account_schemes::AllowedAccountSchemes;
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
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let mut state =
            create_test_app_state(network_client, storage_backend, metadata_store).await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        let result = configure_account(&state, params).await;
        assert!(
            result.is_ok(),
            "existing Falcon account must reconfigure: {result:?}"
        );
    }

    #[tokio::test]
    async fn retry_of_an_unfinished_first_configuration_is_held_to_the_new_account_scheme_policy() {
        use crate::config::account_schemes::AllowedAccountSchemes;
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let claimed_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        let storage_backend = MockStorageBackend::new()
            .with_pull_state(Err("Failed to pull state: Record not found".to_string()));
        let metadata_store = MockMetadataStore::new().with_get(Ok(Some(claimed_metadata)));

        let mut state =
            create_test_app_state(MockNetworkClient::new(), storage_backend, metadata_store).await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        match configure_account(&state, params).await {
            Err(GuardianError::SignatureSchemeNotAllowed { scheme, .. }) => {
                assert_eq!(scheme, "falcon");
            }
            other => panic!(
                "metadata without a stored state is an unfinished first configuration, \
                 held to the new-account scheme policy: got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn configure_rejects_timestamp_outside_the_skew_window() {
        let state = create_test_app_state(
            MockNetworkClient::new(),
            MockStorageBackend::new(),
            MockMetadataStore::new(),
        )
        .await;
        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!("0x{}", "aa".repeat(32))],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature("0x00".into(), "0x00".into(), 0),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationFailed(_))
        ));
    }

    #[tokio::test]
    async fn test_configure_account_success_for_ecdsa() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::auth_request_payload::AuthRequestPayload;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let signer = TestEcdsaSigner::new();

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x1234", 3)));

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
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));

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

    #[tokio::test]
    async fn configure_rejects_replayed_reconfiguration_before_writes() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let existing = AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(existing)))
            .with_update_timestamp_cas(Ok(false));
        let observed = metadata.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            MockStorageBackend::new(),
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationReplay)
        ));
        assert_eq!(
            observed.get_update_timestamp_cas_calls(),
            vec![(account_id.to_string(), commitment, timestamp)]
        );
        assert!(observed.get_set_calls().is_empty());
    }

    /// A duplicate of a first configuration (a client retry or a replay)
    /// that loses the claim to the configuration it duplicates is judged as
    /// a reconfiguration of the account it lost to: its timestamp, already
    /// consumed by the winner, is refused before it writes anything, so it
    /// cannot store the initial state over the state the account has moved
    /// to since.
    #[tokio::test]
    async fn duplicate_first_configuration_is_refused_before_writes() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let winner = AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            has_pending_candidate: true,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        // Reads pop LIFO: the first read sees no account, the re-read after
        // the lost claim sees the winner's.
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(winner)))
            .with_get(Ok(None))
            .with_create(Ok(false))
            .with_update_timestamp_cas(Ok(false));
        let observed_metadata = metadata.clone();
        let storage = MockStorageBackend::new();
        let observed_storage = storage.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            storage,
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationReplay)
        ));
        assert_eq!(observed_metadata.get_create_calls().len(), 1);
        assert_eq!(
            observed_metadata.get_update_timestamp_cas_calls(),
            vec![(account_id.to_string(), commitment, timestamp)]
        );
        assert!(observed_storage.get_submit_state_calls().is_empty());
        assert!(observed_metadata.get_set_calls().is_empty());
    }

    /// The configuration that wins the claim consumes its timestamp before
    /// it stores the initial state, and creates the account's metadata
    /// instead of writing it with the generic upsert.
    #[tokio::test]
    async fn first_configuration_claims_then_consumes_before_storing_state() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let metadata = MockMetadataStore::new()
            .with_get(Ok(None))
            .with_create(Ok(true))
            .with_update_timestamp_cas(Ok(false));
        let observed_metadata = metadata.clone();
        let storage = MockStorageBackend::new();
        let observed_storage = storage.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            storage,
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationReplay)
        ));
        let created = observed_metadata.get_create_calls();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].account_id, account_id);
        assert!(!created[0].has_pending_candidate);
        assert!(
            observed_storage.get_submit_state_calls().is_empty(),
            "a refused timestamp must stop the first configuration before its state write"
        );
        assert!(observed_metadata.get_set_calls().is_empty());
    }

    fn claimed_metadata(account_id: &str, commitment: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            has_pending_candidate: true,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    /// The retry that finishes an unfinished first configuration is the
    /// account's creation, so it is counted as one.
    #[tokio::test]
    async fn retry_of_an_unfinished_first_configuration_counts_as_a_creation() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            MockStorageBackend::new()
                .with_pull_state(Err("Failed to pull state: Record not found".to_string())),
            MockMetadataStore::new().with_get(Ok(Some(claimed_metadata(account_id, &commitment)))),
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        let recorder = crate::metrics::recorder::build_recorder();
        let handle = recorder.handle();
        let result = {
            let _guard = metrics::set_default_local_recorder(&recorder);
            configure_account(&state, params).await
        };

        assert!(
            result.is_ok(),
            "the retry must finish the account: {result:?}"
        );
        let rendered = handle.render();
        assert!(
            rendered.contains(&format!(
                "{}{{kind=\"miden\"}} 1",
                crate::metrics::names::ACCOUNTS_CREATED_TOTAL
            )),
            "the finishing retry must count one creation:\n{rendered}"
        );
    }

    /// A first configuration that loses the claim to a different signer
    /// passes its own replay check and goes ahead as a reconfiguration of
    /// the account it lost to: it writes metadata over the winner's row,
    /// keeping the winner's creation time and in-flight candidate, rather
    /// than treating the account as new.
    #[tokio::test]
    async fn losing_first_configuration_from_another_signer_reconfigures_the_winners_account() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let winner = AccountMetadata {
            created_at: "2023-06-01T00:00:00Z".into(),
            ..claimed_metadata(account_id, &commitment)
        };
        // Reads pop LIFO: the first read sees no account, the re-read after
        // the lost claim sees the winner's.
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(winner)))
            .with_get(Ok(None))
            .with_create(Ok(false))
            .with_update_timestamp_cas(Ok(true));
        let observed_metadata = metadata.clone();
        let storage = MockStorageBackend::new();
        let observed_storage = storage.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            storage,
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        let result = configure_account(&state, params).await;
        assert!(result.is_ok(), "{result:?}");

        let stored = observed_storage.get_submit_state_calls();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].created_at, "2023-06-01T00:00:00Z");
        let set = observed_metadata.get_set_calls();
        assert_eq!(set.len(), 1, "the loser reconfigures the winner's row");
        assert_eq!(set[0].created_at, "2023-06-01T00:00:00Z");
        assert!(set[0].has_pending_candidate);
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
            paused_at: Some(paused_at),
            paused_reason: Some("compliance".to_string()),
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
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
        assert_eq!(
            metadata_store.clear_released_calls.lock().unwrap().clone(),
            vec![(account_id_hex.to_string(), "0x5678".to_string())],
            "reconfiguration always clears (idempotently) after its state write, \
             conditional on the state it wrote"
        );
    }

    #[tokio::test]
    async fn test_configure_account_clears_a_release_written_after_its_first_read() {
        // The release sweep released the account between this request's
        // first metadata read (which saw it active) and its state write.
        // The unconditional clear after the write must still undo it.
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
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()))
            // The store reports a release it just cleared.
            .with_clear_released(Ok(ClearTransition::Cleared));

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
        assert_eq!(
            metadata_store.clear_released_calls.lock().unwrap().clone(),
            vec![(account_id_hex.to_string(), "0x5678".to_string())]
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
            paused_at: None,
            paused_reason: None,
            released_at: Some(released_at),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
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
            vec![(account_id_hex.to_string(), "0x5678".to_string())],
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
            paused_at: None,
            paused_reason: None,
            released_at: Some(released_at),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
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
            .with_get_state_head(Err("Network connection failed".to_string()));

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
                "Miden Standards slot 'miden::standards::auth::guardian::pub_key' mismatch"
                    .to_string(),
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
                assert!(msg.contains("miden::standards::auth::guardian::pub_key"));
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
            .with_get_state_head(Ok(head("0x1234", 3)));

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
