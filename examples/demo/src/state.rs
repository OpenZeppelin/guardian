use std::sync::Arc;

use miden_client::rpc::Endpoint;
use miden_multisig_client::{ExportedProposal, MultisigClient};
use tempfile::TempDir;

/// Simplified session state using the MultisigClient SDK.
pub struct SessionState {
    pub client: Option<MultisigClient>,
    pub account_directory: Arc<TempDir>,
    /// Imported proposal for offline workflow.
    pub imported_proposal: Option<ExportedProposal>,
}

impl SessionState {
    pub fn new() -> Result<Self, String> {
        let account_directory =
            TempDir::new().map_err(|e| format!("Failed to create account directory: {}", e))?;

        Ok(SessionState {
            client: None,
            account_directory: Arc::new(account_directory),
            imported_proposal: None,
        })
    }

    /// Initializes the MultisigClient with the given endpoints.
    pub async fn initialize_client(
        &mut self,
        miden_endpoint: Endpoint,
        psm_endpoint: &str,
    ) -> Result<(), String> {
        let account_dir = self.account_directory.path().to_path_buf();

        let client = MultisigClient::builder()
            .miden_endpoint(miden_endpoint)
            .psm_endpoint(psm_endpoint)
            .account_dir(account_dir)
            .generate_key()
            .build()
            .await
            .map_err(|e| format!("Failed to create multisig client: {}", e))?;

        self.client = Some(client);
        Ok(())
    }

    pub fn has_account(&self) -> bool {
        self.client
            .as_ref()
            .map(|c| c.has_account())
            .unwrap_or(false)
    }

    pub fn get_client(&self) -> Result<&MultisigClient, String> {
        self.client
            .as_ref()
            .ok_or_else(|| "Client not initialized".to_string())
    }

    pub fn get_client_mut(&mut self) -> Result<&mut MultisigClient, String> {
        self.client
            .as_mut()
            .ok_or_else(|| "Client not initialized".to_string())
    }

    pub fn user_commitment_hex(&self) -> Result<String, String> {
        self.get_client().map(|c| c.user_commitment_hex())
    }

    /// Sets the imported proposal.
    pub fn set_imported_proposal(&mut self, proposal: ExportedProposal) {
        self.imported_proposal = Some(proposal);
    }

    /// Gets a reference to the imported proposal.
    pub fn get_imported_proposal(&self) -> Option<&ExportedProposal> {
        self.imported_proposal.as_ref()
    }

    /// Takes ownership of the imported proposal.
    pub fn take_imported_proposal(&mut self) -> Option<ExportedProposal> {
        self.imported_proposal.take()
    }
}
