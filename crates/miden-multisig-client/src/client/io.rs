//! Export/import operations for MultisigClient.
//!
//! This module handles exporting proposals to files/strings and
//! importing them back for offline sharing workflows.

use private_state_manager_client::delta_status::Status;

use super::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::export::{ExportedProposal, ExportedSignature};

impl MultisigClient {
    /// Exports a proposal to a file for offline sharing.
    ///
    /// This fetches the proposal from PSM, including all collected signatures,
    /// and writes it to the specified file path as JSON.
    ///
    /// # Example
    ///
    /// ```ignore
    /// client.export_proposal(&proposal_id, "/tmp/proposal.json").await?;
    /// ```
    pub async fn export_proposal(
        &mut self,
        proposal_id: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        let exported = self.export_proposal_to_exported(proposal_id).await?;
        let json = exported.to_json()?;
        std::fs::write(path, json)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to write file: {}", e)))?;
        Ok(())
    }

    /// Exports a proposal to a JSON string for programmatic use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let json = client.export_proposal_to_string(&proposal_id).await?;
    /// println!("{}", json);
    /// ```
    pub async fn export_proposal_to_string(&mut self, proposal_id: &str) -> Result<String> {
        let exported = self.export_proposal_to_exported(proposal_id).await?;
        exported.to_json()
    }

    /// Internal helper to create an ExportedProposal from PSM data.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The proposal is not found in the parsed proposals list
    /// - The raw delta cannot be found in PSM response
    /// - The delta has no pending status with signature data
    async fn export_proposal_to_exported(&mut self, proposal_id: &str) -> Result<ExportedProposal> {
        let account = self.require_account()?.clone();
        let account_id = account.id();

        // Get the proposal
        let proposals = self.list_proposals().await?;
        let proposal = proposals
            .iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        // Get raw delta to extract signatures
        let mut psm_client = self.create_authenticated_psm_client().await?;
        let proposals_response = psm_client
            .get_delta_proposals(&account_id)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get proposals: {}", e)))?;

        // Find the raw proposal - fail if not found
        let raw_proposal = proposals_response
            .proposals
            .iter()
            .find(|p| p.nonce == proposal.nonce)
            .ok_or_else(|| {
                MultisigError::ProposalNotFound(format!(
                    "raw delta not found for proposal {} (nonce {})",
                    proposal_id, proposal.nonce
                ))
            })?;

        // Extract signatures - fail if status structure is missing
        let status = raw_proposal.status.as_ref().ok_or_else(|| {
            MultisigError::PsmServer(format!("proposal {} has no status field", proposal_id))
        })?;

        let status_oneof = status.status.as_ref().ok_or_else(|| {
            MultisigError::PsmServer(format!("proposal {} has empty status", proposal_id))
        })?;

        let pending = match status_oneof {
            Status::Pending(p) => p,
            _ => {
                return Err(MultisigError::PsmServer(format!(
                    "proposal {} is not in pending state",
                    proposal_id
                )));
            }
        };

        let mut signatures = Vec::new();
        for cosigner_sig in pending.cosigner_sigs.iter() {
            if let Some(ref sig) = cosigner_sig.signature {
                signatures.push(ExportedSignature {
                    signer_commitment: cosigner_sig.signer_id.clone(),
                    signature: sig.signature.clone(),
                });
            }
        }

        let exported =
            ExportedProposal::from_proposal(proposal, account_id).with_signatures(signatures);

        Ok(exported)
    }

    /// Imports a proposal from a file.
    ///
    /// The proposal can then be signed with `sign_imported_proposal`
    /// or executed with `execute_imported_proposal`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal("/tmp/proposal.json")?;
    /// println!("Imported proposal: {}", proposal.id);
    /// ```
    pub fn import_proposal(&self, path: &std::path::Path) -> Result<ExportedProposal> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to read file: {}", e)))?;
        self.import_proposal_from_string(&json)
    }

    /// Imports a proposal from a JSON string.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal_from_string(&json)?;
    /// ```
    pub fn import_proposal_from_string(&self, json: &str) -> Result<ExportedProposal> {
        let exported = ExportedProposal::from_json(json)?;

        // Validate account ID matches if we have an account loaded
        if let Some(account) = &self.account {
            let expected_id = account.id().to_string();
            if !exported.account_id.eq_ignore_ascii_case(&expected_id) {
                return Err(MultisigError::InvalidConfig(format!(
                    "proposal account {} does not match loaded account {}",
                    exported.account_id, expected_id
                )));
            }
        }

        Ok(exported)
    }
}
