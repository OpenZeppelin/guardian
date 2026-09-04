//! Miden Multisig Client SDK
//!
//! A high-level SDK for interacting with multisig accounts on Miden,
//! coordinated through Guardian servers.
//!
//! # Quick Start
//!
//! ```ignore
//! use miden_multisig_client::{MultisigClient, ProverConfig, ProverRetryPolicy};
//! use miden_client::rpc::Endpoint;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a client with auto-generated keys
//!     let mut client = MultisigClient::builder()
//!         .miden_endpoint(Endpoint::new("http://localhost:57291"))
//!         .guardian_endpoint("http://localhost:50051")
//!         .account_dir("/tmp/multisig-client")
//!         .prover_config(
//!             ProverConfig::new()
//!                 .with_url("https://prover.example")?
//!                 .with_retry_policy(ProverRetryPolicy::new(4)),
//!         )
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

use miden_client::Client;
use miden_client::keystore::FilesystemKeyStore;

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
mod prover;
mod rpc;
mod transaction;
mod utils;

pub(crate) type MidenSdkClient = Client<FilesystemKeyStore>;

// Main client
pub use builder::MultisigClientBuilder;
pub use client::{AbandonRequestState, AbandonStatus};
pub use client::{
    BlockRange, ConsumableNote, HistoryAssetKind, HistoryDecodeSection, HistoryDecodeWarning,
    HistoryEntry, HistoryEntryStatus, HistoryNote, HistoryNoteAsset, HistoryNoteTag,
    HistoryNoteVisibility, HistoryPage, MultisigClient, NoteFilter, NoteImportOutcome,
    NoteImportSource, NoteImportStatus, NoteRecoveryOptions, NoteRecoveryReport, ProposalResult,
    PublicBackfillOptions, PublicBackfillReport, RecoveredAccount, RecoveryStep,
    RecoveryStepProblem, StateVerificationResult, TransportRecoveryReport, TransportRecoveryStatus,
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
pub use execution::{SignatureAdvice, build_transfer_asset};
pub use payload::{ProposalMetadataPayload, ProposalPayload};
pub use proposal::{
    CONSUME_NOTES_METADATA_VERSION_V2, MAX_CONSUME_NOTES_METADATA_BYTES, P2ideHeights, Proposal,
    ProposalMetadata, ProposalStatus, SerializedNote, TransactionType,
};
pub use prover::{ProverConfig, ProverRetryPolicy};
pub use rpc::{RpcConfig, RpcRetryPolicy};
pub use transaction::{
    ProposalBuilder, build_p2id_transaction_request, deserialize_transaction_request, generate_salt,
};

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
pub use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
pub use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
pub use miden_protocol::note::{NoteId, NoteType};
