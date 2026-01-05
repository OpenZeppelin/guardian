//! Miden Multisig Client SDK
//!
//! A high-level SDK for interacting with multisig accounts on Miden,
//! coordinated through Private State Manager (PSM) servers.
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

mod account;
mod builder;
mod client;
mod config;
mod error;
mod execution;
mod export;
mod keystore;
mod payload;
mod proposal;
mod transaction;

// Main client
pub use builder::MultisigClientBuilder;
pub use client::{ConsumableNote, MultisigClient, NoteFilter, ProposalResult};

// Configuration
pub use config::{MultisigConfig, PsmConfig};

// Account types
pub use account::MultisigAccount;

// Key management and hex utilities
pub use keystore::{
    KeyManager,
    PsmKeyStore,
    // Hex utilities
    commitment_from_hex,
    ensure_hex_prefix,
    strip_hex_prefix,
    validate_commitment_hex,
};

// Proposals
pub use payload::{ProposalMetadataPayload, ProposalPayload};
pub use proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};
pub use transaction::ProposalBuilder;

// Export/Import
pub use export::{EXPORT_VERSION, ExportedMetadata, ExportedProposal, ExportedSignature};

// Errors
pub use error::{MultisigError, Result};

// Re-exports for convenience
pub use miden_client::rpc::Endpoint;
pub use miden_objects::Word;
pub use miden_objects::account::AccountId;
pub use miden_objects::asset::Asset;
pub use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
pub use miden_objects::note::NoteId;
