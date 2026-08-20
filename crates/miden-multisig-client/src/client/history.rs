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

impl HistoryNote {
    fn from_proto(note: ProtoHistoryNote) -> Self {
        Self {
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
}

impl HistoryEntry {
    fn from_proto(entry: ProtoHistoryEntry) -> Self {
        Self {
            nonce: entry.nonce,
            timestamp: entry.timestamp,
            new_commitment: entry.new_commitment,
            input_notes: entry
                .input_notes
                .into_iter()
                .map(HistoryNote::from_proto)
                .collect(),
            output_notes: entry
                .output_notes
                .into_iter()
                .map(HistoryNote::from_proto)
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
            entries: response
                .entries
                .into_iter()
                .map(HistoryEntry::from_proto)
                .collect(),
            next_cursor: response.next_cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guardian_client::{HistoryDecodeWarning as ProtoWarning, HistoryNoteAsset as ProtoAsset};

    #[test]
    fn entry_from_proto_maps_every_field() {
        let entry = HistoryEntry::from_proto(ProtoHistoryEntry {
            nonce: 7,
            timestamp: "2026-08-19T12:00:07Z".to_string(),
            new_commitment: Some("0xnew".to_string()),
            input_notes: vec![ProtoHistoryNote {
                note_id: "0xin".to_string(),
                tag: "custom".to_string(),
                assets: vec![],
                sender: None,
                recipient: None,
            }],
            output_notes: vec![ProtoHistoryNote {
                note_id: "0xout".to_string(),
                tag: "p2id".to_string(),
                assets: vec![ProtoAsset {
                    asset_id: "0xfaucet".to_string(),
                    kind: "fungible".to_string(),
                    amount: Some("100".to_string()),
                }],
                sender: Some("0xsender".to_string()),
                recipient: Some("0xrecipient".to_string()),
            }],
            decode_warnings: vec![ProtoWarning {
                section: "tx_summary".to_string(),
                reason: "malformed_tx_summary".to_string(),
            }],
        });

        assert_eq!(entry.nonce, 7);
        assert_eq!(entry.timestamp, "2026-08-19T12:00:07Z");
        assert_eq!(entry.new_commitment.as_deref(), Some("0xnew"));
        assert_eq!(entry.input_notes.len(), 1);
        assert_eq!(entry.input_notes[0].note_id, "0xin");
        assert_eq!(entry.input_notes[0].tag, "custom");
        assert!(entry.input_notes[0].sender.is_none());
        let out = &entry.output_notes[0];
        assert_eq!(out.note_id, "0xout");
        assert_eq!(out.tag, "p2id");
        assert_eq!(out.assets[0].asset_id, "0xfaucet");
        assert_eq!(out.assets[0].kind, "fungible");
        assert_eq!(out.assets[0].amount.as_deref(), Some("100"));
        assert_eq!(out.sender.as_deref(), Some("0xsender"));
        assert_eq!(out.recipient.as_deref(), Some("0xrecipient"));
        assert_eq!(entry.decode_warnings.len(), 1);
        assert_eq!(entry.decode_warnings[0].section, "tx_summary");
        assert_eq!(entry.decode_warnings[0].reason, "malformed_tx_summary");
    }
}
