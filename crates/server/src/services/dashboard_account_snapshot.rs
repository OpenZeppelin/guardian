//! Decoded account snapshot for the operator dashboard.
//!
//! Spec reference: feature `005-operator-dashboard-metrics` follow-up
//! addition. The snapshot endpoint surfaces views derived from the
//! account's *stored* state at the commitment Guardian last
//! canonicalized — no live Miden RPC calls, no aggregations across
//! accounts, no joins with delta feed. The endpoint stays cheap and
//! snapshot-true.
//!
//! v1 surface:
//!   - vault: fungible and non-fungible asset entries
//!
//! Future fields land as new top-level keys on `SnapshotResponse` —
//! e.g. `storage`, `code` — when concrete needs appear. New fields MUST
//! be additive and derivable from the existing state blob.

use guardian_shared::FromJson;
use guardian_shared::hex::IntoHex;
use miden_protocol::asset::Asset;
use serde::Serialize;

use crate::error::{GuardianError, Result};
use crate::state::AppState;

/// One fungible asset entry in the vault snapshot. `amount` is a string
/// to keep `u64`-precision values safe across JS (`Number.MAX_SAFE_INTEGER`
/// is 2^53 − 1). Decimal handling is a dashboard-client concern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardVaultFungibleEntry {
    pub faucet_id: String,
    pub amount: String,
}

