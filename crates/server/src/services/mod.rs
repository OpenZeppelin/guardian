use crate::error::{PsmError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::auth::{Credentials, MAX_TIMESTAMP_SKEW_MS};
use crate::state::AppState;
use crate::storage::StorageBackend;
use std::sync::Arc;

mod configure_account;
mod delta_commit;
mod get_delta;
mod get_delta_proposals;
mod get_delta_since;
mod get_state;
mod payload_normalize;
mod push_delta;
mod push_delta_proposal;
mod sign_delta_proposal;

pub use crate::jobs::canonicalization::{
    process_canonicalizations_now, start_canonicalization_worker,
};
pub use configure_account::{ConfigureAccountParams, ConfigureAccountResult, configure_account};
pub use get_delta::{GetDeltaParams, GetDeltaResult, get_delta};
pub use get_delta_proposals::{
    GetDeltaProposalsParams, GetDeltaProposalsResult, get_delta_proposals,
};
pub use get_delta_since::{GetDeltaSinceParams, GetDeltaSinceResult, get_delta_since};
pub use get_state::{GetStateParams, GetStateResult, get_state};
pub use payload_normalize::normalize_payload;
pub use push_delta::{PushDeltaParams, PushDeltaResult, push_delta};
pub use push_delta_proposal::{
    PushDeltaProposalParams, PushDeltaProposalResult, push_delta_proposal,
};
pub use sign_delta_proposal::{
    SignDeltaProposalParams, SignDeltaProposalResult, sign_delta_proposal,
};

#[derive(Clone)]
pub struct ResolvedAccount {
    pub metadata: AccountMetadata,
    pub storage: Arc<dyn StorageBackend>,
}

impl std::fmt::Debug for ResolvedAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAccount")
            .field("metadata", &self.metadata)
            .field("storage", &"<StorageBackend>")
            .finish()
    }
}

