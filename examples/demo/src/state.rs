use std::sync::Arc;

use miden_client::rpc::Endpoint;
use miden_multisig_client::{ExportedProposal, MultisigClient, SignatureScheme};
use tempfile::TempDir;

/// Simplified session state using the MultisigClient SDK.
pub struct SessionState {
    pub client: Option<MultisigClient>,
    pub account_directory: Arc<TempDir>,
    /// Imported proposal for offline workflow.
    pub imported_proposal: Option<ExportedProposal>,
    /// Signature scheme used by this demo session.
    signature_scheme: SignatureScheme,
    /// Stored endpoints for reinitialization.
    miden_endpoint: Option<Endpoint>,
    psm_endpoint: Option<String>,
}

impl SessionState {
    pub fn new() -> Result<Self, String> {
        let account_directory =
            TempDir::new().map_err(|e| format!("Failed to create account directory: {}", e))?;

        Ok(SessionState {
            client: None,
            account_directory: Arc::new(account_directory),
            imported_proposal: None,
            signature_scheme: SignatureScheme::Falcon,
            miden_endpoint: None,
            psm_endpoint: None,
        })
    }

    /// Initializes the MultisigClient with the given endpoints.
    pub async fn initialize_client(
        &mut self,
        miden_endpoint: Endpoint,
        psm_endpoint: &str,
        signature_scheme: SignatureScheme,
    ) -> Result<(), String> {
        // Store endpoints for potential reinitialization
        self.miden_endpoint = Some(miden_endpoint.clone());
        self.psm_endpoint = Some(psm_endpoint.to_string());
        self.signature_scheme = signature_scheme;

        let account_dir = self.account_directory.path().to_path_buf();

        let builder = MultisigClient::builder()
            .miden_endpoint(miden_endpoint)
            .psm_endpoint(psm_endpoint)
            .account_dir(account_dir);

        let mut client = match self.signature_scheme {
            SignatureScheme::Falcon => builder.generate_key(),
            SignatureScheme::Ecdsa => builder.generate_ecdsa_key(),
        }
        .build()
        .await
        .map_err(|e| format!("Failed to create multisig client: {}", e))?;

        client
            .reset_miden_client()
            .await
            .map_err(|e| format!("Failed to reset miden client: {}", e))?;

        self.client = Some(client);
        Ok(())
    }

    /// Reinitializes the MultisigClient with fresh database connections.
    ///
    /// This is useful when the connection pool gets poisoned due to panics.
    /// It preserves the key manager and account state but creates a new miden-client.
    pub async fn reinitialize_client(&mut self) -> Result<(), String> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| "Client not initialized".to_string())?;

        // Reset the miden client (creates a fresh SQLite connection)
        client
            .reset_miden_client()
            .await
            .map_err(|e| format!("Failed to reinitialize client: {}", e))?;

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

    pub fn signature_scheme_name(&self) -> &'static str {
        match self.signature_scheme {
            SignatureScheme::Falcon => "Falcon",
            SignatureScheme::Ecdsa => "ECDSA",
        }
    }

    pub fn is_ecdsa(&self) -> bool {
        matches!(self.signature_scheme, SignatureScheme::Ecdsa)
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
