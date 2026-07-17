mod processor;
mod removal;
mod worker;

pub(crate) use removal::{RemovalMode, record_candidate_outcome, remove_candidate};
pub use worker::{
    process_all_accounts_now as process_canonicalizations_now,
    start_worker as start_canonicalization_worker,
};
