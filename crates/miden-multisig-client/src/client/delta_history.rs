//! Canonical delta history (issue #413).
//!
//! Thin typed layer over GUARDIAN's `GetDeltaHistory` RPC so callers do not
//! handle proto types directly. Mirrors `Multisig.deltaHistory` in the TS
//! SDK: one page per call, newest-first by nonce, resumable via the
//! opaque cursor.

use guardian_client::{HistoryEntry as ProtoHistoryEntry, HistoryNote as ProtoHistoryNote};

use crate::client::MultisigClient;
use crate::error::{MultisigError, Result};

/// One page of an account's canonical delta history.
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
    /// Delta lifecycle status; [`HistoryEntryStatus::Canonical`] for
    /// every entry today.
    pub status: HistoryEntryStatus,
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

/// One decoded note attached to a history entry.
#[derive(Debug, Clone)]
pub struct HistoryNote {
    pub note_id: String,
    pub tag: HistoryNoteTag,
    /// On-chain visibility from the note metadata.
    pub note_type: HistoryNoteVisibility,
    pub assets: Vec<HistoryNoteAsset>,
    pub sender: Option<String>,
    pub recipient: Option<String>,
}

/// One decoded asset inside a history note. `amount` is a base-10
/// string for fungible assets, absent for non-fungible ones.
#[derive(Debug, Clone)]
pub struct HistoryNoteAsset {
    pub asset_id: String,
    pub kind: HistoryAssetKind,
    pub amount: Option<String>,
}

/// Server-side decode warning attached to a history entry.
#[derive(Debug, Clone)]
pub struct HistoryDecodeWarning {
    pub section: HistoryDecodeSection,
    pub reason: String,
}

/// Generate a typed wire-vocabulary enum with an `Other` fallback so a
/// server that grows the vocabulary never breaks decoding — mirrors the
/// TS SDK's closed unions while staying forward-compatible.
macro_rules! wire_enum {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum $name {
            $($variant,)+
            /// A wire label this SDK version does not know yet.
            Other(String),
        }

        impl $name {
            fn from_wire(label: String) -> Self {
                match label.as_str() {
                    $($label => Self::$variant,)+
                    _ => Self::Other(label),
                }
            }

            /// The stable wire label for this value.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $label,)+
                    Self::Other(label) => label,
                }
            }
        }
    };
}

wire_enum! {
    /// Note classification decoded from the on-chain note script.
    HistoryNoteTag {
        P2id => "p2id",
        P2ide => "p2ide",
        Pswap => "pswap",
        Mint => "mint",
        Burn => "burn",
        Custom => "custom",
    }
}

wire_enum! {
    /// On-chain note visibility from the note metadata.
    HistoryNoteVisibility {
        Public => "public",
        Private => "private",
    }
}

wire_enum! {
    /// Asset kind inside a decoded note.
    HistoryAssetKind {
        Fungible => "fungible",
        NonFungible => "non_fungible",
    }
}

wire_enum! {
    /// Which section of the persisted payload failed to decode.
    HistoryDecodeSection {
        TxSummary => "tx_summary",
        Metadata => "metadata",
        InputNotes => "input_notes",
        OutputNotes => "output_notes",
        Vault => "vault",
        Storage => "storage",
    }
}

wire_enum! {
    /// Delta lifecycle status of a history entry.
    HistoryEntryStatus {
        Canonical => "canonical",
    }
}

impl HistoryNote {
    fn from_proto(note: ProtoHistoryNote) -> Self {
        Self {
            note_id: note.note_id,
            tag: HistoryNoteTag::from_wire(note.tag),
            note_type: HistoryNoteVisibility::from_wire(note.note_type),
            assets: note
                .assets
                .into_iter()
                .map(|asset| HistoryNoteAsset {
                    asset_id: asset.asset_id,
                    kind: HistoryAssetKind::from_wire(asset.kind),
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
            status: HistoryEntryStatus::from_wire(entry.status),
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
                    section: HistoryDecodeSection::from_wire(warning.section),
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
    pub async fn delta_history(
        &mut self,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<HistoryPage> {
        let account_id = self.require_account()?.id();

        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let response = guardian_client
            .get_delta_history(&account_id, limit, cursor)
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
            status: "canonical".to_string(),
            timestamp: "2026-08-19T12:00:07Z".to_string(),
            new_commitment: Some("0xnew".to_string()),
            input_notes: vec![ProtoHistoryNote {
                note_id: "0xin".to_string(),
                tag: "custom".to_string(),
                note_type: "private".to_string(),
                assets: vec![],
                sender: None,
                recipient: None,
            }],
            output_notes: vec![ProtoHistoryNote {
                note_id: "0xout".to_string(),
                tag: "p2id".to_string(),
                note_type: "public".to_string(),
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
        assert_eq!(entry.status, HistoryEntryStatus::Canonical);
        assert_eq!(entry.timestamp, "2026-08-19T12:00:07Z");
        assert_eq!(entry.new_commitment.as_deref(), Some("0xnew"));
        assert_eq!(entry.input_notes.len(), 1);
        assert_eq!(entry.input_notes[0].note_id, "0xin");
        assert_eq!(entry.input_notes[0].tag, HistoryNoteTag::Custom);
        assert_eq!(
            entry.input_notes[0].note_type,
            HistoryNoteVisibility::Private
        );
        assert!(entry.input_notes[0].sender.is_none());
        let out = &entry.output_notes[0];
        assert_eq!(out.note_id, "0xout");
        assert_eq!(out.tag, HistoryNoteTag::P2id);
        assert_eq!(out.note_type, HistoryNoteVisibility::Public);
        assert_eq!(out.assets[0].asset_id, "0xfaucet");
        assert_eq!(out.assets[0].kind, HistoryAssetKind::Fungible);
        assert_eq!(out.assets[0].amount.as_deref(), Some("100"));
        assert_eq!(out.sender.as_deref(), Some("0xsender"));
        assert_eq!(out.recipient.as_deref(), Some("0xrecipient"));
        assert_eq!(entry.decode_warnings.len(), 1);
        assert_eq!(
            entry.decode_warnings[0].section,
            HistoryDecodeSection::TxSummary
        );
        assert_eq!(entry.decode_warnings[0].reason, "malformed_tx_summary");

        // Unknown labels survive as Other rather than failing decode.
        assert_eq!(
            HistoryNoteTag::from_wire("brand_new_tag".to_string()),
            HistoryNoteTag::Other("brand_new_tag".to_string())
        );
    }
}
