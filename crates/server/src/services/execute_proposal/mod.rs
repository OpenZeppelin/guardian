//! Guardian execution of threshold-met proposals.

mod executor;
mod service;
mod worker;

pub use executor::{
    ExecutedTransactionInfo, ExecutionAttempt, ExecutionInput, GuardianAck, ProposalExecutor,
    ProvenTransactionInfo, SignatureSelection, SubmissionOutcome,
};
pub use service::{ExecutionState, RequestExecutionParams, request_execution};

#[cfg(test)]
mod binding_tests;
#[cfg(test)]
mod capability_tests;
#[cfg(test)]
mod concurrency_tests;
#[cfg(test)]
mod fault_injection_tests;
#[cfg(test)]
pub(crate) mod tests;
