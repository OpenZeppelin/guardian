//! Requesting and observing Guardian execution of this client's proposals.

use guardian_client::{ClientError, ProposalExecution};

use super::MultisigClient;
use crate::error::{MultisigError, Result};

fn refusal(error: ClientError) -> MultisigError {
    match error.guardian_code() {
        Some(code) => MultisigError::GuardianExecutionRefused {
            message: error.user_message().unwrap_or_else(|| error.to_string()),
            code,
        },
        None => MultisigError::from(error),
    }
}

impl MultisigClient {
    /// Asks Guardian to prove and submit a threshold-met proposal. Returns once the request is
    /// accepted; [`execution_status`](Self::execution_status) reports the outcome. The proposal
    /// must have been created by a client in
    /// [`GuardianExecutable`](crate::ProposalExecutionMode::GuardianExecutable) mode.
    pub async fn request_guardian_execution(
        &mut self,
        proposal_id: &str,
    ) -> Result<ProposalExecution> {
        let account_id = self.require_account()?.id();
        let mut guardian = self.create_authenticated_guardian_client().await?;
        guardian
            .execute_delta_proposal(&account_id, proposal_id)
            .await
            .map_err(refusal)
    }

    /// The latest Guardian execution of a proposal.
    pub async fn execution_status(&mut self, proposal_id: &str) -> Result<ProposalExecution> {
        let account_id = self.require_account()?.id();
        let mut guardian = self.create_authenticated_guardian_client().await?;
        guardian
            .get_delta_proposal_execution(&account_id, proposal_id)
            .await
            .map_err(refusal)
    }

    /// The account's in-flight Guardian execution, if any.
    pub async fn current_execution(&mut self) -> Result<Option<ProposalExecution>> {
        let account_id = self.require_account()?.id();
        let mut guardian = self.create_authenticated_guardian_client().await?;
        guardian
            .get_current_execution(&account_id)
            .await
            .map_err(refusal)
    }
}
