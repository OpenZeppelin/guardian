//! Shared test utilities
//!
//! This module contains helper functions and utilities used across multiple test files.

#[cfg(test)]
pub mod test_helpers {
    use std::collections::HashMap;
    use std::sync::Arc;

    use miden_objects::account::{AccountDelta, AccountId, AccountStorageDelta, AccountVaultDelta};
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
    use miden_objects::crypto::hash::rpo::Rpo256;
    use miden_objects::utils::Serializable;
    use miden_objects::{Felt, FieldElement, Word};
    use private_state_manager_shared::ToJson;

    use server::api::grpc::StateManagerService;
    use server::network::NetworkType;
    use server::state::AppState;
    use server::storage::filesystem::{FilesystemMetadataStore, FilesystemService};
    use server::storage::{StorageBackend, StorageRegistry, StorageType};

    // Re-export types needed by test functions
    pub use server::api::grpc::state_manager::*;
    pub use tonic::{Request, metadata::MetadataValue};

    /// Create test app state with temporary storage and metadata
    pub async fn create_test_app_state() -> AppState {
        // Create temporary directories for test storage
        let storage_dir =
            std::env::temp_dir().join(format!("psm_test_storage_{}", uuid::Uuid::new_v4()));
        let metadata_dir =
            std::env::temp_dir().join(format!("psm_test_metadata_{}", uuid::Uuid::new_v4()));

        std::fs::create_dir_all(&storage_dir).expect("Failed to create storage directory");
        std::fs::create_dir_all(&metadata_dir).expect("Failed to create metadata directory");

        let storage = FilesystemService::new(storage_dir)
            .await
            .expect("Failed to create storage");
        let metadata = FilesystemMetadataStore::new(metadata_dir)
            .await
            .expect("Failed to create metadata");

        // Create storage registry
        let mut storage_backends: HashMap<StorageType, Arc<dyn StorageBackend>> = HashMap::new();
        storage_backends.insert(StorageType::Filesystem, Arc::new(storage));
        let storage_registry = StorageRegistry::new(storage_backends);

        // Create network client
        let network_client =
            server::network::miden::MidenNetworkClient::from_network(NetworkType::MidenTestnet)
                .await
                .expect("Failed to create network client");

        AppState {
            storage: storage_registry,
            metadata: Arc::new(metadata),
            network_client: Arc::new(tokio::sync::Mutex::new(network_client)),
            canonicalization_mode: server::canonicalization::CanonicalizationMode::default(),
        }
    }

    /// Create gRPC service from app state
    pub fn create_grpc_service(state: AppState) -> StateManagerService {
        StateManagerService { app_state: state }
    }

    /// Create a gRPC request with authentication metadata
    ///
    /// # Arguments
    /// * `payload` - The request payload
    /// * `pubkey` - Publisher public key (hex string with 0x prefix)
    /// * `sig` - Publisher signature (hex string with 0x prefix)
    pub fn create_request_with_auth<T>(payload: T, pubkey: &str, sig: &str) -> Request<T> {
        let mut request = Request::new(payload);
        let metadata = request.metadata_mut();

        metadata.insert(
            "x-pubkey",
            MetadataValue::try_from(pubkey).expect("Valid pubkey metadata"),
        );
        metadata.insert(
            "x-signature",
            MetadataValue::try_from(sig).expect("Valid sig metadata"),
        );

        request
    }

