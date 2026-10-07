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
) -> Result<std::sync::Arc<dyn crate::services::execute_proposal::ProposalExecutor>, String> {
    use miden_client::remote_prover::RemoteTransactionProver;
    use miden_client::rpc::{Endpoint, GrpcClient, VerifyingRpcClient};

    let endpoint = Endpoint::try_from(node_endpoint)
        .map_err(|e| format!("invalid Miden node endpoint for execution: {e}"))?;
    let timeout_ms = u64::try_from(node_timeout.as_millis()).unwrap_or(u64::MAX);
    let rpc = VerifyingRpcClient::new(GrpcClient::new(&endpoint, timeout_ms));
    let remote = RemoteTransactionProver::new(prover.url.expose_secret().to_string())
        .with_timeout(prover.timeout);
    let prover: std::sync::Arc<dyn miden_client::transaction::TransactionProver + Send + Sync> =
        match prover.max_concurrent {
            Some(limit) => std::sync::Arc::new(BoundedProver {
                inner: remote,
                permits: tokio::sync::Semaphore::new(limit as usize),
            }),
            None => std::sync::Arc::new(remote),
        };
    Ok(std::sync::Arc::new(MidenExecutor::new(
        std::sync::Arc::new(rpc),
        prover,
    )))
}

/// Bounds the proofs this process has at the prover at once. The permit covers the prover call
/// alone, so an attempt backing off between retries, reading the chain, executing locally or
/// submitting holds none, and attempts beyond the bound wait here instead of reaching the
/// prover.
struct BoundedProver<P> {
    inner: P,
    permits: tokio::sync::Semaphore,
}

#[async_trait::async_trait]
impl<P: miden_client::transaction::TransactionProver + Send + Sync>
    miden_client::transaction::TransactionProver for BoundedProver<P>
{
    async fn prove(
        &self,
        inputs: miden_protocol::transaction::TransactionInputs,
    ) -> Result<miden_protocol::transaction::ProvenTransaction, miden_tx::TransactionProverError>
    {
        bounded(&self.permits, self.inner.prove(inputs)).await
    }
}

async fn bounded<T>(
    permits: &tokio::sync::Semaphore,
    call: impl std::future::Future<Output = T>,
) -> T {
    let _permit = permits
        .acquire()
        .await
        .expect("the proof permits are never closed");
    call.await
}

#[cfg(test)]
mod bounded_prover_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn no_more_proofs_than_permits_reach_the_prover_at_once() {
        let permits = Arc::new(tokio::sync::Semaphore::new(2));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls: Vec<_> = (0..6)
            .map(|_| {
                let (permits, in_flight, peak) = (permits.clone(), in_flight.clone(), peak.clone());
                tokio::spawn(async move {
                    super::bounded(&permits, async {
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
                })
            })
            .collect();
        for call in calls {
            call.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(permits.available_permits(), 2, "every permit is returned");
    }
}
