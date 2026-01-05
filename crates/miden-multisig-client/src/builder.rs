//! Builder pattern for constructing MultisigClient instances.

use std::path::PathBuf;
use std::sync::Arc;

use miden_client::crypto::RpoRandomCoin;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::{Client, ExecutionOptions};
use miden_client_sqlite_store::SqliteStore;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use miden_objects::{MAX_TX_EXECUTION_CYCLES, MIN_TX_EXECUTION_CYCLES};

use crate::client::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::keystore::{KeyManager, PsmKeyStore};

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
///     .psm_endpoint("http://localhost:50051")
///     .account_dir("/tmp/multisig-client")
///     .generate_key()
///     .build()
///     .await?;
/// ```
pub struct MultisigClientBuilder {
    miden_endpoint: Option<Endpoint>,
    psm_endpoint: Option<String>,
    account_dir: Option<PathBuf>,
    key_manager: Option<Box<dyn KeyManager>>,
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
            psm_endpoint: None,
            account_dir: None,
            key_manager: None,
        }
    }

    /// Sets the Miden node RPC endpoint.
    pub fn miden_endpoint(mut self, endpoint: Endpoint) -> Self {
        self.miden_endpoint = Some(endpoint);
        self
    }

    /// Sets the PSM server endpoint.
    pub fn psm_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.psm_endpoint = Some(endpoint.into());
        self
    }

    /// Sets the account directory for miden-client storage.
    ///
    /// This directory will contain the SQLite database for account and transaction data.
    pub fn account_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.account_dir = Some(path.into());
        self
    }

    /// Sets a custom key manager for PSM authentication.
    pub fn key_manager(mut self, key_manager: impl KeyManager + 'static) -> Self {
        self.key_manager = Some(Box::new(key_manager));
        self
    }

    /// Uses a PsmKeyStore with the given secret key.
    pub fn with_secret_key(mut self, secret_key: SecretKey) -> Self {
        self.key_manager = Some(Box::new(PsmKeyStore::new(secret_key)));
        self
    }

    /// Generates a new random key for PSM authentication.
    pub fn generate_key(mut self) -> Self {
        self.key_manager = Some(Box::new(PsmKeyStore::generate()));
        self
    }

    /// Builds the MultisigClient.
    pub async fn build(self) -> Result<MultisigClient> {
        let miden_endpoint = self
            .miden_endpoint
            .ok_or_else(|| MultisigError::MissingConfig("miden_endpoint".to_string()))?;

        let psm_endpoint = self
            .psm_endpoint
            .ok_or_else(|| MultisigError::MissingConfig("psm_endpoint".to_string()))?;

        let account_dir = self
            .account_dir
            .ok_or_else(|| MultisigError::MissingConfig("account_dir".to_string()))?;

        let key_manager = self.key_manager.ok_or(MultisigError::NoKeyManager)?;

        // Ensure account directory exists
        std::fs::create_dir_all(&account_dir).map_err(|e| {
            MultisigError::MidenClient(format!("failed to create account dir: {}", e))
        })?;

        let miden_client = create_miden_client(&account_dir, &miden_endpoint).await?;

        Ok(MultisigClient::new(
            miden_client,
            key_manager,
            psm_endpoint,
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
) -> Result<Client<()>> {
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
    let rng = Box::new(RpoRandomCoin::new(rng_seed.into()));
    let exec_options = ExecutionOptions::new(
        Some(MAX_TX_EXECUTION_CYCLES),
        MIN_TX_EXECUTION_CYCLES,
        true,
        true,
    )
    .map_err(|e| MultisigError::MidenClient(format!("failed to build execution options: {}", e)))?;

    let grpc_client = GrpcClient::new(endpoint, 20_000);
    let rpc_client: Arc<dyn NodeRpcClient> = Arc::new(grpc_client);

    Client::new(
        rpc_client,
        rng,
        store,
        None,
        exec_options,
        Some(20),
        Some(256),
        None,
        None,
    )
    .await
    .map_err(|e| MultisigError::MidenClient(format!("failed to create miden client: {}", e)))
}
