use std::collections::HashMap;
use std::sync::Arc;

use crate::ack::AckRegistry;
use crate::api::grpc::GuardianService;
use crate::metadata::auth::Auth;
use crate::metadata::filesystem::FilesystemMetadataStore;
use crate::network::NetworkClient;
use crate::state::AppState;
use crate::storage::StorageBackend;
use crate::storage::filesystem::FilesystemService;
use crate::testing::mocks::MockNetworkClient;
use async_trait::async_trait;
use chrono::Utc;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use guardian_shared::{FromJson, ToJson};
use miden_protocol::account::{AccountDelta, AccountId, AccountStorageDelta, AccountVaultDelta};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{Felt, Word, ZERO};
use prost::Message;

pub use crate::api::grpc::guardian::*;
pub use tonic::{Request, metadata::MetadataValue};

pub struct IntegrationMockNetworkClient {
    miden_client: crate::network::miden::MidenNetworkClient,
    initial_commitments: HashMap<String, String>,
}

impl IntegrationMockNetworkClient {
    pub fn new(miden_client: crate::network::miden::MidenNetworkClient) -> Self {
        Self {
            miden_client,
            initial_commitments: HashMap::new(),
        }
    }

    pub fn register_account(&mut self, account_id: String, commitment: String) {
        self.initial_commitments.insert(account_id, commitment);
    }
}

