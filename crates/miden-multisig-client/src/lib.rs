//! Miden Multisig Client SDK
//!
//! A high-level SDK for interacting with multisig accounts on Miden,
//! coordinated through Private State Manager (PSM) servers.
//!
//! # Overview
//!
//! This crate provides a simple, ergonomic API for:
//! - Creating and managing multisig accounts
//! - Coordinating multi-party transaction signing via PSM
//! - Executing common multisig operations (transfers, signer management)
//!
//! # Quick Start
//!
//! ```ignore
//! use miden_multisig_client::{MultisigClient, MultisigConfig, PsmConfig};
//! use miden_client::rpc::Endpoint;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a client with auto-generated keys
//!     let mut client = MultisigClient::builder()
//!         .miden_endpoint(Endpoint::new("http://localhost:57291"))
//!         .data_dir("/tmp/multisig-client")
//!         .generate_key()
//!         .build()
//!         .await?;
//!
//!     // Print your commitment for sharing with cosigners
//!     println!("Your commitment: {}", client.user_commitment_hex());
//!
//!     // Create a 2-of-3 multisig
//!     let config = MultisigConfig::new(
//!         2,  // threshold
//!         vec![signer1, signer2, signer3],  // commitments
//!         PsmConfig::new("http://localhost:50051"),
//!     );
//!     let account = client.create_account(config).await?;
//!
//!     // Register with PSM so other cosigners can pull
//!     client.push_account(&account).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! # Architecture
//!
//! The SDK follows a proposal-based workflow for multisig operations:
//!
//! 1. **Create Proposal**: Any cosigner initiates a transaction
//! 2. **Sign Proposal**: Other cosigners review and add signatures
//! 3. **Finalize Proposal**: Once threshold is met, submit to network
//!
//! All coordination happens through PSM, which stores proposals and
//! collects signatures from multiple parties.

mod account;
mod builder;
mod client;
mod config;
mod error;
mod keystore;
mod proposal;
mod sync;
mod transaction;

// Main client
pub use builder::MultisigClientBuilder;
pub use client::MultisigClient;

// Configuration
pub use config::{MultisigConfig, PsmConfig};

// Account types
pub use account::MultisigAccount;

// Key management
pub use keystore::{KeyManager, PsmKeyStore, commitment_from_hex};

// Proposals
pub use proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

// Errors
pub use error::{MultisigError, Result};

// Re-exports for convenience
pub use miden_client::rpc::Endpoint;
pub use miden_objects::Word;
pub use miden_objects::account::AccountId;
pub use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
