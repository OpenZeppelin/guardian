//! Multisig configuration transaction utilities.
//!
//! Functions for building transactions that modify the multisig configuration
//! (signers, threshold, etc.) and for preparing signature advice entries.

mod config;
mod signature;

pub use config::build_update_signers_transaction_request;
pub use signature::{build_ecdsa_signature_advice_entry, build_signature_advice_entry};
