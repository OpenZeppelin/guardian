//! Canonical nonce pre-check (issue #191): the nonce and commitment of the
//! state GUARDIAN currently holds as canonical, without the state blob.
//! A client has nothing to pull, and can skip the full `get_state` fetch,
//! when this nonce is below its local nonce, or equal to it at the same
//! commitment.
//!
//! Both values are stored with the state when it is written, so the
//! response comes from a two-column read: no state blob is loaded,
//! decrypted, or decoded. A row written before the server stored nonces
//! (see [`crate::state_object::StateObject::nonce`]) is decoded once here
//! and its nonce stored for the next call.

use crate::coordination::read_records::PrefetchedHead;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::network::ReconstructError;
use crate::services::{ResolvedAccount, resolve_account_for_head_read};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
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
    level = "debug",
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn get_canonical_nonce(
    state: &AppState,
    params: GetCanonicalNonceParams,
) -> Result<CanonicalNonceResponse> {
    tracing::debug!("Getting canonical nonce");

    let (resolved, prefetched) =
        resolve_account_for_head_read(state, &params.account_id, &params.credentials).await?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "get_canonical_nonce".to_string(),
        });
    }

    let head = match prefetched {
        PrefetchedHead::Found(head) => head,
        PrefetchedHead::Missing => {
            return Err(GuardianError::StateNotFound(params.account_id));
        }
        PrefetchedHead::NotRead => resolved
            .storage
            .pull_state_head(&params.account_id)
            .await
            .map_err(|e| state_read_error(&params.account_id, e))?,
    };

    match head.nonce {
        Some(nonce) => Ok(CanonicalNonceResponse {
            account_id: params.account_id,
            nonce,
            commitment: head.commitment,
        }),
        None => backfill_canonical_nonce(state, &resolved, params.account_id).await,
    }
}

/// A missing state row is `state_not_found` (404); any other read failure
/// (pool exhaustion, a dropped connection, an undecryptable row) is a
/// retryable storage error rather than a spurious 404.
fn state_read_error(account_id: &str, error: String) -> GuardianError {
    if crate::storage::is_storage_not_found(&error) {
        return GuardianError::StateNotFound(account_id.to_string());
    }
    tracing::error!(
        account_id = %account_id,
        error = %error,
        "Failed to read the canonical state"
    );
    GuardianError::StorageError(format!("Failed to read the canonical state: {error}"))
}

/// The canonical nonce of a stored state that does not carry one: decode
/// the state once, answer from it, and store the nonce so later calls read
/// it from the row ([`canonical_account_nonce`]).
async fn backfill_canonical_nonce(
    state: &AppState,
    resolved: &ResolvedAccount,
    account_id: String,
) -> Result<CanonicalNonceResponse> {
    let account_state = resolved
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|e| state_read_error(&account_id, e))?;

    let nonce = canonical_account_nonce(state, resolved.storage.as_ref(), &account_state)
        .await?
        .ok_or_else(|| {
            // Unreachable for Miden (the only non-EVM network, and its
            // client always reads a nonce); a backend without the notion
            // cannot serve the pre-check.
            tracing::error!(
                account_id = %account_id,
                "Network client reports no nonce for the canonical state"
            );
            GuardianError::AccountDataUnavailable(account_id.clone())
        })?;

    Ok(CanonicalNonceResponse {
        account_id: account_state.account_id,
        nonce,
        commitment: account_state.commitment,
    })
}

