//! Guardian Client
//!
//! A gRPC client library for interacting with the Guardian server,
//! providing secure off-chain state management for Miden accounts.
//!
//! # Overview
//!
//! This crate provides:
//! - [`GuardianClient`] - The main client for communicating with GUARDIAN servers
//! - [`Auth`] - Scheme-aware authentication providers for request signing
//! - [`keystore::Signer`] - The signing boundary for authenticated requests
//! - [`FalconKeyStore`] - The default in-memory Falcon signer
//! - [`EcdsaKeyStore`] - The default in-memory ECDSA signer
//! - Error types for handling GUARDIAN-related failures
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use guardian_client::{FalconKeyStore, GuardianClient};
//! use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to GUARDIAN server
//!     let mut client = GuardianClient::connect("http://localhost:50051").await?;
//!
//!     // Configure request signing
//!     let secret_key = SecretKey::new();
//!     let signer = Arc::new(FalconKeyStore::new(secret_key));
//!     let client = client.with_signer(signer);
//!
//!     Ok(())
//! }
//! ```
pub use guardian_shared::hex::{FromHex, IntoHex};
pub use guardian_shared::{FromJson, ToJson};

mod proto {
    tonic::include_proto!("guardian");
}

pub mod auth;
mod client;
mod error;
pub mod keystore;
mod transaction;

#[cfg(test)]
pub mod testing;

pub use auth::{Auth, EcdsaSigner, FalconRpoSigner};
pub use client::GuardianClient;
pub use error::{ClientError, ClientResult};
pub use keystore::{EcdsaKeyStore, FalconKeyStore, Signer, verify_commitment_signature};
pub use proto::*;
pub use transaction::{TryIntoTxSummary, tx_summary_commitment_hex};
