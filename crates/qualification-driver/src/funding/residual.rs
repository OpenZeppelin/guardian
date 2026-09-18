use serde::{Deserialize, Serialize};

/// What happens to whatever is left in an ephemeral account when a run ends.
///
/// On-chain accounts cannot be deleted, so something is always left behind.
/// Saying which of these applies is not optional: unswept residue is what
/// drains a treasury over time, and a suite that never states its policy
/// discovers the answer when funding runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidualPolicy {
    /// Return what is left to the treasury before the run ends.
    Sweep,
    /// Leave it, and count it against the run's spend.
    Accept,
}

impl Default for ResidualPolicy {
    /// Accepting is the default because a sweep is itself a transaction, and on
    /// a chain that charges per transaction it usually costs more than the dust
    /// it recovers. Funding each account with the minimum it needs keeps that
    /// residue small by construction.
    fn default() -> Self {
        Self::Accept
    }
}

/// What a run actually spent, once residue is attributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidualOutcome {
    pub policy: ResidualPolicy,
    pub recovered: u64,
    pub accepted: u64,
}

impl ResidualOutcome {
    /// Residue left behind is a real cost and is charged to the run. Residue
    /// swept back is not, because the treasury has it again.
    pub fn effective_spend(&self, transferred: u64) -> u64 {
        match self.policy {
            ResidualPolicy::Sweep => transferred.saturating_sub(self.recovered),
            ResidualPolicy::Accept => transferred,
        }
    }
}

pub fn accept(residual_total: u64) -> ResidualOutcome {
    ResidualOutcome {
        policy: ResidualPolicy::Accept,
        recovered: 0,
        accepted: residual_total,
    }
}

pub fn swept(recovered: u64) -> ResidualOutcome {
    ResidualOutcome {
        policy: ResidualPolicy::Sweep,
        recovered,
        accepted: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepting_charges_the_whole_transfer_to_the_run() {
        let outcome = accept(250);
        assert_eq!(outcome.effective_spend(1_000), 1_000);
        assert_eq!(outcome.accepted, 250);
    }

    #[test]
    fn sweeping_credits_what_came_back() {
        let outcome = swept(250);
        assert_eq!(outcome.effective_spend(1_000), 750);
        assert_eq!(outcome.accepted, 0);
    }

    #[test]
    fn recovering_more_than_was_sent_cannot_produce_a_negative_spend() {
        let outcome = swept(5_000);
        assert_eq!(outcome.effective_spend(1_000), 0);
    }

    #[test]
    fn the_default_leaves_residue_rather_than_paying_to_chase_it() {
        assert_eq!(ResidualPolicy::default(), ResidualPolicy::Accept);
    }
}