/// The account nonce of the canonical state `current_state` holds: the
/// nonce stored with it, or, for a row that does not carry one (see
/// [`StateObject::nonce`]), the nonce its state decodes to, read on the
/// reconstruction pool and stored onto the row so later reads find it
/// there. The store is best effort: the answer is correct either way, a row
/// it misses is decoded again on its next read, and it only lands while the
/// row still holds the decoded state's commitment, so a concurrent write is
/// never overwritten and never paired with this nonce. `None` on a network
/// without account nonces. A state that no longer decodes is
/// `account_data_unavailable`.
pub(crate) async fn canonical_account_nonce(
    state: &AppState,
    storage: &dyn StorageBackend,
    current_state: &StateObject,
) -> Result<Option<u64>> {
    if let Some(nonce) = current_state.nonce {
        return Ok(Some(nonce));
    }
    let account_id = &current_state.account_id;
    let client = state.network_client.clone();
    let state_json = current_state.state_json.clone();
    let decoded = crate::network::reconstructor()
        .run(move || client.account_nonce(&state_json))
        .await
        .map_err(|error| match error {
            ReconstructError::Operation(message) => {
                tracing::error!(
                    account_id = %account_id,
                    error = %message,
                    "Canonical state blob does not decode to an account"
                );
                GuardianError::AccountDataUnavailable(account_id.clone())
            }
            task => GuardianError::from(task),
        })?;
    let Some(nonce) = decoded else {
        return Ok(None);
    };
    match storage
        .backfill_state_nonce(account_id, &current_state.commitment, nonce)
        .await
    {
        Ok(stored) => tracing::info!(
            account_id = %account_id,
            nonce,
            stored,
            "Canonical state had no stored nonce; decoded it for the backfill"
        ),
        Err(e) => tracing::warn!(
            account_id = %account_id,
            nonce,
            error = %e,
            "Failed to store the decoded canonical nonce; the next read decodes again"
        ),
    }
    Ok(Some(nonce))
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

    /// A stored state at commitment `0xabc`; `nonce` is its stored nonce
    /// (`None` for a row written before the server stored nonces).
    fn stored_state(nonce: Option<u64>, state_json: serde_json::Value) -> StateObject {
        StateObject {
            account_id: ACCOUNT_ID.to_string(),
            commitment: "0xabc".to_string(),
            nonce,
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

    fn known_account(metadata: &MockMetadataStore, signer: &TestSigner) {
        let _metadata = metadata.clone().with_get(Ok(Some(create_account_metadata(
            ACCOUNT_ID.to_string(),
            vec![signer.commitment_hex.clone()],
        ))));
    }

    #[tokio::test]
    async fn serves_the_stored_head_without_loading_the_state() {
        let (state, storage, _network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        // No `account_nonce` answer is queued: a decode would read the
        // mock's `Ok(None)` default and fail with 503.
        let _storage = storage.clone().with_pull_state(Ok(stored_state(
            Some(7),
            serde_json::json!({ "data": "opaque" }),
        )));

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
        assert_eq!(
            storage.pending_pull_state_responses(),
            1,
            "the head read must not load the state"
        );
        assert!(storage.get_backfill_state_nonce_calls().is_empty());
    }

    #[tokio::test]
    async fn decodes_and_backfills_a_state_without_a_stored_nonce() {
        let (state, storage, network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        let _storage = storage.clone().with_pull_state(Ok(stored_state(
            None,
            serde_json::json!({ "data": "opaque" }),
        )));
        let _network = network.with_account_nonce(Ok(Some(7)));

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
        assert_eq!(
            storage.get_backfill_state_nonce_calls(),
            vec![(ACCOUNT_ID.to_string(), "0xabc".to_string(), 7)],
            "the decoded nonce is stored against the commitment it was decoded from"
        );
    }

    #[tokio::test]
    async fn a_failed_backfill_still_answers() {
        let (state, storage, network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        let _storage = storage
            .clone()
            .with_pull_state(Ok(stored_state(
                None,
                serde_json::json!({ "data": "opaque" }),
            )))
            .with_backfill_state_nonce(Err("connection reset".to_string()));
        let _network = network.with_account_nonce(Ok(Some(7)));

        let response = get_canonical_nonce(&state, params(&signer))
            .await
            .expect("a failed backfill must not fail the read");

        assert_eq!(response.nonce, 7);
        assert_eq!(response.commitment, "0xabc");
        assert_eq!(storage.get_backfill_state_nonce_calls().len(), 1);
    }

    #[tokio::test]
    async fn missing_state_is_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        let _storage = storage.with_pull_state(Err("Record not found".to_string()));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("missing state should fail");

        assert!(matches!(err, GuardianError::StateNotFound(_)));
        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_failed_state_read_is_a_retryable_storage_error_not_a_404() {
        let (state, storage, _network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        let _storage = storage.with_pull_state(Err(
            "Failed to get connection: timed out waiting for a pool slot".to_string(),
        ));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("a storage failure should fail");

        assert!(matches!(err, GuardianError::StorageError(_)), "got {err:?}");
        assert_eq!(err.http_status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.retryable());
    }

    #[tokio::test]
    async fn undecodable_state_without_a_stored_nonce_is_account_data_unavailable() {
        let (state, storage, network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        let _storage = storage
            .clone()
            .with_pull_state(Ok(stored_state(None, serde_json::json!({ "balance": 0 }))));
        let _network = network.with_account_nonce(Err("no data field".to_string()));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("undecodable state should fail");

        assert!(matches!(err, GuardianError::AccountDataUnavailable(_)));
        assert_eq!(err.http_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(storage.get_backfill_state_nonce_calls().is_empty());
    }

    #[tokio::test]
    async fn state_without_a_nonce_is_account_data_unavailable() {
        let (state, storage, network, metadata) = create_test_state();
        let signer = TestSigner::new();
        known_account(&metadata, &signer);
        let _storage = storage.clone().with_pull_state(Ok(stored_state(
            None,
            serde_json::json!({ "data": "opaque" }),
        )));
        let _network = network.with_account_nonce(Ok(None));

        let err = get_canonical_nonce(&state, params(&signer))
            .await
            .expect_err("a state without a nonce should fail");

        assert!(matches!(err, GuardianError::AccountDataUnavailable(_)));
        assert_eq!(err.http_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(storage.get_backfill_state_nonce_calls().is_empty());
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
