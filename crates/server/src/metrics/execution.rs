//! Guardian execution metrics. Internal phases are diagnosed here and in logs, never on the wire.

use crate::metrics::names::{
    EXECUTION_CAPACITY_REFUSALS_TOTAL, EXECUTION_OBSERVATION_OUTAGE_SECONDS,
    EXECUTION_OLDEST_RESERVATION_AGE_SECONDS, EXECUTION_OUTCOMES_TOTAL, LABEL_CODE, LABEL_OUTCOME,
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

pub fn record_oldest_reservation_age(seconds: f64) {
    metrics::gauge!(EXECUTION_OLDEST_RESERVATION_AGE_SECONDS).set(seconds);
}

pub fn record_observation_outage(seconds: f64) {
    metrics::gauge!(EXECUTION_OBSERVATION_OUTAGE_SECONDS).set(seconds);
}
