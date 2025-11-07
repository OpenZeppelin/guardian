use std::path::PathBuf;
use std::sync::Arc;

use miden_client::account::{Account, AccountId};
use miden_client::keystore::FilesystemKeyStore;
use miden_client::rpc::Endpoint;
use miden_client::Client;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use tempfile::TempDir;

use private_state_manager_client::{Auth, FalconRpoSigner, PsmClient};

use crate::helpers::create_miden_client;

pub struct SessionState {
    pub psm_endpoint: String,
    pub miden_endpoint: Endpoint,
    pub psm_client: Option<PsmClient>,
    pub miden_client: Option<Client<()>>,
    pub account: Option<Account>,
    pub account_id: Option<AccountId>,
    pub user_secret_key: Option<SecretKey>,
    pub user_commitment_hex: Option<String>,
    pub cosigner_commitments: Vec<String>,
    pub keystore: Arc<FilesystemKeyStore<StdRng>>,
    pub temp_dir: Arc<TempDir>,
}

impl SessionState {
    pub fn new(psm_endpoint: String, miden_endpoint: Endpoint) -> Result<Self, String> {
        let temp_dir =
            TempDir::new().map_err(|e| format!("Failed to create temp directory: {}", e))?;

        let keystore_path = temp_dir.path().join("keystore");
        let keystore = FilesystemKeyStore::new(keystore_path)
            .map_err(|e| format!("Failed to create keystore: {}", e))?;

        Ok(SessionState {
            psm_endpoint,
            miden_endpoint,
            psm_client: None,
            miden_client: None,
            account: None,
            account_id: None,
            user_secret_key: None,
            user_commitment_hex: None,
            cosigner_commitments: Vec::new(),
            keystore: Arc::new(keystore),
            temp_dir: Arc::new(temp_dir),
        })
    }

    pub async fn connect_psm(&mut self) -> Result<(), String> {
        let client = PsmClient::connect(&self.psm_endpoint)
            .await
            .map_err(|e| format!("Failed to connect to PSM: {}", e))?;

        self.psm_client = Some(client);
        Ok(())
    }

    pub async fn connect_miden(&mut self) -> Result<(), String> {
        let data_dir = self.temp_dir.path().to_path_buf();
        let client = create_miden_client(&data_dir, &self.miden_endpoint).await?;

        self.miden_client = Some(client);
        Ok(())
    }

    pub fn is_psm_connected(&self) -> bool {
        self.psm_client.is_some()
    }

    pub fn is_miden_connected(&self) -> bool {
        self.miden_client.is_some()
    }

    pub fn has_account(&self) -> bool {
        self.account.is_some()
    }

    pub fn has_keypair(&self) -> bool {
        self.user_secret_key.is_some()
    }

    #[allow(dead_code)]
    pub fn get_psm_client(&self) -> Result<&PsmClient, String> {
        self.psm_client
            .as_ref()
            .ok_or_else(|| "PSM client not connected".to_string())
    }

    pub fn get_psm_client_mut(&mut self) -> Result<&mut PsmClient, String> {
        self.psm_client
            .as_mut()
            .ok_or_else(|| "PSM client not connected".to_string())
    }

    pub fn configure_psm_auth(&mut self) -> Result<(), String> {
        let secret_key = self.get_secret_key()?.clone();
        let signer = FalconRpoSigner::new(secret_key);
        let auth = Auth::FalconRpoSigner(signer);

        let client = self
            .psm_client
            .take()
            .ok_or_else(|| "PSM client not connected".to_string())?;

        self.psm_client = Some(client.with_auth(auth));
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_miden_client(&self) -> Result<&Client<()>, String> {
        self.miden_client
            .as_ref()
            .ok_or_else(|| "Miden client not connected".to_string())
    }

    pub fn get_miden_client_mut(&mut self) -> Result<&mut Client<()>, String> {
        self.miden_client
            .as_mut()
            .ok_or_else(|| "Miden client not connected".to_string())
    }

    pub fn get_account(&self) -> Result<&Account, String> {
        self.account
            .as_ref()
            .ok_or_else(|| "No account loaded".to_string())
    }

    pub fn get_account_mut(&mut self) -> Result<&mut Account, String> {
        self.account
            .as_mut()
            .ok_or_else(|| "No account loaded".to_string())
    }

    pub fn get_account_id(&self) -> Result<AccountId, String> {
        self.account_id
            .ok_or_else(|| "No account ID set".to_string())
    }

    pub fn get_secret_key(&self) -> Result<&SecretKey, String> {
        self.user_secret_key
            .as_ref()
            .ok_or_else(|| "No keypair generated".to_string())
    }

    pub fn get_commitment_hex(&self) -> Result<&str, String> {
        self.user_commitment_hex
            .as_ref()
            .map(|s| s.as_str())
            .ok_or_else(|| "No keypair generated".to_string())
    }

    pub fn set_account(&mut self, account: Account) {
        let account_id = account.id();
        self.account_id = Some(account_id);
        self.account = Some(account);
    }

    pub fn set_keypair(&mut self, commitment_hex: String, secret_key: SecretKey) {
        self.user_commitment_hex = Some(commitment_hex);
        self.user_secret_key = Some(secret_key);
    }

    #[allow(dead_code)]
    pub fn get_data_dir(&self) -> PathBuf {
        self.temp_dir.path().to_path_buf()
    }

    pub fn get_keystore(&self) -> Arc<FilesystemKeyStore<StdRng>> {
        Arc::clone(&self.keystore)
    }

    pub fn create_rng(&self) -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(rand::random())
    }
}