#[tracing::instrument(skip(state, creds), fields(account_id = %account_id))]
pub async fn resolve_account(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
) -> Result<ResolvedAccount> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to check account in resolve_account"
            );
            PsmError::StorageError(format!("Failed to check account: {e}"))
        })?
        .ok_or_else(|| PsmError::AccountNotFound(account_id.to_string()))?;

    let request_timestamp = creds.timestamp();
    let server_now_ms = state.clock.now().timestamp_millis();
    let time_diff_ms = (server_now_ms - request_timestamp).abs();
    if time_diff_ms > MAX_TIMESTAMP_SKEW_MS {
        tracing::warn!(
            account_id = %account_id,
            request_timestamp = %request_timestamp,
            server_now_ms = %server_now_ms,
            time_diff_ms = %time_diff_ms,
            max_skew_ms = %MAX_TIMESTAMP_SKEW_MS,
            "Request timestamp outside allowed skew window"
        );
        return Err(PsmError::AuthenticationFailed(format!(
            "Request timestamp outside allowed window: {}ms drift (max {}ms)",
            time_diff_ms, MAX_TIMESTAMP_SKEW_MS
        )));
    }

    metadata.auth.verify(account_id, creds).map_err(|e| {
        tracing::warn!(
            account_id = %account_id,
            error = %e,
            "Authentication failed in resolve_account"
        );
        PsmError::AuthenticationFailed(e)
    })?;

    // Atomically check and update the last auth timestamp for replay protection
    let now_str = state.clock.now_rfc3339();
    let updated = state
        .metadata
        .update_last_auth_timestamp_cas(account_id, request_timestamp, &now_str)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to update last auth timestamp"
            );
            PsmError::StorageError(format!("Failed to update last auth timestamp: {e}"))
        })?;

    if !updated {
        tracing::warn!(
            account_id = %account_id,
            request_timestamp = %request_timestamp,
            "Replay attack detected: timestamp not greater than last seen (CAS failed)"
        );
        return Err(PsmError::AuthenticationFailed(
            "Replay attack detected: timestamp must be greater than previous request".to_string(),
        ));
    }

    let storage = state.storage.clone();

    Ok(ResolvedAccount { metadata, storage })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::Clock;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::auth::Auth;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::{TimeZone, Utc};
    use tokio::sync::Mutex;

    fn create_test_state_with_mocks_and_clock(
        metadata: MockMetadataStore,
        clock: MockClock,
    ) -> AppState {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();

        let keystore_dir =
            std::env::temp_dir().join(format!("psm_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");
        let ack = AckRegistry::new(keystore_dir).expect("Failed to create ack registry");

        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(network)),
            ack,
            canonicalization: None,
            clock: Arc::new(clock),
        }
    }

    fn create_account_metadata(account_id: String, commitments: Vec<String>) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: commitments,
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    #[tokio::test]
    async fn test_resolve_account_timestamp_too_old() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let (signer_pubkey, signer_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock
        let metadata = MockMetadataStore::new().with_get(Ok(Some(create_account_metadata(
            account_id.to_string(),
            vec![signer_commitment],
        ))));

        let state = create_test_state_with_mocks_and_clock(metadata, clock.clone());

        // Create credentials with timestamp way in the past (10 minutes = 600000ms ago)
        let old_timestamp = clock.now().timestamp_millis() - 600_000;
        let (old_signature, _) = crate::testing::helpers::TestSigner::new()
            .sign_with_timestamp(account_id, old_timestamp);
        let creds = Credentials::signature(signer_pubkey, old_signature, old_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::AuthenticationFailed(msg) => {
                assert!(msg.contains("outside allowed window"));
            }
            e => panic!("Expected AuthenticationFailed, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_timestamp_in_future() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let (signer_pubkey, signer_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock
        let metadata = MockMetadataStore::new().with_get(Ok(Some(create_account_metadata(
            account_id.to_string(),
            vec![signer_commitment],
        ))));

        let state = create_test_state_with_mocks_and_clock(metadata, clock.clone());

        // Create credentials with timestamp way in the future (10 minutes = 600000ms ahead)
        let future_timestamp = clock.now().timestamp_millis() + 600_000;
        let (future_signature, _) = crate::testing::helpers::TestSigner::new()
            .sign_with_timestamp(account_id, future_timestamp);
        let creds = Credentials::signature(signer_pubkey, future_signature, future_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::AuthenticationFailed(msg) => {
                assert!(msg.contains("outside allowed window"));
            }
            e => panic!("Expected AuthenticationFailed, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_replay_attack_detected() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7bfb0f38b0fafa103f86a805594170";

        // Create a signer and generate signature with the mock clock's timestamp
        let test_signer = crate::testing::helpers::TestSigner::new();
        let timestamp = clock.now().timestamp_millis();
        let (signature, _) = test_signer.sign_with_timestamp(account_id, timestamp);

        // Configure metadata mock with CAS returning false (replay detected)
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(create_account_metadata(
                account_id.to_string(),
                vec![test_signer.commitment_hex.clone()],
            ))))
            .with_update_timestamp_cas(Ok(false));

        let state = create_test_state_with_mocks_and_clock(metadata, clock);

        let creds = Credentials::signature(test_signer.pubkey_hex, signature, timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::AuthenticationFailed(msg) => {
                assert!(msg.contains("Replay attack detected"));
            }
            e => panic!("Expected AuthenticationFailed with replay, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_cas_storage_error() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7bfb0f38b0fafa103f86a805594170";

        // Create a signer and generate signature with the mock clock's timestamp
        let test_signer = crate::testing::helpers::TestSigner::new();
        let timestamp = clock.now().timestamp_millis();
        let (signature, _) = test_signer.sign_with_timestamp(account_id, timestamp);

        // Configure metadata mock with CAS returning error
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(create_account_metadata(
                account_id.to_string(),
                vec![test_signer.commitment_hex.clone()],
            ))))
            .with_update_timestamp_cas(Err("Database connection failed".to_string()));

        let state = create_test_state_with_mocks_and_clock(metadata, clock);

        let creds = Credentials::signature(test_signer.pubkey_hex, signature, timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::StorageError(msg) => {
                assert!(msg.contains("Failed to update last auth timestamp"));
            }
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_not_found() {
        let clock = MockClock::default();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let (signer_pubkey, _, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock to return None (account not found)
        let metadata = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_state_with_mocks_and_clock(metadata, clock);

        let creds = Credentials::signature(signer_pubkey, signer_signature, signer_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::AccountNotFound(_) => {}
            e => panic!("Expected AccountNotFound, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_metadata_storage_error() {
        let clock = MockClock::default();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let (signer_pubkey, _, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock to return error
        let metadata = MockMetadataStore::new().with_get(Err("Database error".to_string()));

        let state = create_test_state_with_mocks_and_clock(metadata, clock);

        let creds = Credentials::signature(signer_pubkey, signer_signature, signer_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::StorageError(msg) => {
                assert!(msg.contains("Failed to check account"));
            }
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }
}
