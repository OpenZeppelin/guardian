use miden_protocol::account::{Account, AccountId};

use super::fees::FeeModel;
use super::network::vault_balances;

/// Why a run cannot start, kept distinct from anything a scenario could report.
///
/// A treasury problem is a setup failure: the run has proved nothing about the
/// product, and saying so plainly is what stops it being read as a defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Usability {
    /// This chain charges nothing, so funding is not needed at all.
    FundingNotRequired,
    Usable {
        faucet: AccountId,
        balance: u64,
        projected_runs: u64,
    },
    NotDeployed,
    WrongAsset {
        expected: AccountId,
        held: Vec<AccountId>,
    },
    Underfunded {
        faucet: AccountId,
        balance: u64,
        required: u64,
    },
}

impl Usability {
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Usable { .. } | Self::FundingNotRequired)
    }

    /// Phrased for whoever has to act on it rather than for whoever wrote it.
    pub fn remediation(&self) -> Option<String> {
        match self {
            Self::FundingNotRequired | Self::Usable { .. } => None,
            Self::NotDeployed => Some(
                "The treasury has never transacted. Send it a note and run treasury-bootstrap, \
                 which consumes the note, deploys the account, and pays its own fee out of it."
                    .to_string(),
            ),
            Self::WrongAsset { expected, held } => Some(format!(
                "The treasury holds none of {expected}, the asset this chain charges fees in. It \
                 holds: {}. Fund it with the chain's own fee asset.",
                if held.is_empty() {
                    "nothing".to_string()
                } else {
                    held.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )),
            Self::Underfunded {
                balance, required, ..
            } => Some(format!(
                "The treasury holds {balance} but this run needs {required}. Top it up from the \
                 network's faucet before running again."
            )),
        }
    }
}

pub fn assess(
    account: Option<&Account>,
    fees: &FeeModel,
    required: u64,
    per_run_cost: u64,
) -> Usability {
    if !fees.charges_fees() {
        return Usability::FundingNotRequired;
    }

    let Some(account) = account else {
        return Usability::NotDeployed;
    };

    let balances = vault_balances(account);
    let Some((_, balance)) = balances.iter().copied().find(|(id, _)| *id == fees.faucet) else {
        return Usability::WrongAsset {
            expected: fees.faucet,
            held: balances.into_iter().map(|(id, _)| id).collect(),
        };
    };

    if balance < required {
        return Usability::Underfunded {
            faucet: fees.faucet,
            balance,
            required,
        };
    }

    Usability::Usable {
        faucet: fees.faucet,
        balance,
        projected_runs: balance.checked_div(per_run_cost).unwrap_or(u64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faucet() -> AccountId {
        AccountId::from_hex("0xaabbccddeeff00011b27a8df4ddbe0").expect("valid id")
    }

    fn zero_fee() -> FeeModel {
        FeeModel {
            faucet: faucet(),
            verification_base_fee: 0,
        }
    }

    fn charging() -> FeeModel {
        FeeModel {
            faucet: faucet(),
            verification_base_fee: 100,
        }
    }

    #[test]
    fn a_zero_fee_chain_needs_no_funding() {
        let assessment = assess(None, &zero_fee(), 1_000, 10);
        assert_eq!(assessment, Usability::FundingNotRequired);
        assert!(assessment.is_usable());
        assert!(assessment.remediation().is_none());
    }

    #[test]
    fn an_undeployed_treasury_names_the_bootstrap() {
        let assessment = assess(None, &charging(), 1_000, 10);
        assert_eq!(assessment, Usability::NotDeployed);
        assert!(!assessment.is_usable());
        assert!(
            assessment
                .remediation()
                .expect("has remediation")
                .contains("treasury-bootstrap")
        );
    }

    #[test]
    fn every_unusable_state_tells_the_operator_what_to_do() {
        let states = [
            Usability::NotDeployed,
            Usability::WrongAsset {
                expected: faucet(),
                held: Vec::new(),
            },
            Usability::Underfunded {
                faucet: faucet(),
                balance: 1,
                required: 2,
            },
        ];
        for state in states {
            assert!(state.remediation().is_some(), "{state:?} needs remediation");
            assert!(!state.is_usable(), "{state:?} must not read as usable");
        }
    }

    #[test]
    fn an_underfunded_treasury_names_both_amounts() {
        let assessment = Usability::Underfunded {
            faucet: faucet(),
            balance: 7,
            required: 99,
        };
        let remediation = assessment.remediation().expect("has remediation");
        assert!(remediation.contains('7') && remediation.contains("99"));
    }
}
