//! Miden Multisig Client SDK
//!
//! A high-level SDK for interacting with multisig accounts on Miden,
//! coordinated through Guardian servers.
//!
//! # Quick Start
//!
//! ```ignore
//! use miden_multisig_client::MultisigClient;
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
//!     let account = client.create_account(2, vec![signer1, signer2, signer3]).await?;
//!
//!     // Register with GUARDIAN so other cosigners can pull
//!     client.push_account().await?;
//!
//!     Ok(())
//! }
//! ```
//!

mod account;
mod builder;
mod client;
mod error;
mod execution;
mod export;
mod guardian_endpoint;
mod keystore;
mod payload;
mod procedures;
mod proposal;
mod transaction;
mod utils;

// Main client
pub use builder::MultisigClientBuilder;
pub use client::{
    ConsumableNote, MultisigClient, NoteFilter, ProposalResult, StateVerificationResult,
};

// Procedures
pub use procedures::{ProcedureName, ProcedureThreshold};

// Account types
pub use account::MultisigAccount;

// Key management and hex utilities
pub use keystore::{
    EcdsaGuardianKeyStore,
    FalconKeyStore,
    GuardianKeyStore,
    KeyManager,
    // Hex utilities
    commitment_from_hex,
    ensure_hex_prefix,
    proposal_public_key_hex,
    strip_hex_prefix,
    validate_commitment_hex,
    word_from_hex,
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
pub use guardian_shared::SignatureScheme;
pub use miden_client::rpc::Endpoint;
pub use miden_protocol::Word;
pub use miden_protocol::account::AccountId;
pub use miden_protocol::asset::Asset;
pub use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
pub use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
pub use miden_protocol::note::NoteId;
