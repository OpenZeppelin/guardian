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
//!
//!     let mut client = MultisigClient::builder()
//!         .miden_endpoint(Endpoint::new("http://localhost:57291"))
//!         .data_dir("/tmp/multisig-client")
//!         .generate_key()
//!         .build()
//!         .await?;
//!
//!
//!     println!("Your commitment: {}", client.user_commitment_hex());
//!
//!
//!     let config = MultisigConfig::new(
//!         2,
//!         vec![signer1, signer2, signer3],
//!         PsmConfig::new("http://localhost:50051"),
//!     );
//!     let account = client.create_account(config).await?;
//!
//!
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
mod procedures;
mod proposal;
mod transaction;

pub use builder::MultisigClientBuilder;
pub use client::{ConsumableNote, MultisigClient, NoteFilter, ProposalResult};

pub use config::{MultisigConfig, ProcedureThreshold, PsmConfig};

pub use procedures::ProcedureName;

pub use account::MultisigAccount;

pub use keystore::{
    EcdsaPsmKeyStore, KeyManager, PsmKeyStore, SchemeSecretKey, commitment_from_hex,
    ensure_hex_prefix, strip_hex_prefix, validate_commitment_hex,
};

pub use payload::{ProposalMetadataPayload, ProposalPayload};
pub use proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};
pub use transaction::ProposalBuilder;

pub use export::{EXPORT_VERSION, ExportedMetadata, ExportedProposal, ExportedSignature};

pub use error::{MultisigError, Result};

pub use miden_client::rpc::Endpoint;
pub use miden_protocol::Word;
pub use miden_protocol::account::AccountId;
pub use miden_protocol::asset::Asset;
pub use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
pub use miden_protocol::note::NoteId;
pub use private_state_manager_shared::SignatureScheme;