/// One non-fungible asset entry. `vault_key` is the canonical Miden
/// identifier for the asset within the vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardVaultNonFungibleEntry {
    pub faucet_id: String,
    pub vault_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct DashboardVaultSnapshot {
    pub fungible: Vec<DashboardVaultFungibleEntry>,
    pub non_fungible: Vec<DashboardVaultNonFungibleEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardAccountSnapshot {
    /// Commitment of the state the snapshot was decoded from. Equals
    /// `DashboardAccountDetail::current_commitment` for the same
    /// account at the same point in time; callers can correlate the
    /// snapshot with a delta feed entry by matching on this hex.
    pub commitment: String,
    /// RFC3339 wall-clock time of the underlying state row's
    /// `updated_at` column — i.e. when Guardian last persisted the
    /// canonicalized state this snapshot was decoded from. Equals
    /// `DashboardAccountDetail::state_updated_at` for the same account
    /// at the same point in time.
    pub updated_at: String,
    /// True when the account has a candidate delta in flight that has
    /// not yet been canonicalized. The snapshot decodes the *current
    /// canonical state*, not the candidate's projected state — when
    /// this flag is `true` the vault content here may already be
    /// stale relative to the chain. Clients SHOULD surface this in
    /// the UI rather than silently displaying stale data.
    pub has_pending_candidate: bool,
    pub vault: DashboardVaultSnapshot,
}

/// Build the snapshot for `account_id` from Guardian's stored state.
///
/// Errors:
///   - [`GuardianError::AccountNotFound`] (`404`) when no metadata
///     exists.
///   - [`GuardianError::UnsupportedForNetwork`] (`400`,
///     `code: unsupported_for_network`) when the account's
///     `network_config` is EVM — there is no Miden vault to decode for
///     that network. This is a permanent condition for the endpoint,
///     not a transient failure, so it is reported separately from
///     `data_unavailable`.
///   - [`GuardianError::AccountDataUnavailable`] (`503`,
///     `code: account_data_unavailable`) when metadata exists but the
///     state row cannot be loaded, or when the stored state blob fails
///     to deserialize as a Miden `Account`. Both are treated as
///     transient/recoverable.
pub async fn get_account_snapshot(
    state: &AppState,
    account_id: &str,
) -> Result<DashboardAccountSnapshot> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to load metadata: {e}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    // Snapshot is Miden-only by construction. Dispatch off the
    // account's `network_config` rather than pattern-matching on the
    // auth variant — see AGENTS.md §5 ("services dispatch from the
    // account's network config"). EVM accounts have no Miden
    // `AssetVault` to decode.
    if metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "dashboard.account.snapshot".to_string(),
        });
    }

    let account_state = state.storage.pull_state(account_id).await.map_err(|e| {
        tracing::warn!(
            account_id = %account_id,
            error = %e,
            "Dashboard snapshot could not load state"
        );
        GuardianError::AccountDataUnavailable(account_id.to_string())
    })?;

    let account =
        miden_protocol::account::Account::from_json(&account_state.state_json).map_err(|e| {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "Dashboard snapshot could not decode stored Miden Account"
            );
            GuardianError::AccountDataUnavailable(account_id.to_string())
        })?;

    let mut fungible = Vec::new();
    let mut non_fungible = Vec::new();
    for asset in account.vault().assets() {
        match asset {
            Asset::Fungible(a) => fungible.push(DashboardVaultFungibleEntry {
                faucet_id: a.faucet_id().to_hex(),
                amount: a.amount().to_string(),
            }),
            Asset::NonFungible(a) => {
                let key_word = a.vault_key().to_word();
                non_fungible.push(DashboardVaultNonFungibleEntry {
                    faucet_id: a.faucet_id().to_hex(),
                    vault_key: (&key_word).into_hex(),
                });
            }
        }
    }

    Ok(DashboardAccountSnapshot {
        commitment: account_state.commitment.clone(),
        updated_at: account_state.updated_at.clone(),
        has_pending_candidate: metadata.has_pending_candidate,
        vault: DashboardVaultSnapshot {
            fungible,
            non_fungible,
        },
    })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::state_object::StateObject;
    use crate::testing::helpers::load_fixture_account;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use guardian_shared::FromJson;
    use miden_protocol::account::Account;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    async fn build_state(
        metadata: Option<AccountMetadata>,
        stored_state: std::result::Result<StateObject, String>,
    ) -> AppState {
        let mock_metadata = MockMetadataStore::new().with_get(Ok(metadata));
        let mock_storage = MockStorageBackend::new().with_pull_state(stored_state);

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");

        AppState {
            storage: Arc::new(mock_storage),
            metadata: Arc::new(mock_metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    fn falcon_metadata(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-05-11T00:00:00Z".to_string(),
            updated_at: "2026-05-11T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    /// Build a state row backed by the canonical Miden account fixture
    /// shipped under `crates/server/src/testing/fixtures/account.json`.
    /// Used by the happy-path test to exercise `Account::from_json` +
    /// vault decode end-to-end.
    fn fixture_state(account_id: &str) -> StateObject {
        let (_, _, state_json) = load_fixture_account();
        let account = Account::from_json(&state_json).expect("fixture deserializes");
        let commitment = format!("0x{}", hex::encode(account.to_commitment().as_bytes()));
        StateObject {
            account_id: account_id.to_string(),
            state_json,
            commitment,
            created_at: "2026-05-11T00:00:00Z".to_string(),
            updated_at: "2026-05-11T00:01:00Z".to_string(),
            auth_scheme: "falcon".to_string(),
        }
    }

    #[tokio::test]
    async fn returns_account_not_found_when_metadata_missing() {
        let state = build_state(None, Ok(StateObject::default())).await;
        let err = get_account_snapshot(&state, "0xacc").await.unwrap_err();
        assert!(matches!(err, GuardianError::AccountNotFound(_)));
    }

    #[tokio::test]
    async fn returns_unsupported_for_network_for_evm_accounts() {
        let mut meta = falcon_metadata("0xacc");
        meta.network_config = NetworkConfig::Evm {
            chain_id: 11155111,
            account_address: "0x0000000000000000000000000000000000000001".to_string(),
            multisig_validator_address: "0x0000000000000000000000000000000000000002".to_string(),
        };
        // EVM check happens before any storage pull, so the stored state is irrelevant.
        let state = build_state(Some(meta), Ok(StateObject::default())).await;
        let err = get_account_snapshot(&state, "0xacc").await.unwrap_err();
        match err {
            GuardianError::UnsupportedForNetwork { network, operation } => {
                assert_eq!(network, "evm");
                assert_eq!(operation, "dashboard.account.snapshot");
            }
            other => panic!("expected UnsupportedForNetwork, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_data_unavailable_when_state_blob_is_unreadable() {
        // Miden falcon account, but state row pull fails.
        let state = build_state(
            Some(falcon_metadata("0xacc")),
            Err("storage broken".to_string()),
        )
        .await;
        let err = get_account_snapshot(&state, "0xacc").await.unwrap_err();
        assert!(matches!(err, GuardianError::AccountDataUnavailable(_)));
    }

    #[tokio::test]
    async fn returns_data_unavailable_when_state_blob_does_not_decode() {
        // Miden falcon account, state row present but `data` is garbage.
        let bad_state = StateObject {
            account_id: "0xacc".to_string(),
            state_json: serde_json::json!({ "data": "not-base64-bytes!" }),
            commitment: "0xc".to_string(),
            created_at: "2026-05-11T00:00:00Z".to_string(),
            updated_at: "2026-05-11T00:01:00Z".to_string(),
            auth_scheme: "falcon".to_string(),
        };
        let state = build_state(Some(falcon_metadata("0xacc")), Ok(bad_state)).await;
        let err = get_account_snapshot(&state, "0xacc").await.unwrap_err();
        assert!(matches!(err, GuardianError::AccountDataUnavailable(_)));
    }

    #[tokio::test]
    async fn happy_path_decodes_fixture_account_into_snapshot() {
        let stored = fixture_state("0xacc");
        let expected_commitment = stored.commitment.clone();
        let expected_updated_at = stored.updated_at.clone();
        let state = build_state(Some(falcon_metadata("0xacc")), Ok(stored)).await;

        let snapshot = get_account_snapshot(&state, "0xacc").await.unwrap();
        assert_eq!(snapshot.commitment, expected_commitment);
        assert_eq!(snapshot.updated_at, expected_updated_at);
        // Default falcon_metadata builder sets has_pending_candidate=false.
        assert!(!snapshot.has_pending_candidate);

        // Vault is an array (may be empty for the fixture); every
        // produced entry must have a non-empty hex faucet_id and the
        // serialized amount must parse back to a u64.
        for f in &snapshot.vault.fungible {
            assert!(f.faucet_id.starts_with("0x"));
            assert!(f.amount.parse::<u64>().is_ok());
        }
        for nf in &snapshot.vault.non_fungible {
            assert!(nf.faucet_id.starts_with("0x"));
            // vault_key is the canonical Word hex form — 0x + 64 hex chars.
            assert!(nf.vault_key.starts_with("0x"));
            assert_eq!(nf.vault_key.len(), 2 + 64);
        }
    }
}
