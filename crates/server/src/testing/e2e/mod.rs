// E2E tests (enabled with `--features e2e`)
#![cfg(feature = "e2e")]

mod abandon_candidate;
mod candidate_queue;
mod configure_account;
mod switch_guardian_canonicalization;
