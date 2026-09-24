use crate::report::FundingSummary;

use super::budget::SpendBudget;
use super::usability::Usability;

/// Turns what a run observed and spent into the funding section of its result.
///
/// The projection is the part that matters operationally: a balance is only
/// actionable if it says how many more runs it supports, because that is what
/// tells someone to top up before a run fails rather than after.
pub fn summarize(usability: &Usability, budget: &SpendBudget) -> FundingSummary {
    match usability {
        Usability::FundingNotRequired => FundingSummary::not_required(),
        Usability::Usable {
            balance,
            projected_runs,
            ..
        } => FundingSummary {
            required: true,
            balance_at_start: Some(balance.to_string()),
            spent: Some(budget.spent().to_string()),
            projected_remaining_runs: Some(remaining_runs(
                *balance,
                budget.spent(),
                *projected_runs,
            )),
        },
        Usability::Underfunded { balance, .. } => FundingSummary {
            required: true,
            balance_at_start: Some(balance.to_string()),
            spent: Some(0.to_string()),
            projected_remaining_runs: Some(0),
        },
        Usability::NotDeployed | Usability::WrongAsset { .. } => FundingSummary {
            required: true,
            balance_at_start: None,
            spent: Some(0.to_string()),
            projected_remaining_runs: Some(0),
        },
    }
}

/// Projected from what this run actually spent when that is known, rather than
/// from an estimate, so the number tightens as real costs come in.
fn remaining_runs(balance: u64, spent: u64, fallback: u64) -> u64 {
    if spent == 0 {
        return fallback;
    }
    balance.saturating_sub(spent) / spent
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;

    fn faucet() -> AccountId {
        AccountId::from_hex("0xaabbccddeeff00011b27a8df4ddbe0").expect("valid id")
    }

    #[test]
    fn a_zero_fee_chain_reports_funding_as_not_required() {
        let summary = summarize(&Usability::FundingNotRequired, &SpendBudget::new(100));
        assert!(!summary.required);
        assert!(summary.balance_at_start.is_none());
    }

    #[test]
    fn the_projection_uses_what_this_run_actually_spent() {
        let mut budget = SpendBudget::new(1_000);
        budget.reserve(100).expect("within cap");
        let usable = Usability::Usable {
            faucet: faucet(),
            balance: 1_000,
            projected_runs: 999,
        };
        let summary = summarize(&usable, &budget);
        assert_eq!(summary.projected_remaining_runs, Some(9));
        assert_eq!(summary.spent.as_deref(), Some("100"));
    }

    #[test]
    fn a_run_that_spent_nothing_falls_back_to_the_estimate() {
        let usable = Usability::Usable {
            faucet: faucet(),
            balance: 1_000,
            projected_runs: 42,
        };
        let summary = summarize(&usable, &SpendBudget::new(1_000));
        assert_eq!(summary.projected_remaining_runs, Some(42));
    }

    #[test]
    fn an_underfunded_treasury_projects_no_further_runs() {
        let summary = summarize(
            &Usability::Underfunded {
                faucet: faucet(),
                balance: 1,
                required: 100,
            },
            &SpendBudget::new(100),
        );
        assert!(summary.required);
        assert_eq!(summary.projected_remaining_runs, Some(0));
        assert_eq!(summary.balance_at_start.as_deref(), Some("1"));
    }

    #[test]
    fn an_undeployed_treasury_reports_no_balance_rather_than_zero() {
        let summary = summarize(&Usability::NotDeployed, &SpendBudget::new(100));
        assert!(summary.required);
        assert!(
            summary.balance_at_start.is_none(),
            "an unknown balance must not be reported as a known zero"
        );
    }
}
