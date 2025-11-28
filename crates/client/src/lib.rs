//! Private State Manager Client
//!
//! A gRPC client library for interacting with the Private State Manager (PSM) server,
//! providing secure off-chain state management for Miden accounts.
//!
//! # Overview
//!
//! This crate provides:
//! - [`PsmClient`] - The main client for communicating with PSM servers
//! - [`Auth`] - Authentication types for signing requests
//! - [`FalconRpoSigner`] - Falcon signature-based authentication
//! - Error types for handling PSM-related failures
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use private_state_manager_client::{PsmClient, Auth, FalconRpoSigner};
//! use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to PSM server
//!     let mut client = PsmClient::connect("http://localhost:50051").await?;
//!
//!     // Configure authentication
//!     let secret_key = SecretKey::new();
//!     let auth = Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key));
//!     let client = client.with_auth(auth);
//!
//!     Ok(())
//! }
//! ```
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
pub use transaction::{TryIntoTxSummary, tx_summary_commitment_hex};
