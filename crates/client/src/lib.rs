pub use private_state_manager_shared::hex::{FromHex, IntoHex};
pub use private_state_manager_shared::{FromJson, ToJson};

mod proto {
    tonic::include_proto!("state_manager");
}

pub mod auth;
mod client;
mod error;
mod transaction;

#[cfg(test)]
pub mod testing;

pub use auth::{Auth, FalconRpoSigner, verify_commitment_signature};
pub use client::PsmClient;
pub use error::{ClientError, ClientResult};
pub use proto::*;
pub use transaction::{extract_tx_summary_from_delta, tx_summary_commitment_hex};
