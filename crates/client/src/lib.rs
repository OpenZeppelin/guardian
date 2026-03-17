//! Private State Manager Client
//!
//! A gRPC client library for interacting with the Private State Manager (PSM) server,
//! providing secure off-chain state management for Miden accounts.
//!
//! # Overview
//!
//! This crate provides:
//! - [`PsmClient`] - The main client for communicating with PSM servers
//! - [`Auth`] - Scheme-aware authentication providers for request signing
//! - [`keystore::Signer`] - The signing boundary for authenticated requests
//! - [`FalconKeyStore`] - The default in-memory Falcon signer
//! - [`EcdsaKeyStore`] - The default in-memory ECDSA signer
//! - Error types for handling PSM-related failures
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use private_state_manager_client::{FalconKeyStore, PsmClient};
//! use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to PSM server
//!     let mut client = PsmClient::connect("http://localhost:50051").await?;
//!
//!     // Configure request signing
//!     let secret_key = SecretKey::new();
//!     let signer = Arc::new(FalconKeyStore::new(secret_key));
//!     let client = client.with_signer(signer);
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
pub mod keystore;
mod transaction;

#[cfg(test)]
pub mod testing;

pub use auth::{Auth, EcdsaSigner, FalconRpoSigner};
pub use client::PsmClient;
pub use error::{ClientError, ClientResult};
pub use keystore::{EcdsaKeyStore, FalconKeyStore, Signer, verify_commitment_signature};
pub use proto::*;
pub use transaction::{TryIntoTxSummary, tx_summary_commitment_hex};
