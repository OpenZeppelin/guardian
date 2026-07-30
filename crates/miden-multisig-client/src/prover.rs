//! Retrying wrapper around a transaction prover.
//!
//! On the public networks proving is delegated to a shared remote prover, which
//! cancels requests under load. Measured against testnet, 45% of operations at
//! both 8 and 16 concurrent writers failed with
//! `TransactionProvingError(... Status { code: Cancelled, message: "Timeout
//! expired" })` -- an identical proportion at double the offered load, which is
//! the signature of a saturated queue rather than a rejected transaction.
//!
//! Retrying *here* is safe in a way that retrying the surrounding execution is
//! not. By the time proving runs, the delta has already been pushed to GUARDIAN
//! to obtain the acknowledgment signature, so a second proof attempt leaves
//! GUARDIAN's state untouched. Retrying the whole execution instead re-pushes
//! that delta and is refused as a pending-delta conflict -- measured as one
//! failed account becoming twenty-five.

use std::sync::Arc;
use std::time::Duration;

use miden_client::transaction::TransactionProver;
use miden_protocol::transaction::{ProvenTransaction, TransactionInputs};
use miden_tx::TransactionProverError;

/// Default number of proof attempts, including the first.
const DEFAULT_ATTEMPTS: u32 = 4;

/// Base delay before the first retry; doubles per attempt.
const DEFAULT_BASE_DELAY_MS: u64 = 500;

/// Ceiling for a single backoff wait.
const MAX_DELAY_MS: u64 = 8_000;

/// Wraps a prover and retries transient proving failures with bounded
/// exponential backoff and jitter.
pub struct RetryingTransactionProver {
    inner: Arc<dyn TransactionProver + Send + Sync>,
    attempts: u32,
    base_delay: Duration,
}

impl RetryingTransactionProver {
    pub fn new(inner: Arc<dyn TransactionProver + Send + Sync>) -> Self {
        Self {
            inner,
            attempts: DEFAULT_ATTEMPTS,
            base_delay: Duration::from_millis(DEFAULT_BASE_DELAY_MS),
        }
    }

    /// Override the attempt budget. One attempt disables retrying.
    #[must_use]
    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts.max(1);
        self
    }

    #[must_use]
    pub fn with_base_delay(mut self, base_delay: Duration) -> Self {
        self.base_delay = base_delay;
        self
    }

    /// Backoff for `attempt` (0-based), doubling and capped, with +/-25%
    /// jitter so concurrent writers that failed together do not retry in
    /// lockstep and re-saturate the prover.
    fn backoff(&self, attempt: u32) -> Duration {
        let doubled = self
            .base_delay
            .as_millis()
            .saturating_mul(1u128 << attempt.min(16)) as f64;
        let jitter = 0.75 + rand::random::<f64>() * 0.5;
        // Cap after jitter: a ceiling that jitter can exceed is not a ceiling.
        Duration::from_millis(((doubled * jitter) as u64).min(MAX_DELAY_MS))
    }
}

