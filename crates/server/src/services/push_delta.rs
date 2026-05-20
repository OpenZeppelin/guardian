use guardian_shared::SignatureScheme;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active;
use crate::services::delta_commit::{CommitContext, DeltaCommitStrategy};
use crate::services::resolve_account;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.delta.account_id)
)]
pub async fn push_delta(state: &AppState, params: PushDeltaParams) -> Result<PushDeltaResult> {
    tracing::info!(account_id = %params.delta.account_id, "Pushing delta");

    // Feature 001-account-pausing chokepoint (FR-008 / FR-025).
    ensure_account_active(state, &params.delta.account_id).await?;

    let resolved = resolve_account(state, &params.delta.account_id, &params.credentials).await?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "push_delta".to_string(),
        });
    }

    let current_state = resolved
        .storage
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to fetch account state in push_delta"
            );
            GuardianError::StorageError(format!("Failed to fetch account state: {e}"))
        })?;

    // Check for pending candidates before accepting new delta
    let has_pending = resolved
        .storage
        .has_pending_candidate(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to check deltas in push_delta"
            );
            GuardianError::StorageError(format!("Failed to check deltas: {e}"))
        })?;

    if has_pending {
        return Err(GuardianError::ConflictPendingDelta);
    }

    if params.delta.prev_commitment != current_state.commitment {
        return Err(GuardianError::CommitmentMismatch {
            expected: current_state.commitment.clone(),
            actual: params.delta.prev_commitment.clone(),
        });
    }

    let (new_state_json, new_commitment) = {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &current_state.commitment,
                &current_state.state_json,
                &params.delta.delta_payload,
            )
            .map_err(GuardianError::InvalidDelta)?;
        client
            .apply_delta(&current_state.state_json, &params.delta.delta_payload)
            .map_err(GuardianError::InvalidDelta)?
    };

    let mut result_delta = params.delta.clone();
    result_delta.new_commitment = Some(new_commitment.clone());
    let scheme = resolved.metadata.auth.scheme();
    result_delta = state.ack.ack_delta(result_delta, &scheme)?;
    result_delta.ack_pubkey = state.ack.pubkey(&scheme);
    result_delta.ack_scheme = match scheme {
        SignatureScheme::Falcon => "falcon",
        SignatureScheme::Ecdsa => "ecdsa",
    }
    .to_string();

    let now = state.clock.now_rfc3339();
    let commit_strategy = DeltaCommitStrategy::from_app_state(state);
    commit_strategy
        .commit(
            CommitContext {
                state,
                resolved: &resolved,
                current_state: &current_state,
                now,
            },
            &mut result_delta,
            new_state_json,
            &new_commitment,
        )
        .await?;

    Ok(PushDeltaResult {
        delta: result_delta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::TimeZone;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn paused_metadata(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                    .unwrap(),
            ),
            paused_reason: Some("compliance".to_string()),
        }
    }

    /// Pause-gate guard: `push_delta` MUST reject before touching
    /// storage or the network. Defends against a refactor that
    /// silently drops the `ensure_account_active` call.
    #[tokio::test]
    async fn paused_account_rejected_before_side_effects() {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new().with_get(Ok(Some(paused_metadata("acc-paused"))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: "acc-paused".to_string(),
                ..Default::default()
            },
            credentials: Credentials::signature(String::new(), String::new(), 0),
        };

        let err = push_delta(&state, params)
            .await
            .expect_err("paused account must be rejected");
        assert!(
            matches!(err, GuardianError::AccountPaused { ref paused_reason, .. }
                if paused_reason.as_deref() == Some("compliance")),
            "unexpected error: {err:?}"
        );

        assert!(
            storage.get_submit_delta_calls().is_empty(),
            "no delta should be submitted when the account is paused"
        );
    }
}