#[async_trait]
impl NetworkClient for IntegrationMockNetworkClient {
    fn get_state_commitment(
        &self,
        _account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<String, String> {
        use miden_protocol::account::Account;

        let account = Account::from_json(state_json)
            .map_err(|e| format!("Failed to deserialize account: {e}"))?;

        let local_commitment = account.to_commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        Ok(local_commitment_hex)
    }

    async fn verify_state(
        &mut self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<(), String> {
        use miden_protocol::account::Account;

        let account = Account::from_json(state_json)
            .map_err(|e| format!("Failed to deserialize account: {e}"))?;

        let local_commitment = account.to_commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        if let Some(on_chain_commitment) = self.initial_commitments.get(account_id) {
            if &local_commitment_hex != on_chain_commitment {
                return Err(format!(
                    "Commitment mismatch for account '{account_id}': local={local_commitment_hex}, on-chain={on_chain_commitment}"
                ));
            }
        } else {
            self.initial_commitments
                .insert(account_id.to_string(), local_commitment_hex.clone());
        }

        Ok(())
    }

    fn verify_delta(
        &self,
        prev_proof: &str,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(), String> {
        self.miden_client
            .verify_delta(prev_proof, prev_state_json, delta_payload)
    }

    fn apply_delta(
        &self,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(serde_json::Value, String), String> {
        self.miden_client
            .apply_delta(prev_state_json, delta_payload)
    }

    fn merge_deltas(
        &self,
        delta_payloads: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.miden_client.merge_deltas(delta_payloads)
    }

    fn delta_proposal_id(
        &self,
        account_id: &str,
        nonce: u64,
        delta_payload: &serde_json::Value,
    ) -> Result<String, String> {
        self.miden_client
            .delta_proposal_id(account_id, nonce, delta_payload)
    }

    fn validate_account_id(&self, account_id: &str) -> Result<(), String> {
        self.miden_client.validate_account_id(account_id)
    }

    fn validate_credential(
        &self,
        _state_json: &serde_json::Value,
        _credential: &crate::metadata::auth::Credentials,
        _auth: &crate::metadata::auth::Auth,
    ) -> Result<(), String> {
        // For integration tests, skip actual validation since test keys won't match account fixture
        Ok(())
    }

    fn validate_guardian_commitment(
        &self,
        _state_json: &serde_json::Value,
        _expected_guardian_commitment: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn should_update_auth(
        &mut self,
        state_json: &serde_json::Value,
        current_auth: &Auth,
    ) -> Result<Option<Auth>, String> {
        self.miden_client
            .should_update_auth(state_json, current_auth)
            .await
    }
}

pub async fn create_test_app_state() -> AppState {
    let storage_dir =
        std::env::temp_dir().join(format!("guardian_test_storage_{}", uuid::Uuid::new_v4()));
    let metadata_dir =
        std::env::temp_dir().join(format!("guardian_test_metadata_{}", uuid::Uuid::new_v4()));
    let keystore_dir =
        std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));

    std::fs::create_dir_all(&storage_dir).expect("Failed to create storage directory");
    std::fs::create_dir_all(&metadata_dir).expect("Failed to create metadata directory");
    std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");

    let storage = FilesystemService::new(storage_dir)
        .await
        .expect("Failed to create storage");
    let metadata = FilesystemMetadataStore::new(metadata_dir)
        .await
        .expect("Failed to create metadata");

    let storage_backend: Arc<dyn StorageBackend> = Arc::new(storage);

    let mock_client = MockNetworkClient::new();
    let ack = AckRegistry::new(keystore_dir)
        .await
        .expect("Failed to create signer registry");

    AppState {
        storage: storage_backend,
        metadata: Arc::new(metadata),
        network_client: Arc::new(tokio::sync::Mutex::new(mock_client)),
        ack,
        canonicalization: Some(crate::canonicalization::CanonicalizationConfig::default()),
        clock: Arc::new(crate::clock::SystemClock),
    }
}

pub fn create_grpc_service(state: AppState) -> GuardianService {
    GuardianService { app_state: state }
}

pub fn create_request_with_auth<T>(
    payload: T,
    pubkey: &str,
    sig: &str,
    timestamp: i64,
) -> Request<T> {
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
    metadata.insert(
        "x-timestamp",
        MetadataValue::try_from(timestamp.to_string()).expect("Valid timestamp metadata"),
    );

    request
}

pub fn create_signed_request_with_auth<T: Message>(
    payload: T,
    account_id_hex: &str,
    signer: &TestSigner,
) -> Request<T> {
    let request_payload = AuthRequestPayload::from_protobuf_message(&payload);
    let (sig, timestamp) = signer.sign_request(account_id_hex, &request_payload);
    create_request_with_auth(payload, &signer.pubkey_hex, &sig, timestamp)
}

pub fn create_miden_falcon_rpo_auth(cosigner_commitments: Vec<String>) -> AuthConfig {
    AuthConfig {
        auth_type: Some(auth_config::AuthType::MidenFalconRpo(MidenFalconRpoAuth {
            cosigner_commitments,
        })),
    }
}

pub fn create_router(state: AppState) -> axum::Router {
    use crate::api::http;

    axum::Router::new()
        .route("/configure", axum::routing::post(http::configure))
        .route("/push_delta", axum::routing::post(http::push_delta))
        .route("/get_delta", axum::routing::get(http::get_delta))
        .route("/get_state", axum::routing::get(http::get_state))
        .route("/pubkey", axum::routing::get(http::get_pubkey))
        .route(
            "/push_delta_proposal",
            axum::routing::post(http::push_delta_proposal),
        )
        .route(
            "/get_delta_proposals",
            axum::routing::get(http::get_delta_proposals),
        )
        .route(
            "/get_delta_proposal",
            axum::routing::get(http::get_delta_proposal),
        )
        .route(
            "/sign_delta_proposal",
            axum::routing::post(http::sign_delta_proposal),
        )
        .with_state(state)
}

pub fn load_fixture_account() -> (AccountId, String, serde_json::Value) {
    let fixture_json: serde_json::Value =
        serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
            .expect("Failed to parse fixture JSON");

    let account_id_hex = fixture_json["account_id"]
        .as_str()
        .expect("No account_id in fixture")
        .to_string();

    let account_id = AccountId::from_hex(&account_id_hex).expect("Invalid account ID in fixture");

    (account_id, account_id_hex, fixture_json)
}

pub fn load_fixture_account_grpc() -> (AccountId, String, String) {
    let (account_id, account_id_hex, fixture_json) = load_fixture_account();
    let fixture_string =
        serde_json::to_string(&fixture_json).expect("Failed to serialize fixture JSON");
    (account_id, account_id_hex, fixture_string)
}

pub fn get_test_account_id() -> (AccountId, String) {
    let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
    let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");
    (account_id, account_id_hex.to_string())
}

pub fn load_fixture_delta(delta_num: u8) -> serde_json::Value {
    let fixture_contents = match delta_num {
        1 => crate::testing::fixtures::DELTA_1_JSON,
        2 => crate::testing::fixtures::DELTA_2_JSON,
        3 => crate::testing::fixtures::DELTA_3_JSON,
        _ => panic!("Invalid delta number: {}", delta_num),
    };

    serde_json::from_str(fixture_contents).expect("Failed to parse delta fixture")
}

// load_fixture_delta_old removed - use load_fixture_delta(1) instead

pub fn create_test_delta_payload(account_id_hex: &str) -> serde_json::Value {
    let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");

    let delta = AccountDelta::new(
        account_id,
        AccountStorageDelta::default(),
        AccountVaultDelta::default(),
        Felt::ZERO,
    )
    .expect("Valid empty delta");

    // Wrap the AccountDelta in a TransactionSummary
    let tx_summary = TransactionSummary::new(
        delta,
        InputNotes::new(Vec::new()).unwrap(),
        RawOutputNotes::new(Vec::new()).unwrap(),
        Word::from([ZERO; 4]), // Salt
    );

    tx_summary.to_json()
}

/// A test signer that can be reused to sign multiple messages with the same keypair
/// Tracks the last used timestamp to prevent replay attack detection in tests
pub struct TestSigner {
    secret_key: SecretKey,
    pub pubkey_hex: String,
    pub commitment_hex: String,
    last_timestamp: std::cell::Cell<i64>,
}

impl Default for TestSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl TestSigner {
    pub fn new() -> Self {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let pubkey_hex = public_key.into_hex();
        Self {
            secret_key,
            pubkey_hex,
            commitment_hex,
            last_timestamp: std::cell::Cell::new(0),
        }
    }

    /// Sign an account ID with an auto-incrementing timestamp
    /// Ensures each call returns a timestamp greater than the previous one
    /// Returns (signature_hex, timestamp_ms)
    pub fn sign(&self, account_id_hex: &str) -> (String, i64) {
        let current = Utc::now().timestamp_millis();
        let last = self.last_timestamp.get();
        let timestamp = if current <= last { last + 1 } else { current };
        self.last_timestamp.set(timestamp);
        self.sign_with_timestamp(account_id_hex, timestamp)
    }

    /// Sign an account ID and request payload with an auto-incrementing timestamp.
    /// Ensures each call returns a timestamp greater than the previous one.
    /// Returns (signature_hex, timestamp_ms)
    pub fn sign_request(
        &self,
        account_id_hex: &str,
        request_payload: &AuthRequestPayload,
    ) -> (String, i64) {
        let current = Utc::now().timestamp_millis();
        let last = self.last_timestamp.get();
        let timestamp = if current <= last { last + 1 } else { current };
        self.last_timestamp.set(timestamp);
        self.sign_with_timestamp_and_request(account_id_hex, timestamp, request_payload)
    }

    pub fn sign_json_payload<T: serde::Serialize>(
        &self,
        account_id_hex: &str,
        request_payload: &T,
    ) -> (String, i64) {
        let request_payload = AuthRequestPayload::from_json_serializable(request_payload)
            .expect("Valid JSON payload");
        self.sign_request(account_id_hex, &request_payload)
    }

    /// Sign an account ID with a specific timestamp
    /// Returns (signature_hex, timestamp)
    pub fn sign_with_timestamp(&self, account_id_hex: &str, timestamp: i64) -> (String, i64) {
        self.sign_with_timestamp_and_request(
            account_id_hex,
            timestamp,
            &AuthRequestPayload::empty(),
        )
    }

    /// Sign an account ID and request payload with a specific timestamp.
    /// Returns (signature_hex, timestamp)
    pub fn sign_with_timestamp_and_request(
        &self,
        account_id_hex: &str,
        timestamp: i64,
        request_payload: &AuthRequestPayload,
    ) -> (String, i64) {
        let message = AuthRequestMessage::from_account_id_hex(
            account_id_hex,
            timestamp,
            request_payload.clone(),
        )
        .expect("Valid account ID")
        .to_word();

        let signature = self.secret_key.sign(message);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        (signature_hex, timestamp)
    }
}

pub struct TestEcdsaSigner {
    secret_key: EcdsaSecretKey,
    pub pubkey_hex: String,
    pub commitment_hex: String,
    last_timestamp: std::cell::Cell<i64>,
}

impl Default for TestEcdsaSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl TestEcdsaSigner {
    pub fn new() -> Self {
        let secret_key = EcdsaSecretKey::new();
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let pubkey_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        Self {
            secret_key,
            pubkey_hex,
            commitment_hex,
            last_timestamp: std::cell::Cell::new(0),
        }
    }

    pub fn sign_request(
        &self,
        account_id_hex: &str,
        request_payload: &AuthRequestPayload,
    ) -> (String, i64) {
        let current = Utc::now().timestamp_millis();
        let last = self.last_timestamp.get();
        let timestamp = if current <= last { last + 1 } else { current };
        self.last_timestamp.set(timestamp);
        self.sign_with_timestamp_and_request(account_id_hex, timestamp, request_payload)
    }

    pub fn sign_json_payload<T: serde::Serialize>(
        &self,
        account_id_hex: &str,
        request_payload: &T,
    ) -> (String, i64) {
        let request_payload = AuthRequestPayload::from_json_serializable(request_payload)
            .expect("Valid JSON payload");
        self.sign_request(account_id_hex, &request_payload)
    }

    pub fn sign_with_timestamp(&self, account_id_hex: &str, timestamp: i64) -> (String, i64) {
        self.sign_with_timestamp_and_request(
            account_id_hex,
            timestamp,
            &AuthRequestPayload::empty(),
        )
    }

    pub fn sign_with_timestamp_and_request(
        &self,
        account_id_hex: &str,
        timestamp: i64,
        request_payload: &AuthRequestPayload,
    ) -> (String, i64) {
        let message = AuthRequestMessage::from_account_id_hex(
            account_id_hex,
            timestamp,
            request_payload.clone(),
        )
        .expect("Valid account ID")
        .to_word();

        let signature = self.secret_key.sign(message);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        (signature_hex, timestamp)
    }
}

/// Generates a Falcon signature for replay-resistant authentication.
/// Returns (pubkey_hex, commitment_hex, signature_hex, timestamp)
pub fn generate_falcon_signature_with_timestamp(
    account_id_hex: &str,
    timestamp: i64,
) -> (String, String, String, i64) {
    let signer = TestSigner::new();
    let (signature_hex, timestamp) = signer.sign_with_timestamp(account_id_hex, timestamp);
    (
        signer.pubkey_hex,
        signer.commitment_hex,
        signature_hex,
        timestamp,
    )
}

/// Convenience function that generates a signature with current timestamp (milliseconds)
pub fn generate_falcon_signature(account_id_hex: &str) -> (String, String, String, i64) {
    let timestamp = chrono::Utc::now().timestamp_millis();
    generate_falcon_signature_with_timestamp(account_id_hex, timestamp)
}

/// Generates an ECDSA signature for replay-resistant authentication.
/// Returns (pubkey_hex, commitment_hex, signature_hex, timestamp)
pub fn generate_ecdsa_signature_with_timestamp(
    account_id_hex: &str,
    timestamp: i64,
) -> (String, String, String, i64) {
    let signer = TestEcdsaSigner::new();
    let (signature_hex, timestamp) = signer.sign_with_timestamp(account_id_hex, timestamp);
    (
        signer.pubkey_hex,
        signer.commitment_hex,
        signature_hex,
        timestamp,
    )
}

/// Convenience function that generates an ECDSA signature with current timestamp (milliseconds)
pub fn generate_ecdsa_signature(account_id_hex: &str) -> (String, String, String, i64) {
    let timestamp = chrono::Utc::now().timestamp_millis();
    generate_ecdsa_signature_with_timestamp(account_id_hex, timestamp)
}

pub fn pubkey_hex_to_commitment_hex(pubkey_hex: &str) -> String {
    use miden_protocol::crypto::dsa::falcon512_poseidon2::PublicKey;
    use miden_protocol::utils::serde::{Deserializable, Serializable};

    let pubkey_hex = pubkey_hex.strip_prefix("0x").unwrap_or(pubkey_hex);
    let pubkey_bytes = hex::decode(pubkey_hex).expect("Valid public key hex");
    let pubkey = PublicKey::read_from_bytes(&pubkey_bytes).expect("Valid public key");
    let commitment = pubkey.to_commitment();
    format!("0x{}", hex::encode(commitment.to_bytes()))
}

pub async fn update_mock_on_chain_commitment(
    state: &AppState,
    account_id: String,
    commitment: String,
) {
    let _ = state;
    let _ = account_id;
    let _ = commitment;
}

pub fn create_test_app_state_with_mocks(
    storage: Arc<dyn StorageBackend>,
    network_client: Arc<tokio::sync::Mutex<dyn NetworkClient>>,
    metadata: Arc<dyn crate::metadata::MetadataStore>,
) -> AppState {
    let keystore_dir =
        std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");

    let storage_backend: Arc<dyn StorageBackend> = storage;

    let ack = futures::executor::block_on(AckRegistry::new(keystore_dir))
        .expect("Failed to create signer registry");

    AppState {
        storage: storage_backend,
        metadata,
        network_client,
        ack,
        canonicalization: None, // Use optimistic mode for unit tests
        clock: Arc::new(crate::clock::SystemClock),
    }
}
