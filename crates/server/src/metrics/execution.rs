//! Guardian execution metrics. Internal phases are diagnosed here and in logs, never on the wire.

use crate::metrics::names::{
    EXECUTION_CAPACITY_REFUSALS_TOTAL, EXECUTION_CHAIN_VIEW_DURATION_SECONDS,
    EXECUTION_OBSERVATION_OUTAGE_SECONDS, EXECUTION_OLDEST_RESERVATION_AGE_SECONDS,
    EXECUTION_OUTCOMES_TOTAL, EXECUTION_PHASE_DURATION_SECONDS, EXECUTION_PROVER_RETRIES_TOTAL,
    EXECUTION_PROVING_DURATION_SECONDS, EXECUTION_RECONCILE_OUTCOMES_TOTAL,
    EXECUTION_RECORDS_PRUNED_TOTAL, LABEL_CODE, LABEL_OUTCOME, LABEL_PHASE,
};
use crate::storage::ExecutionFailureCode;

/// Counts an execution that ended: committed, or failed with its stable code.
pub fn record_outcome(failure: Option<&ExecutionFailureCode>) {
    let (outcome, code) = match failure {
        Some(code) => ("failed", code.as_str()),
        None => ("committed", "none"),
    };
    metrics::counter!(EXECUTION_OUTCOMES_TOTAL, LABEL_OUTCOME => outcome, LABEL_CODE => code)
        .increment(1);
}

/// Counts a request refused because the process already ran its maximum number of executions.
pub fn record_capacity_refusal() {
    metrics::counter!(EXECUTION_CAPACITY_REFUSALS_TOTAL).increment(1);
}

/// Records how long one worker phase took. `phase` is one of the worker's fixed phase names.
pub fn record_phase(phase: &'static str, took: std::time::Duration) {
    metrics::histogram!(EXECUTION_PHASE_DURATION_SECONDS, LABEL_PHASE => phase)
        .record(took.as_secs_f64());
}

/// Records how long assembling one attempt's chain view took.
pub fn record_chain_view(took: std::time::Duration) {
    metrics::histogram!(EXECUTION_CHAIN_VIEW_DURATION_SECONDS).record(took.as_secs_f64());
}

/// Records how long proving took, retries included.
pub fn record_proving(took: std::time::Duration) {
    metrics::histogram!(EXECUTION_PROVING_DURATION_SECONDS).record(took.as_secs_f64());
}

/// Counts one retry after a transient prover failure.
pub fn record_prover_retry() {
    metrics::counter!(EXECUTION_PROVER_RETRIES_TOTAL).increment(1);
}

/// Counts one reconciled execution by the outcome the reconciler reached.
pub fn record_reconcile_outcome(outcome: &'static str) {
    metrics::counter!(EXECUTION_RECONCILE_OUTCOMES_TOTAL, LABEL_OUTCOME => outcome).increment(1);
}

pub fn record_oldest_reservation_age(seconds: f64) {
    metrics::gauge!(EXECUTION_OLDEST_RESERVATION_AGE_SECONDS).set(seconds);
}

pub fn record_observation_outage(seconds: f64) {
    metrics::gauge!(EXECUTION_OBSERVATION_OUTAGE_SECONDS).set(seconds);
}

/// Counts the attempts one retention batch deleted.
pub fn record_records_pruned(attempts: usize) {
    metrics::counter!(EXECUTION_RECORDS_PRUNED_TOTAL).increment(attempts as u64);
}
