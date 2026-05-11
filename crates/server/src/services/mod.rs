use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::auth::{Credentials, MAX_TIMESTAMP_SKEW_MS};
use crate::state::AppState;
use crate::storage::StorageBackend;
use base64::Engine;
use serde_json::Value;
use std::sync::Arc;

mod configure_account;
mod dashboard_account_deltas;
mod dashboard_account_proposals;
mod dashboard_account_snapshot;
mod dashboard_accounts;
mod dashboard_global_deltas;
mod dashboard_global_proposals;
mod dashboard_info;
mod dashboard_pagination;
mod delta_commit;
mod get_delta;
mod get_delta_proposal;
mod get_delta_proposals;
mod get_delta_since;
mod get_state;
mod push_delta;
mod push_delta_proposal;
mod sign_delta_proposal;

pub use crate::jobs::canonicalization::{
    process_canonicalizations_now, start_canonicalization_worker,
};
pub use configure_account::{ConfigureAccountParams, ConfigureAccountResult, configure_account};
pub use dashboard_account_deltas::{
    DashboardDeltaEntry, DashboardDeltaStatus, list_account_deltas,
};
pub use dashboard_account_proposals::{DashboardProposalEntry, list_account_proposals};
pub use dashboard_account_snapshot::{
    DashboardAccountSnapshot, DashboardVaultFungibleEntry, DashboardVaultNonFungibleEntry,
    DashboardVaultSnapshot, get_account_snapshot,
};
pub use dashboard_accounts::{
    DashboardAccountDetail, DashboardAccountStateStatus, DashboardAccountSummary,
    GetDashboardAccountResult, get_dashboard_account, list_dashboard_accounts_paged,
};
pub use dashboard_global_deltas::{
    DashboardGlobalDeltaEntry, list_global_deltas, parse_status_filter,
};
pub use dashboard_global_proposals::{DashboardGlobalProposalEntry, list_global_proposals};
pub use dashboard_info::{
    AGG_DELTA_STATUS_COUNTS, AGG_IN_FLIGHT_PROPOSAL_COUNT, AGG_LATEST_ACTIVITY,
    DashboardDeltaStatusCounts, DashboardInfoResponse, DashboardServiceStatus, get_dashboard_info,
};
pub use dashboard_pagination::{DEFAULT_LIMIT, MAX_LIMIT, PagedResult, parse_cursor, parse_limit};
pub use get_delta::{GetDeltaParams, GetDeltaResult, get_delta};
pub use get_delta_proposal::{GetDeltaProposalParams, GetDeltaProposalResult, get_delta_proposal};
pub use get_delta_proposals::{
    GetDeltaProposalsParams, GetDeltaProposalsResult, get_delta_proposals,
};
pub use get_delta_since::{GetDeltaSinceParams, GetDeltaSinceResult, get_delta_since};
pub use get_state::{GetStateParams, GetStateResult, get_state};
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
            GuardianError::StorageError(format!("Failed to check account: {e}"))
        })?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

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
        return Err(GuardianError::AuthenticationFailed(format!(
            "Request timestamp outside allowed window: {}ms drift (max {}ms)",
            time_diff_ms, MAX_TIMESTAMP_SKEW_MS
        )));
    }

    if metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "delta_api".to_string(),
        });
    } else {
        if matches!(metadata.auth, crate::metadata::Auth::EvmEcdsa { .. }) {
            return Err(GuardianError::UnsupportedForNetwork {
                network: "evm".to_string(),
                operation: "delta_api".to_string(),
            });
        }

        metadata.auth.verify(account_id, creds).map_err(|e| {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "Authentication failed in resolve_account"
            );
            GuardianError::AuthenticationFailed(e)
        })?;
    }

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
            GuardianError::StorageError(format!("Failed to update last auth timestamp: {e}"))
        })?;

    if !updated {
        tracing::warn!(
            account_id = %account_id,
            request_timestamp = %request_timestamp,
            "Replay attack detected: timestamp not greater than last seen (CAS failed)"
        );
        return Err(GuardianError::AuthenticationFailed(
            "Replay attack detected: timestamp must be greater than previous request".to_string(),
        ));
    }

    let storage = state.storage.clone();

    Ok(ResolvedAccount { metadata, storage })
}

const VALID_PROPOSAL_TYPES: &[&str] = &[
    "add_signer",
    "remove_signer",
    "change_threshold",
    "update_procedure_threshold",
    "switch_guardian",
    "consume_notes",
    "p2id",
];

pub fn normalize_payload(payload: Value) -> Result<Value> {
    let mut obj = payload.as_object().cloned().ok_or_else(|| {
        GuardianError::InvalidDelta("delta_payload must be an object".to_string())
    })?;

    let tx_summary = obj
        .get("tx_summary")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;
    validate_tx_summary(tx_summary)?;

    let metadata = obj
        .remove("metadata")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing 'metadata' field".to_string()))?;
    let normalized_metadata = normalize_metadata(metadata)?;
    obj.insert("metadata".to_string(), normalized_metadata);

    Ok(Value::Object(obj))
}

