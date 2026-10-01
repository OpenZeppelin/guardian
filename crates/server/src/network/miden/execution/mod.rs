//! Server-side execution of Guardian-executable proposals: the chain view an attempt runs
//! against, the data store the executor reads through, foreign-account loading, and sealing
//! of submission inputs.

mod aborts;
mod attempt;
mod chain;
mod foreign;
mod request;
mod sealing;
mod store;
mod threshold;

pub use attempt::MidenExecutor;
pub use chain::{ChainViewError, build_chain_view};
pub use foreign::{ForeignAccountUnavailable, ForeignAccounts};
pub use request::StoredRequest;
pub use store::ExecutionDataStore;

#[cfg(all(test, feature = "e2e"))]
mod tests;

#[cfg(test)]
mod live_tests;

/// Builds the executor for a Miden node endpoint and the configured remote prover.
pub fn executor_for(
    node_endpoint: &str,
    node_timeout: std::time::Duration,
    prover: &crate::config::execution::ProverConfig,
    config: &crate::config::execution::ExecutionConfig,
) -> Result<std::sync::Arc<dyn crate::services::execute_proposal::ProposalExecutor>, String> {
    use miden_client::remote_prover::RemoteTransactionProver;
    use miden_client::rpc::{Endpoint, GrpcClient};

    let endpoint = Endpoint::try_from(node_endpoint)
        .map_err(|e| format!("invalid Miden node endpoint for execution: {e}"))?;
    let timeout_ms = u64::try_from(node_timeout.as_millis()).unwrap_or(u64::MAX);
    let rpc = GrpcClient::new(&endpoint, timeout_ms);
    let prover = RemoteTransactionProver::new(prover.url.expose_secret().to_string())
        .with_timeout(prover.timeout);
    Ok(std::sync::Arc::new(MidenExecutor::new(
        std::sync::Arc::new(rpc),
        std::sync::Arc::new(prover),
        config.clone(),
    )))
}
