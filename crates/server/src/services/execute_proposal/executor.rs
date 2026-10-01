//! The network-specific half of a Guardian execution, behind a trait so the lifecycle in this
//! module stays network-agnostic. The Miden implementation lives in
//! `network::miden::execution`.

use async_trait::async_trait;
use guardian_shared::SignatureScheme;

use crate::delta_object::CosignerSignature;
use crate::error::GuardianError;
use crate::storage::ExecutionFailure;

/// What an execution attempt starts from, read under the reservation.
#[derive(Debug, Clone)]
pub struct ExecutionInput {
    pub account_id: String,
    pub state_json: serde_json::Value,
    pub proposal_payload: serde_json::Value,
    pub cosigner_signatures: Vec<CosignerSignature>,
}

/// The distinct, valid, currently registered cosigner signatures of a proposal, counted
/// against the threshold that governs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureSelection {
    pub required: usize,
    pub valid: usize,
    pub ignored: u32,
}

impl SignatureSelection {
    pub fn is_ready(&self) -> bool {
        self.valid >= self.required
    }
}

/// Guardian's acknowledgment of the reproduced summary, ready to become execution advice.
#[derive(Debug, Clone)]
pub struct GuardianAck {
    pub scheme: SignatureScheme,
    pub signature_hex: String,
    pub public_key_hex: String,
    pub commitment_hex: String,
}

/// What execution produced, for the service to re-check against its own acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedTransactionInfo {
    pub final_account_commitment: String,
}

/// The proven transaction's facts the boundary commit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenTransactionInfo {
    pub transaction_id: String,
    pub reference_block: u32,
    pub expiration_block: u32,
}

/// What the node said about a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Accepted,
    /// An explicit application-level rejection: the transaction will not land.
    Rejected {
        reason: String,
    },
    /// No definite answer (timeout, dropped connection, unavailable): it may still land.
    Unknown {
        reason: String,
    },
}

#[async_trait]
pub trait ProposalExecutor: Send + Sync {
    /// Selects the proposal's valid cosigner signatures and the threshold they must meet.
    fn select_signatures(
        &self,
        input: &ExecutionInput,
    ) -> Result<SignatureSelection, GuardianError>;

    /// Decodes and checks the stored request, assembles the chain view at the tip, and
    /// reproduces the transaction, confirming it yields the signed summary.
    async fn prepare(
        &self,
        input: ExecutionInput,
    ) -> Result<Box<dyn ExecutionAttempt>, ExecutionFailure>;

    /// The chain's committed tip, which reconciliation compares with a submission's recorded
    /// expiration.
    async fn chain_tip(&self) -> Result<u32, String>;
}

#[async_trait]
pub trait ExecutionAttempt: Send + Sync {
    fn reference_block(&self) -> u32;

    /// The reproduced summary as the delta payload Guardian acknowledges.
    fn summary_payload(&self) -> &serde_json::Value;

    /// Whether the transaction's authorization needs Guardian's acknowledgment.
    fn requires_guardian_ack(&self) -> bool;

    /// Executes the authorized transaction with the selected signatures and the
    /// acknowledgment, re-verifies the binding, and refuses a reached expiration.
    async fn execute(
        &mut self,
        ack: Option<GuardianAck>,
    ) -> Result<ExecutedTransactionInfo, ExecutionFailure>;

    /// Proves the executed transaction, retrying transient prover failures while the
    /// transaction's expiration can still be met.
    async fn prove(&mut self) -> Result<ProvenTransactionInfo, ExecutionFailure>;

    /// Seals the transaction inputs for submission.
    async fn seal(&mut self) -> Result<(), ExecutionFailure>;

    /// Sends the proven transaction once.
    async fn submit(&mut self) -> SubmissionOutcome;
}