    /// Create AuthConfig for Miden Falcon RPO
    pub fn create_miden_falcon_rpo_auth(cosigner_pubkeys: Vec<String>) -> AuthConfig {
        AuthConfig {
            auth_type: Some(auth_config::AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                cosigner_pubkeys,
            })),
        }
    }

    /// Create HTTP router with all routes configured
    pub fn create_router(state: AppState) -> axum::Router {
        use server::api::http;

        axum::Router::new()
            .route("/configure", axum::routing::post(http::configure))
            .route("/push_delta", axum::routing::post(http::push_delta))
            .route("/get_delta", axum::routing::get(http::get_delta))
            .route("/get_delta_head", axum::routing::get(http::get_delta_head))
            .route("/get_state", axum::routing::get(http::get_state))
            .with_state(state)
    }

    /// Load the test account fixture from fixtures/account.json
    pub fn load_fixture_account() -> (AccountId, String, serde_json::Value) {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("account.json");

        let fixture_contents =
            std::fs::read_to_string(&fixture_path).expect("Failed to read fixture file");

        let fixture_json: serde_json::Value =
            serde_json::from_str(&fixture_contents).expect("Failed to parse fixture JSON");

        let account_id_hex = fixture_json["account_id"]
            .as_str()
            .expect("No account_id in fixture")
            .to_string();

        let account_id =
            AccountId::from_hex(&account_id_hex).expect("Invalid account ID in fixture");

        (account_id, account_id_hex, fixture_json)
    }

    /// Load fixture account for gRPC tests that need a String representation
    pub fn load_fixture_account_grpc() -> (AccountId, String, String) {
        let (account_id, account_id_hex, fixture_json) = load_fixture_account();
        let fixture_string =
            serde_json::to_string(&fixture_json).expect("Failed to serialize fixture JSON");
        (account_id, account_id_hex, fixture_string)
    }

    /// Helper to get a test account ID (old API for backward compatibility)
    /// Uses a real account from Miden testnet that exists on-chain
    #[allow(dead_code)]
    pub fn get_test_account_id() -> (AccountId, String) {
        let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
        let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");
        (account_id, account_id_hex.to_string())
    }

    /// Load delta fixture by number (1 or 2)
    pub fn load_fixture_delta(delta_num: u8) -> serde_json::Value {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(format!("delta_{}.json", delta_num));

        let fixture_contents =
            std::fs::read_to_string(&fixture_path).expect("Failed to read delta fixture");

        serde_json::from_str(&fixture_contents).expect("Failed to parse delta fixture")
    }

    /// Load the test delta fixture from fixtures/delta.json (old API)
    #[allow(dead_code)]
    pub fn load_fixture_delta_old() -> (AccountId, String, serde_json::Value) {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("delta.json");

        let fixture_contents =
            std::fs::read_to_string(&fixture_path).expect("Failed to read delta fixture file");

        let fixture_json: serde_json::Value =
            serde_json::from_str(&fixture_contents).expect("Failed to parse delta fixture JSON");

        let account_id_hex = fixture_json["account_id"]
            .as_str()
            .expect("No account_id in delta fixture")
            .to_string();

        let account_id =
            AccountId::from_hex(&account_id_hex).expect("Invalid account ID in delta fixture");

        (account_id, account_id_hex, fixture_json)
    }

    /// Create a test AccountDelta JSON payload with valid base64-encoded delta bytes
    /// This creates an in-memory delta for the given account ID
    pub fn create_test_delta_payload(account_id_hex: &str) -> serde_json::Value {
        let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");

        // Create an empty delta (no storage or vault changes, nonce delta of 0)
        let delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("Valid empty delta");

        delta.to_json()
    }

    /// Generate a Falcon key pair and signature for the given account ID
    pub fn generate_falcon_signature(account_id_hex: &str) -> (String, String, String) {
        // Generate key pair
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        // Create message digest (same as in verification)
        let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");
        let account_id_felts: [Felt; 2] = account_id.into();

        let message_elements = vec![
            account_id_felts[0],
            account_id_felts[1],
            Felt::ZERO,
            Felt::ZERO,
        ];

        let digest = Rpo256::hash_elements(&message_elements);
        let message: Word = digest;

        // Sign the message
        let signature = secret_key.sign(message);

        // Convert to hex strings
        let pubkey_word: Word = public_key.into();
        let pubkey_hex = format!("0x{}", hex::encode(pubkey_word.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        (account_id_hex.to_string(), pubkey_hex, signature_hex)
    }
}
