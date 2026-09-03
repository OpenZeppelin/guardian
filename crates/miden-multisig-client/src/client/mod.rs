//! Main MultisigClient implementation.
//!
//! This module provides the [`MultisigClient`] type for interacting with multisig accounts.
//! The implementation is split across submodules for better organization:
//!
//! - `account` - Account lifecycle operations (create, pull, push, sync)
//! - `proposals` - Proposal workflow (list, sign, execute, propose)
//! - `offline` - Offline proposal operations
//! - `notes` - Note filtering and listing
//! - `recovery` - Recovery primitives (transport backlog drain)
//! - `io` - Export/import functionality
//! - `proposal_note_import` - Recovery primitive: proposal-embedded note import
//! - `public_note_backfill` - Recovery primitive: historical public-note backfill by tag
//! - `helpers` - Internal GUARDIAN client helpers

mod account;
mod delta_history;
mod helpers;
mod io;
mod note_recovery;
mod notes;
mod offline;
mod proposal_note_import;
mod proposals;
mod public_note_backfill;
mod recovery;
#[cfg(test)]
mod switch_recovery_tests;
#[cfg(test)]
pub(crate) mod test_support;
pub use delta_history::{
    HistoryAssetKind, HistoryDecodeSection, HistoryDecodeWarning, HistoryEntry, HistoryEntryStatus,
    HistoryNote, HistoryNoteAsset, HistoryNoteTag, HistoryNoteVisibility, HistoryPage,
};
pub use note_recovery::{
    NoteRecoveryOptions, NoteRecoveryReport, RecoveryStep, RecoveryStepProblem,
};
pub use proposal_note_import::{NoteImportOutcome, NoteImportSource, NoteImportStatus};
pub use proposals::{AbandonRequestState, AbandonStatus};
pub use public_note_backfill::{BlockRange, PublicBackfillOptions, PublicBackfillReport};
pub use recovery::{TransportRecoveryReport, TransportRecoveryStatus};

use std::path::PathBuf;
use std::sync::Arc;

use guardian_client::GetStateResponse;
use miden_client::rpc::Endpoint;
use miden_protocol::Word;
use miden_protocol::account::AccountId;

use crate::MidenSdkClient;
use crate::account::MultisigAccount;
use crate::builder::MultisigClientBuilder;
use crate::error::{MultisigError, Result};
use crate::export::ExportedProposal;
use crate::keystore::KeyManager;
use crate::proposal::Proposal;
use crate::prover::ProverConfig;
use crate::rpc::RpcConfig;

pub use notes::{ConsumableNote, NoteFilter};

/// Result of a proposal creation attempt.
///
/// When creating a proposal, it may either succeed online (via GUARDIAN) or
/// fall back to offline mode if GUARDIAN is unavailable.
#[derive(Debug)]
pub enum ProposalResult {
    /// Proposal successfully created on GUARDIAN and ready for cosigners to sign.
    Online(Box<Proposal>),
    /// Offline proposal created when GUARDIAN is unavailable (`SwitchGuardian` transactions only).
    Offline(Box<ExportedProposal>),
}

/// Result of explicit local-vs-on-chain account state verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateVerificationResult {
    /// Account ID that was verified.
    pub account_id: AccountId,
    /// Local account commitment hex (with 0x prefix).
    pub local_commitment_hex: String,
    /// On-chain account commitment hex (with 0x prefix).
    pub on_chain_commitment_hex: String,
}