fn validate_tx_summary(tx_summary: &Value) -> Result<()> {
    let obj = tx_summary.as_object().ok_or_else(|| {
        GuardianError::InvalidDelta("tx_summary must be an object with 'data' field".to_string())
    })?;

    let data = obj.get("data").and_then(Value::as_str).ok_or_else(|| {
        GuardianError::InvalidDelta("tx_summary.data must be a string".to_string())
    })?;

    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| {
            GuardianError::InvalidDelta(format!("tx_summary.data is not valid base64: {e}"))
        })?;
    Ok(())
}

fn normalize_metadata(metadata: Value) -> Result<Value> {
    let mut obj = metadata
        .as_object()
        .cloned()
        .ok_or_else(|| GuardianError::InvalidDelta("metadata must be a JSON object".to_string()))?;

    let proposal_type = obj
        .get("proposal_type")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            GuardianError::InvalidDelta("metadata.proposal_type is required".to_string())
        })?;
    if !VALID_PROPOSAL_TYPES.contains(&proposal_type) {
        return Err(GuardianError::InvalidDelta(format!(
            "Unknown proposal_type '{}'. Must be one of: {}",
            proposal_type,
            VALID_PROPOSAL_TYPES.join(", ")
        )));
    }
    obj.insert(
        "proposal_type".to_string(),
        Value::String(proposal_type.to_string()),
    );

    obj.entry("description")
        .or_insert_with(|| Value::String(String::new()));

    if let Some(amount) = obj.get("amount") {
        if let Some(num) = amount.as_u64() {
            obj.insert("amount".to_string(), Value::String(num.to_string()));
        } else if let Some(num) = amount.as_i64() {
            obj.insert("amount".to_string(), Value::String(num.to_string()));
        }
    }

    if let Some(required_signatures) = obj.get("required_signatures") {
        let normalized = if let Some(num) = required_signatures.as_u64() {
            num
        } else if let Some(text) = required_signatures.as_str() {
            text.parse::<u64>().map_err(|_| {
                GuardianError::InvalidDelta(
                    "metadata.required_signatures must be a positive integer".to_string(),
                )
            })?
        } else {
            return Err(GuardianError::InvalidDelta(
                "metadata.required_signatures must be a positive integer".to_string(),
            ));
        };

        if normalized == 0 {
            return Err(GuardianError::InvalidDelta(
                "metadata.required_signatures must be greater than zero".to_string(),
            ));
        }

        obj.insert(
            "required_signatures".to_string(),
            Value::Number(serde_json::Number::from(normalized)),
        );
    }

    Ok(Value::Object(obj))
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_payload;
    use serde_json::{Value, json};

    #[test]
    fn normalize_payload_accepts_update_procedure_threshold_metadata() {
        let payload = json!({
            "tx_summary": { "data": "dGVzdA==" },
            "signatures": [],
            "metadata": {
                "proposal_type": "update_procedure_threshold",
                "target_threshold": 1,
                "target_procedure": "send_asset",
                "required_signatures": "2",
                "description": "set override"
            }
        });

        let normalized = normalize_payload(payload).expect("payload should normalize");
        let metadata = normalized
            .get("metadata")
            .and_then(Value::as_object)
            .expect("metadata should be an object");

        assert_eq!(
            metadata.get("proposal_type").and_then(Value::as_str),
            Some("update_procedure_threshold")
        );
        assert_eq!(
            metadata.get("target_procedure").and_then(Value::as_str),
            Some("send_asset")
        );
        assert_eq!(
            metadata.get("required_signatures").and_then(Value::as_u64),
            Some(2)
        );
    }
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

    async fn create_test_state_with_mocks_and_clock(
        metadata: MockMetadataStore,
        clock: MockClock,
    ) -> AppState {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");
        let ack = AckRegistry::new(keystore_dir)
            .await
            .expect("Failed to create ack registry");

        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(network)),
            ack,
            canonicalization: None,
            clock: Arc::new(clock),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    fn create_account_metadata(account_id: String, commitments: Vec<String>) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: commitments,
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
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

        let state = create_test_state_with_mocks_and_clock(metadata, clock.clone()).await;

        // Create credentials with timestamp way in the past (10 minutes = 600000ms ago)
        let old_timestamp = clock.now().timestamp_millis() - 600_000;
        let (old_signature, _) = crate::testing::helpers::TestSigner::new()
            .sign_with_timestamp(account_id, old_timestamp);
        let creds = Credentials::signature(signer_pubkey, old_signature, old_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthenticationFailed(msg) => {
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

        let state = create_test_state_with_mocks_and_clock(metadata, clock.clone()).await;

        // Create credentials with timestamp way in the future (10 minutes = 600000ms ahead)
        let future_timestamp = clock.now().timestamp_millis() + 600_000;
        let (future_signature, _) = crate::testing::helpers::TestSigner::new()
            .sign_with_timestamp(account_id, future_timestamp);
        let creds = Credentials::signature(signer_pubkey, future_signature, future_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthenticationFailed(msg) => {
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

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(test_signer.pubkey_hex, signature, timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthenticationFailed(msg) => {
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

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(test_signer.pubkey_hex, signature, timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StorageError(msg) => {
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

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(signer_pubkey, signer_signature, signer_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AccountNotFound(_) => {}
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

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(signer_pubkey, signer_signature, signer_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StorageError(msg) => {
                assert!(msg.contains("Failed to check account"));
            }
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }
}