/// Whether a proving failure is worth another attempt.
///
/// Only transport-level cancellation and exhaustion qualify. A proof the prover
/// rejected on its merits fails the same way every time, so retrying it would
/// burn the budget and delay the real error.
///
/// `TransactionProverError` carries no typed status -- its catch-all variant is
/// `Other { error_msg, source }` -- so the signal can only be read out of the
/// rendered chain. Two renderings appear in practice, and both must match:
///
///   * the queue giving up:  `code: 'The operation was cancelled', message:
///     "Timeout expired"`, whose cause is a `tonic` transport error;
///   * the connection never being serviced: `code: 'Unknown error', message:
///     "connection error: desc = \"i/o timeout\""`.
///
/// The second was read as permanent and retried zero times, losing 424
/// operations across the 64- and 16-writer #317 legs. Matching a gRPC code would
/// not have caught it either: the node reports it as `Unknown`, so it is
/// recognisable only as a transport failure.
fn is_transient(error: &TransactionProverError) -> bool {
    // Walk the source chain, not just the top message. These errors carry the
    // cause via thiserror's `#[source]`, so `to_string()` yields only "failed to
    // prove transaction" and the transport status -- "Timeout expired", the very
    // signal being matched -- sits one or more levels down. Reading only the top
    // message classified every proving failure as permanent, so the retry never
    // fired and failure durations were unchanged.
    let mut message = error.to_string().to_ascii_lowercase();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push(' ');
        message.push_str(&cause.to_string().to_ascii_lowercase());
        source = cause.source();
    }

    const QUEUE_SIGNALS: [&str; 5] = [
        "timeout expired",
        "cancelled",
        "deadline",
        "unavailable",
        "too many requests",
    ];
    // A connection the prover accepted but never serviced. Rendered without any
    // cancellation or exhaustion wording, so the queue signals never see it.
    const TRANSPORT_SIGNALS: [&str; 5] = [
        "i/o timeout",
        "connection error",
        "transport error",
        "connection reset",
        "broken pipe",
    ];

    QUEUE_SIGNALS
        .iter()
        .chain(TRANSPORT_SIGNALS.iter())
        .any(|signal| message.contains(signal))
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl TransactionProver for RetryingTransactionProver {
    async fn prove(
        &self,
        tx_inputs: TransactionInputs,
    ) -> Result<ProvenTransaction, TransactionProverError> {
        let mut last_error = None;
        for attempt in 0..self.attempts {
            // TransactionInputs is not Clone-free to reuse across attempts, so
            // each attempt proves the same inputs value passed by clone.
            match self.inner.prove(tx_inputs.clone()).await {
                Ok(proven) => return Ok(proven),
                Err(error) if is_transient(&error) => {
                    last_error = Some(error);
                    if attempt + 1 < self.attempts {
                        let delay = self.backoff(attempt);
                        tokio_sleep(delay).await;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.expect("at least one attempt runs, so a failure is recorded"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn tokio_sleep(delay: Duration) {
    tokio::time::sleep(delay).await;
}

#[cfg(target_arch = "wasm32")]
async fn tokio_sleep(_delay: Duration) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from `scale-20260729T213814Z-records.jsonl`: the queue gave up.
    const CANCELLED_RENDERING: &str = "transaction proving failed: failed to prove transaction: code: 'The operation was cancelled', message: \"Timeout expired\", source: tonic::transport::Error(Transport, TimeoutExpired(()))";

    /// Verbatim from the same run: the connection was never serviced. Carries no
    /// cancellation or exhaustion wording at all.
    const TRANSPORT_RENDERING: &str = "transaction proving failed: failed to prove transaction: code: 'Unknown error', message: \"connection error: desc = \\\"i/o timeout\\\"\"";

    /// The rendering older miden-client versions produced, kept so that dropping
    /// the dead `code: cancelled` arm cannot silently narrow coverage.
    const LEGACY_CANCELLED_RENDERING: &str =
        "failed to prove transaction: Status { code: Cancelled, message: \"Timeout expired\" }";

    #[test]
    fn a_cancelled_proof_is_retried() {
        assert!(is_transient(&TransactionProverError::other(
            CANCELLED_RENDERING
        )));
        assert!(is_transient(&TransactionProverError::other(
            LEGACY_CANCELLED_RENDERING
        )));
    }

    #[test]
    fn a_transport_failure_is_retried() {
        // Regression: this rendering matched no signal, so 424 operations across
        // the #317 64- and 16-writer legs were classified permanent and retried
        // zero times. It is not recognisable by gRPC code either -- the node
        // reports `Unknown` -- only as a transport failure.
        assert!(is_transient(&TransactionProverError::other(
            TRANSPORT_RENDERING
        )));
    }

    #[test]
    fn every_signal_is_matched_by_some_real_rendering() {
        // A signal no rendering exercises is dead weight that reads as coverage:
        // the arm this replaces matched `code: cancelled`, which this
        // miden-client renders as `code: 'The operation was cancelled'`, so it
        // could never fire. Retiring a signal here is deliberate, not incidental.
        for signal in ["timeout expired", "cancelled", "i/o timeout"] {
            assert!(
                CANCELLED_RENDERING.to_ascii_lowercase().contains(signal)
                    || TRANSPORT_RENDERING.to_ascii_lowercase().contains(signal)
                    || LEGACY_CANCELLED_RENDERING
                        .to_ascii_lowercase()
                        .contains(signal),
                "no observed rendering contains {signal:?}, so the arm is dead"
            );
        }
    }

    #[test]
    fn the_transient_signal_is_found_in_the_source_chain() {
        // Regression: these errors carry the cause via `#[source]`, so the top
        // message is only "failed to prove transaction". Reading it alone
        // classified every failure as permanent and the retry never fired.
        #[derive(Debug)]
        struct Transport;
        impl std::fmt::Display for Transport {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "Status {{ code: Cancelled, message: \"Timeout expired\" }}"
                )
            }
        }
        impl std::error::Error for Transport {}

        let wrapped =
            TransactionProverError::other_with_source("failed to prove transaction", Transport);
        assert!(
            !wrapped
                .to_string()
                .to_ascii_lowercase()
                .contains("cancelled"),
            "the top message must not carry the signal, or this proves nothing"
        );
        assert!(is_transient(&wrapped));
    }

    #[test]
    fn a_rejected_proof_is_not_retried() {
        // Retrying a proof the prover rejected on its merits would burn the
        // budget and delay surfacing the real error.
        let rejected =
            TransactionProverError::other("transaction kernel program failed: assertion failed");
        assert!(!is_transient(&rejected));
    }

    #[test]
    fn backoff_doubles_and_stays_capped() {
        let prover = RetryingTransactionProver {
            inner: Arc::new(NoopProver),
            attempts: 8,
            base_delay: Duration::from_millis(500),
        };

        // Jitter is +/-25%, so assert the band rather than an exact value.
        let first = prover.backoff(0).as_millis() as u64;
        assert!((375..=625).contains(&first), "first backoff was {first}ms");

        let third = prover.backoff(2).as_millis() as u64;
        assert!(
            (1_500..=2_500).contains(&third),
            "third backoff was {third}ms"
        );

        let far = prover.backoff(12).as_millis() as u64;
        assert!(far <= MAX_DELAY_MS, "backoff must stay capped, got {far}ms");
    }

    #[test]
    fn attempts_cannot_be_zero() {
        let prover = RetryingTransactionProver::new(Arc::new(NoopProver)).with_attempts(0);
        assert_eq!(prover.attempts, 1, "one attempt still runs the prover once");
    }

    struct NoopProver;

    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    impl TransactionProver for NoopProver {
        async fn prove(
            &self,
            _tx_inputs: TransactionInputs,
        ) -> Result<ProvenTransaction, TransactionProverError> {
            Err(TransactionProverError::other("not used"))
        }
    }
}
