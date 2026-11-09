//! Policy module metrics; names and help text are declared in `crate::metrics::names`.

use std::time::Duration;

use crate::metrics::names;

#[derive(Debug, Clone, Copy)]
pub enum Outcome {
    Allow,
    Deny,
    Error,
    /// Not called: open circuit, oversized request or spent budget.
    Skipped,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Allow => "allow",
            Outcome::Deny => "deny",
            Outcome::Error => "error",
            Outcome::Skipped => "skipped",
        }
    }
}

pub fn evaluated(module: &str, outcome: Outcome, took: Duration) {
    metrics::histogram!(
        names::MODULE_EVALUATE_DURATION_SECONDS,
        names::LABEL_MODULE => module.to_string(),
        names::LABEL_OUTCOME => outcome.as_str()
    )
    .record(took.as_secs_f64());
}

pub fn failed_open(module: &str) {
    metrics::counter!(names::MODULE_FAIL_OPEN_TOTAL, names::LABEL_MODULE => module.to_string())
        .increment(1);
}

fn flag(name: &'static str, module: &str, on: bool) {
    metrics::gauge!(name, names::LABEL_MODULE => module.to_string()).set(if on {
        1.0
    } else {
        0.0
    });
}

pub fn up(module: &str, up: bool) {
    flag(names::MODULE_UP, module, up);
}

pub fn circuit_open(module: &str, open: bool) {
    flag(names::MODULE_CIRCUIT_OPEN, module, open);
}

pub fn enabled(module: &str, enabled: bool) {
    flag(names::MODULE_ENABLED, module, enabled);
}

pub fn config_drift(module: &str, drift: bool) {
    flag(names::MODULE_CONFIG_DRIFT, module, drift);
}

pub fn catalog(catalog_hash: &str) {
    metrics::gauge!(names::MODULE_CATALOG_INFO, names::LABEL_CATALOG_HASH => catalog_hash.to_string())
        .set(1.0);
}

pub fn stored_assignments(over_budget_accounts: usize, orphaned_rows: usize) {
    metrics::gauge!(names::MODULE_OVER_BUDGET_ACCOUNTS).set(over_budget_accounts as f64);
    metrics::gauge!(names::MODULE_ORPHANED_ASSIGNMENTS).set(orphaned_rows as f64);
}
