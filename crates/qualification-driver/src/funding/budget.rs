/// What one run may move out of the treasury, and what it actually did.
///
/// The cap is a hard stop rather than a warning: an unattended run that funds
/// in a loop is the failure mode worth guarding against, and a warning nobody
/// reads is not a guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpendBudget {
    cap: u64,
    spent: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this run would spend {would_total} but its cap is {cap}")]
pub struct CapExceeded {
    pub cap: u64,
    pub would_total: u64,
}

impl SpendBudget {
    pub const fn new(cap: u64) -> Self {
        Self { cap, spent: 0 }
    }

    pub const fn spent(&self) -> u64 {
        self.spent
    }

    pub const fn cap(&self) -> u64 {
        self.cap
    }

    pub const fn remaining(&self) -> u64 {
        self.cap.saturating_sub(self.spent)
    }

    pub fn reserve(&mut self, amount: u64) -> Result<(), CapExceeded> {
        let would_total = self.spent.saturating_add(amount);
        if would_total > self.cap {
            return Err(CapExceeded {
                cap: self.cap,
                would_total,
            });
        }
        self.spent = would_total;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spending_accumulates() {
        let mut budget = SpendBudget::new(100);
        budget.reserve(30).expect("within cap");
        budget.reserve(20).expect("within cap");
        assert_eq!(budget.spent(), 50);
        assert_eq!(budget.remaining(), 50);
    }

    #[test]
    fn the_cap_is_a_hard_stop() {
        let mut budget = SpendBudget::new(100);
        budget.reserve(60).expect("within cap");
        let error = budget.reserve(50).expect_err("exceeds the cap");
        assert_eq!(
            error,
            CapExceeded {
                cap: 100,
                would_total: 110
            }
        );
    }

    #[test]
    fn a_rejected_reservation_does_not_consume_budget() {
        let mut budget = SpendBudget::new(100);
        budget.reserve(60).expect("within cap");
        let _ = budget.reserve(50);
        assert_eq!(
            budget.spent(),
            60,
            "a refused reservation must not be charged"
        );
        budget
            .reserve(40)
            .expect("the remaining budget is still usable");
    }

    #[test]
    fn spending_exactly_the_cap_is_allowed() {
        let mut budget = SpendBudget::new(100);
        budget.reserve(100).expect("exactly the cap");
        assert_eq!(budget.remaining(), 0);
    }
}