/// Main client for interacting with multisig accounts.
///
/// This client manages a single multisig account connected to a GUARDIAN server,
/// providing a high-level API for creating and managing multisig accounts,
/// proposals, and transactions.
///
/// # Example
///
/// ```ignore
/// use miden_multisig_client::MultisigClient;
/// use miden_client::rpc::Endpoint;
///
///
/// let mut client = MultisigClient::builder()
///     .miden_endpoint(Endpoint::new("http://localhost:57291"))
///     .guardian_endpoint("http://localhost:50051")
///     .data_dir("/tmp/multisig")
///     .generate_key()
///     .build()
///     .await?;
///
///
/// let account = client.create_account(2, vec![signer1, signer2]).await?;
/// ```
pub struct MultisigClient {
    pub(crate) miden_client: MidenSdkClient,
    pub(crate) key_manager: Arc<dyn KeyManager>,
    /// Guardian server endpoint.
    pub(crate) guardian_endpoint: String,
    /// The multisig account managed by this client.
    pub(crate) account: Option<MultisigAccount>,
    /// Account directory for miden-client storage (for recovery).
    pub(crate) account_dir: PathBuf,
    /// Miden node endpoint (for recovery).
    pub(crate) miden_endpoint: Endpoint,
    /// Note transport endpoint override (for recovery).
    pub(crate) note_transport_endpoint: Option<String>,
    /// Node client for direct commitment reads, built once so its channel is
    /// reused across reads.
    node_rpc_client: Arc<dyn miden_client::rpc::NodeRpcClient>,
    /// Prover selection and retry configuration (for recovery).
    pub(crate) prover_config: ProverConfig,
    /// Node RPC timeout and read-retry configuration (for recovery).
    pub(crate) rpc_config: RpcConfig,
}

impl MultisigClient {
    /// Creates a new MultisigClientBuilder.
    pub fn builder() -> MultisigClientBuilder {
        MultisigClientBuilder::new()
    }

    /// Creates a new MultisigClient (internal use, prefer builder).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        miden_client: MidenSdkClient,
        key_manager: Arc<dyn KeyManager>,
        guardian_endpoint: String,
        account_dir: PathBuf,
        miden_endpoint: Endpoint,
        note_transport_endpoint: Option<String>,
        prover_config: ProverConfig,
        rpc_config: RpcConfig,
    ) -> Self {
        let node_rpc_client =
            crate::builder::configured_node_rpc_client(&miden_endpoint, &rpc_config);
        Self {
            miden_client,
            key_manager,
            guardian_endpoint,
            account: None,
            account_dir,
            miden_endpoint,
            note_transport_endpoint,
            node_rpc_client,
            prover_config,
            rpc_config,
        }
    }

    pub(crate) fn node_rpc_client(&self) -> Arc<dyn miden_client::rpc::NodeRpcClient> {
        Arc::clone(&self.node_rpc_client)
    }

    /// Swaps the node RPC client for a mock, so offline tests can drive the
    /// success paths of node-backed primitives.
    #[cfg(test)]
    pub(crate) fn set_node_rpc_client(
        &mut self,
        client: Arc<dyn miden_client::rpc::NodeRpcClient>,
    ) {
        self.node_rpc_client = client;
    }

    /// Returns the GUARDIAN endpoint.
    pub fn guardian_endpoint(&self) -> &str {
        &self.guardian_endpoint
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

    /// Recover the set of accounts the configured signer authorizes by
    /// querying GUARDIAN's `/state/lookup` endpoint and fetching state for
    /// each match. Mirrors `MultisigClient.recoverByKey` in the TS SDK.
    /// Returns an empty list when no account on the configured GUARDIAN
    /// authorizes this commitment (distinct from "wrong key", which fails
    /// authentication first).
    pub async fn recover_by_key(&self) -> Result<Vec<RecoveredAccount>> {
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let commitment_hex = self.user_commitment_hex();

        let lookup = guardian_client
            .lookup_account_by_key_commitment(&commitment_hex)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("lookup failed: {}", e)))?;

        let mut recovered = Vec::with_capacity(lookup.accounts.len());
        for entry in lookup.accounts {
            let account_id = AccountId::from_hex(&entry.account_id).map_err(|e| {
                MultisigError::InvalidConfig(format!(
                    "GUARDIAN returned non-AccountId hex '{}': {}",
                    entry.account_id, e
                ))
            })?;
            let state = guardian_client.get_state(&account_id).await.map_err(|e| {
                MultisigError::GuardianServer(format!(
                    "get_state failed for {}: {}",
                    entry.account_id, e
                ))
            })?;
            recovered.push(RecoveredAccount {
                account_id: entry.account_id,
                state,
            });
        }
        Ok(recovered)
    }
}

/// One match returned by [`MultisigClient::recover_by_key`]. Pairs the
/// discovered `account_id` with the current state response so callers do not
/// need to do a second round-trip per account.
#[derive(Debug, Clone)]
pub struct RecoveredAccount {
    pub account_id: String,
    pub state: GetStateResponse,
}
