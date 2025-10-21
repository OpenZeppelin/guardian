use crate::storage::DeltaObject;

mod filter;
mod processor;
mod worker;

pub use worker::{
    process_all_accounts_now as process_canonicalizations_now,
    start_worker as start_canonicalization_worker,
};

#[derive(Debug, Clone)]
struct CandidateDelta {
    delta: DeltaObject,
}

#[derive(Debug, Clone)]
struct VerifiedDelta {
    delta: DeltaObject,
    on_chain_commitment: String,
}

#[derive(Debug, Clone)]
enum VerificationResult {
    Matched(VerifiedDelta),
    Mismatched {
        delta: DeltaObject,
        expected_commitment: String,
        actual_commitment: String,
    },
}

impl CandidateDelta {
    fn new(delta: DeltaObject) -> Self {
        Self { delta }
    }

    fn verify(self, on_chain_commitment: String) -> VerificationResult {
        if self.delta.new_commitment == on_chain_commitment {
            VerificationResult::Matched(VerifiedDelta {
                delta: self.delta,
                on_chain_commitment,
            })
        } else {
            VerificationResult::Mismatched {
                expected_commitment: self.delta.new_commitment.clone(),
                actual_commitment: on_chain_commitment,
                delta: self.delta,
            }
        }
    }
}

impl VerifiedDelta {
    fn delta(&self) -> &DeltaObject {
        &self.delta
    }
}
