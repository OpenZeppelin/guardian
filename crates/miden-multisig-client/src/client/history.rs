//! Canonical transaction history (issue #413).
//!
//! Thin typed layer over GUARDIAN's `GetTransactionHistory` RPC so callers do not
//! handle proto types directly. Mirrors `Multisig.transactionHistory` in the TS
//! SDK: one page per call, newest-first by nonce, resumable via the
//! opaque cursor.

use guardian_client::{HistoryEntry as ProtoHistoryEntry, HistoryNote as ProtoHistoryNote};

use crate::client::MultisigClient;
use crate::error::{MultisigError, Result};

/// One page of an account's canonical transaction history.
#[derive(Debug, Clone)]
pub struct HistoryPage {
    /// Entries newest-first by nonce.
    pub entries: Vec<HistoryEntry>,
    /// Opaque resume token for the next page; `None` when the feed is
    /// exhausted.
    pub next_cursor: Option<String>,
}

/// One canonical transaction in an account's history.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub nonce: u64,
    /// RFC 3339 UTC timestamp at which the delta became canonical.
    pub timestamp: String,
    /// Account commitment after this transaction; `None` when the
    /// stored row predates commitment recording.
    pub new_commitment: Option<String>,
    pub input_notes: Vec<HistoryNote>,
    pub output_notes: Vec<HistoryNote>,
    /// Why the note sections are empty when they are: the persisted
    /// payload could not be decoded server-side (schema drift).
    pub decode_warnings: Vec<HistoryDecodeWarning>,
}

/// One decoded note attached to a history entry. `tag` is the stable
/// wire label (`p2id` / `p2ide` / `pswap` / `mint` / `burn` / `custom`).
#[derive(Debug, Clone)]
pub struct HistoryNote {
    pub note_id: String,
    pub tag: String,
    pub assets: Vec<HistoryNoteAsset>,
    pub sender: Option<String>,
    pub recipient: Option<String>,
}

/// One decoded asset inside a history note. `kind` is `fungible` or
/// `non_fungible`; `amount` is a base-10 string for fungible assets.
#[derive(Debug, Clone)]
pub struct HistoryNoteAsset {
    pub asset_id: String,
    pub kind: String,
    pub amount: Option<String>,
}

/// Server-side decode warning attached to a history entry.
#[derive(Debug, Clone)]
pub struct HistoryDecodeWarning {
    pub section: String,
    pub reason: String,
}

fn note_from_proto(note: ProtoHistoryNote) -> HistoryNote {
    HistoryNote {
        note_id: note.note_id,
        tag: note.tag,
        assets: note
            .assets
            .into_iter()
            .map(|asset| HistoryNoteAsset {
                asset_id: asset.asset_id,
                kind: asset.kind,
                amount: asset.amount,
            })
            .collect(),
        sender: note.sender,
        recipient: note.recipient,
    }
}

fn entry_from_proto(entry: ProtoHistoryEntry) -> HistoryEntry {
    HistoryEntry {
        nonce: entry.nonce,
        timestamp: entry.timestamp,
        new_commitment: entry.new_commitment,
        input_notes: entry.input_notes.into_iter().map(note_from_proto).collect(),
        output_notes: entry
            .output_notes
            .into_iter()
            .map(note_from_proto)
            .collect(),
        decode_warnings: entry
            .decode_warnings
            .into_iter()
            .map(|warning| HistoryDecodeWarning {
                section: warning.section,
                reason: warning.reason,
            })
            .collect(),
    }
}

impl MultisigClient {
    /// Fetch one page of the loaded account's canonical transaction
    /// history from GUARDIAN, newest-first by nonce.
    ///
    /// `limit` is the page size in `[1, 500]` (server default 50 when
    /// `None`); `cursor` resumes from a previous page's `next_cursor`
    /// (`None` for the first page). Only transactions pushed through
    /// GUARDIAN appear — it never sees transactions executed elsewhere.
    pub async fn transaction_history(
        &mut self,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<HistoryPage> {
        let account_id = self.require_account()?.id();

        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let response = guardian_client
            .get_transaction_history(&account_id, limit, cursor)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("failed to get history: {}", e)))?;

        Ok(HistoryPage {
            entries: response.entries.into_iter().map(entry_from_proto).collect(),
            next_cursor: response.next_cursor,
        })
    }
}
