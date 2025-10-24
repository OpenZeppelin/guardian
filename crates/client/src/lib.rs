pub use private_state_manager_shared::{FromJson, ToJson};

mod proto {
    tonic::include_proto!("state_manager");
}

pub mod auth;
mod client;
mod error;

pub use auth::{Auth, FalconRpoSigner, verify_commitment_signature};
pub use client::PsmClient;
pub use error::{ClientError, ClientResult};
pub use proto::*;
