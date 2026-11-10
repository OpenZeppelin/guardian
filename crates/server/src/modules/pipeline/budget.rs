//! The evaluation budget of an account's stored modules: the check an operator write must pass,
//! and the recount behind the over-budget and orphaned-row gauges.

use super::ModulePipeline;
use crate::error::GuardianError;
use crate::modules::catalog::{HookName, Scope};
use crate::modules::store::{AccountParamsRow, ParamsKind};

/// An account whose stored modules could take longer than the evaluation budget.
struct OverBudget {
    account_id: String,
    hook: HookName,
    worst_case: std::time::Duration,
}

impl ModulePipeline {
    /// The hook with the largest worst-case module time for an account whose rows name
    /// `modules`, and that time: every `scope = all` instance, once more for an account layer on
    /// it, plus every listed instance. Disabled instances count, since enabling needs no further
    /// check; a row naming an instance the catalog no longer declares costs nothing.
    fn worst_case(&self, modules: &[(&str, ParamsKind)]) -> (HookName, std::time::Duration) {
        self.costs(modules)
            .into_iter()
            .max_by_key(|(_, total)| *total)
            .unwrap_or((
                HookName::BeforeTransactionApproved,
                std::time::Duration::ZERO,
            ))
    }

    /// The summed timeouts of the module calls an account naming `modules` makes at each gate
    /// hook.
    fn costs(&self, modules: &[(&str, ParamsKind)]) -> Vec<(HookName, std::time::Duration)> {
        let calls = |id: &str, kind: ParamsKind| {
            modules
                .iter()
                .filter(|(module_id, named)| *named == kind && *module_id == id)
                .count() as u32
        };
        HookName::GATES
            .into_iter()
            .map(|hook| {
                let total = self
                    .instances
                    .iter()
                    .filter(|instance| instance.config.hooks.contains(&hook))
                    .map(|instance| {
                        let id = instance.config.id.as_str();
                        let count = match instance.config.scope {
                            Scope::All => 1 + calls(id, ParamsKind::Layer),
                            Scope::Default | Scope::Assigned => calls(id, ParamsKind::Assignment),
                        };
                        instance.config.timeout * count
                    })
                    .sum();
                (hook, total)
            })
            .collect()
    }

    /// Refuses a write that would take the account's modules over the budget at a hook where
    /// they are not already at least that far over. A write that keeps or lowers the cost passes,
    /// so an account a deploy pushed over the budget can still be brought back under it.
    pub fn check_budget(
        &self,
        current: &[AccountParamsRow],
        proposed: &[AccountParamsRow],
    ) -> Result<(), GuardianError> {
        let named = |rows: &[AccountParamsRow]| -> Vec<(HookName, std::time::Duration)> {
            let modules: Vec<(&str, ParamsKind)> = rows
                .iter()
                .map(|row| (row.module_id.as_str(), row.kind))
                .collect();
            self.costs(&modules)
        };
        for ((hook, total), (_, now)) in named(proposed).into_iter().zip(named(current)) {
            if total > self.limits.budget && total > now {
                return Err(GuardianError::InvalidInput(format!(
                    "the account's modules at {} would take up to {} ms, over the evaluation budget of {} ms",
                    hook.as_str(),
                    total.as_millis(),
                    self.limits.budget.as_millis()
                )));
            }
        }
        Ok(())
    }

    /// The accounts whose stored modules exceed the budget, each with its worst hook and that
    /// hook's time, and the count of stored rows naming an instance the catalog no longer
    /// declares. One scan of which module each account row names, without params.
    async fn scan_assignments(&self) -> Result<(Vec<OverBudget>, usize), String> {
        let rows = self.store.account_modules().await?;
        let orphaned = rows
            .iter()
            .filter(|row| self.instance(&row.module_id).is_none())
            .count();
        let over = rows
            .chunk_by(|a, b| a.account_id == b.account_id)
            .filter_map(|account| {
                let modules: Vec<(&str, ParamsKind)> = account
                    .iter()
                    .map(|row| (row.module_id.as_str(), row.kind))
                    .collect();
                let (hook, worst_case) = self.worst_case(&modules);
                (worst_case > self.limits.budget).then(|| OverBudget {
                    account_id: account[0].account_id.clone(),
                    hook,
                    worst_case,
                })
            })
            .collect();
        Ok((over, orphaned))
    }

    /// The over-budget account count and the orphaned row count as last counted.
    #[cfg(test)]
    pub fn assignment_report(&self) -> (usize, usize) {
        self.assignment_report
            .lock()
            .map(|report| *report)
            .unwrap_or_default()
    }

    fn record_assignment_report(&self, over_budget: usize, orphaned: usize) {
        if let Ok(mut report) = self.assignment_report.lock() {
            *report = (over_budget, orphaned);
        }
    }

    /// Logs every account whose stored modules exceed the budget, without refusing to start: a
    /// deploy that raised a timeout must not become an outage. Returns the count of those
    /// accounts and of stored rows naming an instance the catalog no longer declares.
    pub async fn report_stored_assignments(&self) -> Result<(usize, usize), String> {
        let (over, orphaned) = self.scan_assignments().await?;
        for account in &over {
            tracing::warn!(
                account_id = %account.account_id,
                hook = account.hook.as_str(),
                worst_case_ms = account.worst_case.as_millis() as u64,
                budget_ms = self.limits.budget.as_millis() as u64,
                "Account's policy modules exceed the evaluation budget; the runtime budget applies"
            );
        }
        self.record_assignment_report(over.len(), orphaned);
        Ok((over.len(), orphaned))
    }

    /// Recounts the over-budget accounts and orphaned rows for the gauges, so they follow what
    /// operators change after startup: an alarm clears once the assignment is fixed.
    pub async fn refresh_assignment_report(&self) {
        match self.scan_assignments().await {
            Ok((over, orphaned)) => self.record_assignment_report(over.len(), orphaned),
            Err(error) => {
                tracing::warn!(%error, "Stored module assignments not recounted; the gauges keep their last values");
            }
        }
    }
}
