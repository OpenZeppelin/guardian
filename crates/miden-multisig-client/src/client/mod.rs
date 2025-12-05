//! Main MultisigClient implementation.
//!
//! This module provides the [`MultisigClient`] type for interacting with multisig accounts.
//! The implementation is split across submodules for better organization:
//!
//! - `account` - Account lifecycle operations (create, pull, push, sync)
//! - `proposals` - Proposal workflow (list, sign, execute, propose)
//! - `offline` - Offline proposal operations
//! - `notes` - Note filtering and listing
//! - `io` - Export/import functionality
//! - `helpers` - Internal PSM client helpers

mod account;
mod helpers;
mod io;
mod notes;
mod offline;
mod proposals;

use std::path::PathBuf;

use miden_client::Client;
use miden_client::rpc::Endpoint;
use miden_objects::Word;
use miden_objects::account::AccountId;

use crate::account::MultisigAccount;
use crate::builder::MultisigClientBuilder;
use crate::export::ExportedProposal;
use crate::keystore::KeyManager;
use crate::proposal::Proposal;

// Re-export public types from submodules
pub use notes::{ConsumableNote, NoteFilter};

/// Result of a proposal creation attempt.
///
/// When creating a proposal, it may either succeed online (via PSM) or
/// fall back to offline mode if PSM is unavailable.
#[derive(Debug)]
pub enum ProposalResult {
    /// Proposal successfully created on PSM and ready for cosigners to sign.
    Online(Box<Proposal>),
    /// Proposal created offline (PSM unavailable). Share with cosigners via file.
    Offline(Box<ExportedProposal>),
}

/// Main client for interacting with multisig accounts.
///
/// This client manages a single multisig account connected to a PSM server,
/// providing a high-level API for creating and managing multisig accounts,
/// proposals, and transactions.
///
/// # Example
///
/// ```ignore
/// use miden_multisig_client::{MultisigClient, MultisigConfig, PsmConfig};
/// use miden_client::rpc::Endpoint;
///
/// // Create a client
/// let mut client = MultisigClient::builder()
///     .miden_endpoint(Endpoint::new("http://localhost:57291"))
///     .psm_endpoint("http://localhost:50051")
///     .data_dir("/tmp/multisig")
///     .generate_key()
///     .build()
///     .await?;
///
/// // Create a multisig account
/// let account = client.create_account(2, vec![signer1, signer2]).await?;
/// ```
pub struct MultisigClient {
    pub(crate) miden_client: Client<()>,
    pub(crate) key_manager: Box<dyn KeyManager>,
    /// Private State Manager server endpoint.
    pub(crate) psm_endpoint: String,
    /// The multisig account managed by this client.
    pub(crate) account: Option<MultisigAccount>,
    /// Account directory for miden-client storage (for recovery).
    pub(crate) account_dir: PathBuf,
    /// Miden node endpoint (for recovery).
    pub(crate) miden_endpoint: Endpoint,
}

impl MultisigClient {
    /// Creates a new MultisigClientBuilder.
    pub fn builder() -> MultisigClientBuilder {
        MultisigClientBuilder::new()
    }

    /// Creates a new MultisigClient (internal use, prefer builder).
    pub(crate) fn new(
        miden_client: Client<()>,
        key_manager: Box<dyn KeyManager>,
        psm_endpoint: String,
        account_dir: PathBuf,
        miden_endpoint: Endpoint,
    ) -> Self {
        Self {
            miden_client,
            key_manager,
            psm_endpoint,
            account: None,
            account_dir,
            miden_endpoint,
        }
    }

    /// Returns the PSM endpoint.
    pub fn psm_endpoint(&self) -> &str {
        &self.psm_endpoint
    }

    /// Returns the current account, if any.
    pub fn account(&self) -> Option<&MultisigAccount> {
        self.account.as_ref()
    }

    /// Returns the current account ID, if any.
    pub fn account_id(&self) -> Option<AccountId> {
        self.account.as_ref().map(|a| a.id())
    }

    /// Returns true if an account is loaded.
    pub fn has_account(&self) -> bool {
        self.account.is_some()
    }

    /// Returns the user's public key commitment as a Word.
    pub fn user_commitment(&self) -> Word {
        self.key_manager.commitment()
    }

    /// Returns the user's public key commitment as a hex string.
    pub fn user_commitment_hex(&self) -> String {
        self.key_manager.commitment_hex()
    }

    /// Returns a reference to the key manager.
    pub fn key_manager(&self) -> &dyn KeyManager {
        self.key_manager.as_ref()
    }
}
