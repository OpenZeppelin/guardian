//! Builder pattern for constructing MultisigClient instances.

use std::path::PathBuf;
use std::sync::Arc;

use crate::prover::RetryingTransactionProver;
use miden_client::DebugMode;
use miden_client::RemoteTransactionProver;
use miden_client::builder::{ClientBuilder, DEVNET_PROVER_ENDPOINT, TESTNET_PROVER_ENDPOINT};
use miden_client::keystore::FilesystemKeyStore;
use miden_client::rpc::Endpoint;
use miden_client_sqlite_store::SqliteStore;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::crypto::rand::RandomCoin;

use crate::MidenSdkClient;
use crate::client::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::keystore::{EcdsaGuardianKeyStore, GuardianKeyStore, KeyManager};

fn configured_client_builder(endpoint: &Endpoint) -> ClientBuilder<FilesystemKeyStore> {
    if endpoint == &Endpoint::devnet() {
        with_prover_retries(
            ClientBuilder::<FilesystemKeyStore>::for_devnet(),
            DEVNET_PROVER_ENDPOINT,
        )
    } else if endpoint == &Endpoint::testnet() {
        with_prover_retries(
            ClientBuilder::<FilesystemKeyStore>::for_testnet(),
            TESTNET_PROVER_ENDPOINT,
        )
    } else if endpoint == &Endpoint::localhost() {
        ClientBuilder::<FilesystemKeyStore>::for_localhost()
    } else {
        ClientBuilder::<FilesystemKeyStore>::new().grpc_client(endpoint, Some(20_000))
    }
}

/// Wrap the preset's prover so transient failures are retried.
///
/// The devnet and testnet presets delegate proving to a shared remote prover
/// that cancels requests under load; localhost proves locally and has nothing to
/// retry. Retrying is safe at this layer because the delta reaches GUARDIAN
/// before proving starts, so another proof attempt changes no server state --
/// unlike retrying the surrounding execution, which re-pushes the delta and is
/// refused as a pending-delta conflict.
fn with_prover_retries(
    builder: ClientBuilder<FilesystemKeyStore>,
    prover_endpoint: &str,
) -> ClientBuilder<FilesystemKeyStore> {
    // The builder exposes no getter for the preset's prover, so the same remote
    // prover is reconstructed from the endpoint the preset uses and wrapped.
    builder.prover(Arc::new(RetryingTransactionProver::new(Arc::new(
        RemoteTransactionProver::new(prover_endpoint.to_string()),
    ))))
}

/// Builder for constructing MultisigClient instances.
///
/// # Example
///
/// ```ignore
/// use miden_multisig_client::MultisigClient;
/// use miden_client::rpc::Endpoint;
///
/// let client = MultisigClient::builder()
///     .miden_endpoint(Endpoint::new("http://localhost:57291"))
///     .guardian_endpoint("http://localhost:50051")
///     .account_dir("/tmp/multisig-client")
///     .generate_key()
///     .build()
///     .await?;
/// ```
pub struct MultisigClientBuilder {
    miden_endpoint: Option<Endpoint>,
    guardian_endpoint: Option<String>,
    account_dir: Option<PathBuf>,
    key_manager: Option<Arc<dyn KeyManager>>,
}

impl Default for MultisigClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MultisigClientBuilder {
    /// Creates a new builder with default settings.
    pub fn new() -> Self {
        Self {
            miden_endpoint: None,
            guardian_endpoint: None,
            account_dir: None,
            key_manager: None,
        }
    }

    /// Sets the Miden node RPC endpoint.
    pub fn miden_endpoint(mut self, endpoint: Endpoint) -> Self {
        self.miden_endpoint = Some(endpoint);
        self
    }

    /// Sets the GUARDIAN server endpoint.
    pub fn guardian_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.guardian_endpoint = Some(endpoint.into());
        self
    }

    /// Sets the account directory for miden-client storage.
    ///
    /// This directory will contain the SQLite database for account and transaction data.
    pub fn account_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.account_dir = Some(path.into());
        self
    }

    /// Sets a custom key manager for GUARDIAN authentication and proposal signing.
    pub fn key_manager(mut self, key_manager: Box<dyn KeyManager>) -> Self {
        self.key_manager = Some(key_manager.into());
        self
    }

    /// Uses a FalconKeyStore with the given secret key.
    pub fn with_secret_key(mut self, secret_key: SecretKey) -> Self {
        self.key_manager = Some(Arc::new(GuardianKeyStore::new(secret_key)));
        self
    }

    /// Uses an ECDSA key store with the given secret key.
    pub fn with_ecdsa_secret_key(mut self, secret_key: EcdsaSecretKey) -> Self {
        self.key_manager = Some(Arc::new(EcdsaGuardianKeyStore::new(secret_key)));
        self
    }

    /// Generates a new random key for GUARDIAN authentication.
    pub fn generate_key(mut self) -> Self {
        self.key_manager = Some(Arc::new(GuardianKeyStore::generate()));
        self
    }

    /// Generates a new random ECDSA key for GUARDIAN authentication.
    pub fn generate_ecdsa_key(mut self) -> Self {
        self.key_manager = Some(Arc::new(EcdsaGuardianKeyStore::generate()));
        self
    }

    /// Builds the MultisigClient.
    pub async fn build(self) -> Result<MultisigClient> {
        let miden_endpoint = self
            .miden_endpoint
            .ok_or_else(|| MultisigError::MissingConfig("miden_endpoint".to_string()))?;

        let guardian_endpoint = self
            .guardian_endpoint
            .ok_or_else(|| MultisigError::MissingConfig("guardian_endpoint".to_string()))?;

        let account_dir = self
            .account_dir
            .ok_or_else(|| MultisigError::MissingConfig("account_dir".to_string()))?;

        let key_manager = self.key_manager.ok_or(MultisigError::NoSigner)?;

        // Ensure account directory exists
        std::fs::create_dir_all(&account_dir).map_err(|e| {
            MultisigError::MidenClient(format!("failed to create account dir: {}", e))
        })?;

        let miden_client = create_miden_client(&account_dir, &miden_endpoint).await?;

        Ok(MultisigClient::new(
            miden_client,
            key_manager,
            guardian_endpoint,
            account_dir,
            miden_endpoint,
        ))
    }
}

/// Creates a miden-client instance with SQLite storage.
///
/// Each call creates a fresh database with a unique filename to ensure
/// no accumulated state from previous sessions.
pub(crate) async fn create_miden_client(
    account_dir: &std::path::Path,
    endpoint: &Endpoint,
) -> Result<MidenSdkClient> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let random_suffix: u32 = rand::random();
    let store_path = account_dir.join(format!(
        "miden-client-{}-{}.sqlite",
        timestamp, random_suffix
    ));
    let store = SqliteStore::new(store_path)
        .await
        .map_err(|e| MultisigError::MidenClient(format!("failed to open SQLite store: {}", e)))?;
    let store = Arc::new(store);

    let rng_seed: [u32; 4] = rand::random();
    let rng = Box::new(RandomCoin::new(rng_seed.into()));

    configured_client_builder(endpoint)
        .store(store)
        .rng(rng)
        .in_debug_mode(DebugMode::Enabled)
        .tx_discard_delta(Some(20))
        .max_block_number_delta(256)
        .build()
        .await
        .map_err(|e| MultisigError::MidenClient(format!("failed to create miden client: {}", e)))
}
