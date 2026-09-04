//! Proposal types and utilities for multisig transactions.

use std::collections::HashSet;
use std::num::NonZeroU32;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use guardian_client::DeltaObject;
use guardian_shared::FromJson;
use guardian_shared::hex::FromHex;
use guardian_shared::{ProposalSignature, SignatureScheme};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, Signature as EcdsaSignature,
};
use miden_protocol::crypto::dsa::falcon512_poseidon2::Signature as Poseidon2FalconSignature;
use miden_protocol::note::{Note, NoteId, NoteType};
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{MultisigError, Result};
use crate::keystore::{ensure_hex_prefix, word_from_hex};
use crate::payload::ProposalPayload;
use crate::procedures::ProcedureName;

/// Max serialized v2 `consume_notes` metadata, enforced at creation. Spec FR-011.
pub const MAX_CONSUME_NOTES_METADATA_BYTES: usize = 256 * 1024;

/// `consume_notes` metadata schema version. Absence on the wire => v1 (legacy).
pub const CONSUME_NOTES_METADATA_VERSION_V2: u32 = 2;

/// Base64 of `Serializable::to_bytes(&note)`. Inner string is private so
/// every construction goes through `from_note` or `from_base64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SerializedNote(String);

impl SerializedNote {
    pub fn from_note(note: &Note) -> Self {
        Self(BASE64.encode(Serializable::to_bytes(note)))
    }

    /// Wraps an already-base64-encoded wire string. Validation is deferred to `to_note`.
    pub fn from_base64(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn to_note(&self) -> Result<Note> {
        let bytes = BASE64
            .decode(self.0.as_bytes())
            .map_err(|e| MultisigError::InvalidConfig(format!("invalid base64 in note: {}", e)))?;
        Note::read_from_bytes(&bytes)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to deserialize note: {}", e)))
    }
}

/// P2IDE execution constraints for a P2ID transfer (issue #366).
///
/// Presence of either height creates a P2IDE note instead of a plain P2ID
/// note; both `None` (the [`Default`]) means plain P2ID. `NonZeroU32` makes
/// the invalid zero height unrepresentable — `0` is the on-chain encoding
/// for "no constraint", so a zero here could silently build an
/// unconstrained note — and serde rejects a wire `0` at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct P2ideHeights {
    /// Absolute block height at which the sender may reclaim the note.
    pub reclaim: Option<NonZeroU32>,
    /// Absolute block height before which the note cannot be consumed.
    pub timelock: Option<NonZeroU32>,
}

impl P2ideHeights {
    /// Returns true when either constraint is set, i.e. the transfer
    /// creates a P2IDE note instead of a plain P2ID note.
    pub fn is_p2ide(&self) -> bool {
        self.reclaim.is_some() || self.timelock.is_some()
    }
}

/// Status of a proposal in the signing workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending,
    Ready,
    Finalized,
}

impl ProposalStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, ProposalStatus::Ready)
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ProposalStatus::Pending)
    }
}

/// Types of transactions supported by the multisig SDK.
///
/// Marked `#[non_exhaustive]` so future proposal types (including evolutions of
/// the custom-type support, issue #266) can be added without breaking external
/// crates: downstream `match` statements must already include a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransactionType {
    P2ID {
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
        /// Visibility of the created note (issue #322). Absent in legacy
        /// proposal metadata, which maps to [`NoteType::Public`].
        note_type: NoteType,
        /// P2IDE reclaim/timelock constraints (issue #366). The default
        /// (both `None`) is a plain P2ID note, the shape of pre-#366
        /// proposals.
        heights: P2ideHeights,
    },
    ConsumeNotes {
        note_ids: Vec<NoteId>,
        /// `None`/`Some(1)` => v1, `Some(2)` => v2 (issue #229).
        metadata_version: Option<u32>,
        /// v2 embedded notes, index-aligned with `note_ids`. Empty on v1.
        notes: Vec<SerializedNote>,
    },
    AddCosigner {
        new_commitment: Word,
    },
    RemoveCosigner {
        commitment: Word,
    },
    SwitchGuardian {
        new_endpoint: String,
        new_commitment: Word,
    },
    UpdateProcedureThreshold {
        procedure: ProcedureName,
        new_threshold: u32,
    },
    UpdateSigners {
        new_threshold: u32,
        signer_commitments: Vec<Word>,
    },
    /// A custom proposal whose `proposal_type` the SDK does not model (issue
    /// #266). Can be parsed, listed, signed, and exported/imported; the original
    /// label is preserved in `ProposalMetadata.proposal_type`. The generic SDK
    /// cannot build or execute the on-chain transaction for a custom procedure;
    /// the integration that owns the recipe drives execution via
    /// `prepare_custom_execution`.
    Custom,
}

impl TransactionType {
    /// Creates a P2ID transfer transaction with a public output note.
    pub fn transfer(recipient: AccountId, faucet_id: AccountId, amount: u64) -> Self {
        Self::transfer_with_note_type(recipient, faucet_id, amount, NoteType::Public)
    }

    /// Creates a P2ID transfer transaction with an explicit note visibility
    /// (issue #322).
    pub fn transfer_with_note_type(
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
        note_type: NoteType,
    ) -> Self {
        Self::P2ID {
            recipient,
            faucet_id,
            amount,
            note_type,
            heights: P2ideHeights::default(),
        }
    }

    /// Creates a P2IDE transfer transaction: a P2ID note with optional
    /// reclaim and/or timelock block heights (issue #366). Passing
    /// `P2ideHeights::default()` degenerates to a plain P2ID transfer.
    pub fn transfer_p2ide(
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
        note_type: NoteType,
        heights: P2ideHeights,
    ) -> Self {
        Self::P2ID {
            recipient,
            faucet_id,
            amount,
            note_type,
            heights,
        }
    }

    /// Legacy (v1) — no embedded notes. Use `consume_notes_v2` for new proposals.
    pub fn consume_notes(note_ids: Vec<NoteId>) -> Self {
        Self::ConsumeNotes {
            note_ids,
            metadata_version: None,
            notes: Vec::new(),
        }
    }

    /// v2 — embeds the notes inline so verification doesn't read the local store (issue #229).
    pub fn consume_notes_v2(note_ids: Vec<NoteId>, notes: Vec<SerializedNote>) -> Self {
        Self::ConsumeNotes {
            note_ids,
            metadata_version: Some(CONSUME_NOTES_METADATA_VERSION_V2),
            notes,
        }
    }

    /// Creates an AddCosigner transaction.
    pub fn add_cosigner(new_commitment: Word) -> Self {
        Self::AddCosigner { new_commitment }
    }

    /// Returns the signer-set size this transaction produces, given the
    /// current size, or `None` when the transaction does not change the
    /// signer set. Used to detect growth that dilutes per-procedure
    /// threshold overrides (absolute counts, never re-scaled on-chain).
    pub fn target_signer_count(&self, current_num_signers: u32) -> Option<u32> {
        match self {
            Self::AddCosigner { .. } => Some(current_num_signers + 1),
            Self::RemoveCosigner { .. } => Some(current_num_signers.saturating_sub(1)),
            Self::UpdateSigners {
                signer_commitments, ..
            } => Some(signer_commitments.len() as u32),
            Self::P2ID { .. }
            | Self::ConsumeNotes { .. }
            | Self::SwitchGuardian { .. }
            | Self::UpdateProcedureThreshold { .. }
            | Self::Custom => None,
        }
    }

    /// Creates a RemoveCosigner transaction.
    pub fn remove_cosigner(commitment: Word) -> Self {
        Self::RemoveCosigner { commitment }
    }

    /// Creates a SwitchGuardian transaction.
    pub fn switch_guardian(new_endpoint: impl Into<String>, new_commitment: Word) -> Self {
        Self::SwitchGuardian {
            new_endpoint: new_endpoint.into(),
            new_commitment,
        }
    }

    /// Creates an UpdateProcedureThreshold transaction.
    pub fn update_procedure_threshold(procedure: ProcedureName, new_threshold: u32) -> Self {
        Self::UpdateProcedureThreshold {
            procedure,
            new_threshold,
        }
    }

    /// Creates an UpdateSigners transaction.
    pub fn update_signers(new_threshold: u32, signer_commitments: Vec<Word>) -> Self {
        Self::UpdateSigners {
            new_threshold,
            signer_commitments,
        }
    }

    pub(crate) fn proposal_type(&self) -> Option<&'static str> {
        match self {
            Self::P2ID { .. } => Some("p2id"),
            Self::ConsumeNotes { .. } => Some("consume_notes"),
            Self::AddCosigner { .. } => Some("add_signer"),
            Self::RemoveCosigner { .. } => Some("remove_signer"),
            Self::SwitchGuardian { .. } => Some("switch_guardian"),
            Self::UpdateProcedureThreshold { .. } => Some("update_procedure_threshold"),
            Self::UpdateSigners { .. } => None,
            Self::Custom => None,
        }
    }

    /// Returns a stable transaction type name for diagnostics and flow checks.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::P2ID { .. } => "P2ID",
            Self::ConsumeNotes { .. } => "ConsumeNotes",
            Self::AddCosigner { .. } => "AddCosigner",
            Self::RemoveCosigner { .. } => "RemoveCosigner",
            Self::SwitchGuardian { .. } => "SwitchGuardian",
            Self::UpdateProcedureThreshold { .. } => "UpdateProcedureThreshold",
            Self::UpdateSigners { .. } => "UpdateSigners",
            Self::Custom => "Custom",
        }
    }

    /// Returns true when execution can proceed without a GUARDIAN acknowledgment.
    pub fn supports_offline_execution(&self) -> bool {
        matches!(self, Self::SwitchGuardian { .. })
    }

    /// Returns true when execution requires a GUARDIAN acknowledgment signature.
    pub fn requires_guardian_ack(&self) -> bool {
        !self.supports_offline_execution()
    }
}

/// Proposal type labels the SDK models natively. The producer (`propose_custom_transaction`)
/// path rejects these so an opaque transaction can never be mis-routed to a
/// built-in handler.
const BUILTIN_PROPOSAL_TYPES: &[&str] = &[
    "add_signer",
    "remove_signer",
    "change_threshold",
    "update_procedure_threshold",
    "switch_guardian",
    "consume_notes",
    "p2id",
    // Reserved: the SDK's internal bucket name for unmodeled types. A producer
    // must not use it as a custom label, or it would collide with the bucket.
    "custom",
];

pub(crate) fn is_builtin_proposal_type(proposal_type: &str) -> bool {
    BUILTIN_PROPOSAL_TYPES.contains(&proposal_type)
}

/// Metadata needed to reconstruct and finalize a proposal.
#[derive(Debug, Clone, Default)]
pub struct ProposalMetadata {
    pub tx_summary_json: Option<Value>,
    pub proposal_type: Option<String>,
    pub new_threshold: Option<u64>,
    pub signer_commitments_hex: Vec<String>,
    pub salt_hex: Option<String>,

    pub recipient_hex: Option<String>,
    pub faucet_id_hex: Option<String>,
    pub amount: Option<u64>,
    /// P2ID note visibility, `"public"` or `"private"` (issue #322).
    /// `None` => public, the wire shape of pre-#322 proposals.
    pub note_type: Option<String>,

    /// P2IDE reclaim block height (issue #366). Presence of either height
    /// means the proposal creates a P2IDE note; both `None` => plain P2ID.
    /// `NonZeroU32`: a wire `0` is rejected at deserialization.
    pub reclaim_height: Option<NonZeroU32>,
    /// P2IDE timelock block height (issue #366).
    pub timelock_height: Option<NonZeroU32>,

    pub note_ids_hex: Vec<String>,

    /// `consume_notes` metadata version. `None` => v1, `Some(2)` => v2.
    /// Other values are rejected at dispatch (spec FR-009).
    pub consume_notes_metadata_version: Option<u32>,

    /// v2 embedded notes, index-aligned with `note_ids_hex`. Empty on v1.
    pub consume_notes_notes: Vec<SerializedNote>,

    pub new_guardian_pubkey_hex: Option<String>,
    pub new_guardian_endpoint: Option<String>,
    pub target_procedure: Option<String>,

    pub required_signatures: Option<usize>,
    pub signers: Vec<String>,

    /// Base64-serialized Miden `ChainAnchor` pinning the reference block the
    /// tx_summary was built at. Required to verify or execute the proposal:
    /// since protocol 0.16 the signed summary binds the reference block
    /// commitment, so it only reproduces when re-executed at that block.
    pub chain_anchor_b64: Option<String>,
}

impl ProposalMetadata {
    pub fn is_consume_notes_v1(&self) -> bool {
        matches!(self.consume_notes_metadata_version, None | Some(1))
    }

    pub fn is_consume_notes_v2(&self) -> bool {
        self.consume_notes_metadata_version == Some(CONSUME_NOTES_METADATA_VERSION_V2)
    }

    /// Decodes the proposal's chain anchor. Errors when absent: a proposal
    /// without an anchor was created at an unknown reference block, so its
    /// signed summary cannot be reproduced, verified, or executed.
    pub fn chain_anchor(&self) -> Result<miden_client::transaction::ChainAnchor> {
        let anchor_b64 = self.chain_anchor_b64.as_deref().ok_or_else(|| {
            MultisigError::InvalidConfig(
                "proposal metadata has no chain_anchor; it was created without \
                 chain-anchored execution and its signed summary cannot be \
                 reproduced at the original reference block"
                    .to_string(),
            )
        })?;
        crate::transaction::chain_anchor_from_base64(anchor_b64)
    }

    /// Converts salt hex to Word.
    ///
    /// Errors when absent, for the same reason [`Self::chain_anchor`] does. The request
    /// declares this salt and miden-client commits `hash(CONVERSION_INFO || SALT)` into
    /// the auth arg from it, so a substituted zero would be committed just as happily as
    /// the real one and reproduce a summary no cosigner signed.
    pub fn salt(&self) -> Result<Word> {
        let value = self.salt_hex.as_deref().ok_or_else(|| {
            MultisigError::InvalidConfig(
                "proposal metadata has no salt; its request cannot be rebuilt because \
                 the auth arg commits hash(CONVERSION_INFO || SALT) and is not \
                 invertible to the salt"
                    .to_string(),
            )
        })?;
        word_from_hex(value).map_err(MultisigError::InvalidConfig)
    }

    /// Converts signer commitments to Words.
    pub fn signer_commitments(&self) -> Result<Vec<Word>> {
        let mut seen = HashSet::new();
        let mut commitments = Vec::with_capacity(self.signer_commitments_hex.len());

        for hex in &self.signer_commitments_hex {
            let commitment = word_from_hex(hex).map_err(MultisigError::InvalidConfig)?;
            let key = ensure_hex_prefix(hex).to_lowercase();
            if !seen.insert(key) {
                return Err(MultisigError::InvalidConfig(format!(
                    "duplicate signer commitment in metadata: {}",
                    hex
                )));
            }
            commitments.push(commitment);
        }

        Ok(commitments)
    }

    /// Parses `note_type` for a P2ID proposal. Absent => public (the only
    /// behavior before issue #322); an unrecognized value is rejected rather
    /// than silently rebuilt as a public note that could never match the
    /// signed tx_summary commitment.
    pub fn p2id_note_type(&self) -> Result<NoteType> {
        match self.note_type.as_deref() {
            None => Ok(NoteType::Public),
            Some(value) => value.parse().map_err(|_| {
                MultisigError::InvalidConfig(format!(
                    "unsupported metadata.note_type '{}': expected 'public' or 'private'",
                    value
                ))
            }),
        }
    }

    /// Converts note ID hex strings to NoteIds.
    pub fn note_ids(&self) -> Result<Vec<NoteId>> {
        self.note_ids_hex
            .iter()
            .map(|hex| {
                let word = word_from_hex(hex).map_err(MultisigError::InvalidConfig)?;
                Ok(NoteId::from_raw(word))
            })
            .collect()
    }

    pub(crate) fn to_transaction_type(&self, proposal_type: &str) -> Result<TransactionType> {
        if proposal_type.is_empty() {
            return Err(MultisigError::InvalidConfig(
                "proposal metadata.proposal_type is required".to_string(),
            ));
        }

        match proposal_type {
            "consume_notes" => {
                if self.note_ids_hex.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "consume_notes proposal requires metadata.note_ids".to_string(),
                    ));
                }
                Ok(TransactionType::ConsumeNotes {
                    note_ids: self.note_ids()?,
                    metadata_version: self.consume_notes_metadata_version,
                    notes: self.consume_notes_notes.clone(),
                })
            }
            "p2id" => {
                let recipient_str = self.recipient_hex.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "p2id proposal requires metadata.recipient_id".to_string(),
                    )
                })?;
                let faucet_str = self.faucet_id_hex.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "p2id proposal requires metadata.faucet_id".to_string(),
                    )
                })?;
                let parsed_amount = self.amount.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "p2id proposal requires metadata.amount".to_string(),
                    )
                })?;
                let recipient = AccountId::from_hex(recipient_str).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid recipient: {}", e))
                })?;
                let faucet_id = AccountId::from_hex(faucet_str).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid faucet_id: {}", e))
                })?;
                Ok(TransactionType::P2ID {
                    recipient,
                    faucet_id,
                    amount: parsed_amount,
                    note_type: self.p2id_note_type()?,
                    heights: P2ideHeights {
                        reclaim: self.reclaim_height,
                        timelock: self.timelock_height,
                    },
                })
            }
            "switch_guardian" => {
                let pubkey_hex = self.new_guardian_pubkey_hex.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "switch_guardian proposal requires metadata.new_guardian_pubkey"
                            .to_string(),
                    )
                })?;
                let endpoint = self.new_guardian_endpoint.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "switch_guardian proposal requires metadata.new_guardian_endpoint"
                            .to_string(),
                    )
                })?;
                let new_commitment =
                    word_from_hex(pubkey_hex).map_err(MultisigError::InvalidConfig)?;
                Ok(TransactionType::SwitchGuardian {
                    new_endpoint: endpoint.clone(),
                    new_commitment,
                })
            }
            "update_procedure_threshold" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "update_procedure_threshold proposal requires metadata.target_threshold"
                            .to_string(),
                    )
                })?;
                let procedure_name = self.target_procedure.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "update_procedure_threshold proposal requires metadata.target_procedure"
                            .to_string(),
                    )
                })?;
                let procedure = procedure_name
                    .parse()
                    .map_err(MultisigError::InvalidConfig)?;
                Ok(TransactionType::UpdateProcedureThreshold {
                    procedure,
                    new_threshold: threshold as u32,
                })
            }
            "add_signer" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "add_signer proposal requires metadata.target_threshold".to_string(),
                    )
                })?;
                let proposed_signers = self.signer_commitments()?;
                if proposed_signers.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "add_signer proposal requires metadata.signer_commitments".to_string(),
                    ));
                }
                Ok(TransactionType::UpdateSigners {
                    new_threshold: threshold as u32,
                    signer_commitments: proposed_signers,
                })
            }
            "remove_signer" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "remove_signer proposal requires metadata.target_threshold".to_string(),
                    )
                })?;
                let proposed_signers = self.signer_commitments()?;
                if proposed_signers.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "remove_signer proposal requires metadata.signer_commitments".to_string(),
                    ));
                }
                Ok(TransactionType::UpdateSigners {
                    new_threshold: threshold as u32,
                    signer_commitments: proposed_signers,
                })
            }
            "change_threshold" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "change_threshold proposal requires metadata.target_threshold".to_string(),
                    )
                })?;
                let proposed_signers = self.signer_commitments()?;
                if proposed_signers.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "change_threshold proposal requires metadata.signer_commitments"
                            .to_string(),
                    ));
                }
                Ok(TransactionType::UpdateSigners {
                    new_threshold: threshold as u32,
                    signer_commitments: proposed_signers,
                })
            }
            _ => Ok(TransactionType::Custom),
        }
    }
}

/// A proposal signature entry.
#[derive(Debug, Clone)]
pub struct ProposalSignatureEntry {
    pub signer_commitment: String,
    pub signature_hex: String,
    pub scheme: SignatureScheme,
    pub public_key_hex: Option<String>,
}

impl ProposalSignatureEntry {
    fn validate(&self) -> Result<()> {
        word_from_hex(&self.signer_commitment).map_err(MultisigError::InvalidConfig)?;

        let signature_hex = ensure_hex_prefix(&self.signature_hex);
        match self.scheme {
            SignatureScheme::Falcon => {
                Poseidon2FalconSignature::from_hex(&signature_hex).map_err(|e| {
                    MultisigError::Signature(format!("invalid proposal signature: {}", e))
                })?;
            }
            SignatureScheme::Ecdsa => {
                let signature_bytes =
                    hex::decode(signature_hex.trim_start_matches("0x")).map_err(|e| {
                        MultisigError::Signature(format!("invalid ECDSA signature hex: {}", e))
                    })?;
                EcdsaSignature::read_from_bytes(&signature_bytes).map_err(|e| {
                    MultisigError::Signature(format!(
                        "invalid ECDSA proposal signature bytes: {}",
                        e
                    ))
                })?;

                let public_key_hex = self.public_key_hex.as_ref().ok_or_else(|| {
                    MultisigError::Signature(
                        "ECDSA proposal signatures require a public key".to_string(),
                    )
                })?;
                let public_key_bytes = hex::decode(public_key_hex.trim_start_matches("0x"))
                    .map_err(|e| {
                        MultisigError::Signature(format!("invalid ECDSA public key hex: {}", e))
                    })?;
                EcdsaPublicKey::read_from_bytes(&public_key_bytes).map_err(|e| {
                    MultisigError::Signature(format!(
                        "invalid ECDSA proposal public key bytes: {}",
                        e
                    ))
                })?;
            }
        }

        Ok(())
    }
}

/// A proposal for a multisig transaction.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: String,
    pub nonce: u64,
    pub transaction_type: TransactionType,
    pub status: ProposalStatus,
    pub tx_summary: TransactionSummary,
    pub signatures: Vec<ProposalSignatureEntry>,
    pub metadata: ProposalMetadata,
}

impl Proposal {
    pub fn from(delta: &DeltaObject) -> Result<Self> {
        let payload: ProposalPayload = serde_json::from_str(&delta.delta_payload)?;

        let tx_summary = TransactionSummary::from_json(&payload.tx_summary).map_err(|e| {
            MultisigError::MidenClient(format!("failed to parse tx_summary: {}", e))
        })?;

        let metadata_payload = payload.metadata.clone().ok_or_else(|| {
            MultisigError::InvalidConfig("proposal is missing metadata".to_string())
        })?;
        let proposal_type = metadata_payload.proposal_type.clone();
        let required_signatures = metadata_payload.required_signatures.ok_or_else(|| {
            MultisigError::InvalidConfig(
                "proposal metadata.required_signatures is required".to_string(),
            )
        })?;
        let required_signatures: usize = usize::try_from(required_signatures).map_err(|_| {
            MultisigError::InvalidConfig(
                "proposal metadata.required_signatures exceeds platform limits".to_string(),
            )
        })?;

        let new_threshold = metadata_payload.target_threshold;
        let signer_commitments_hex = metadata_payload.signer_commitments;
        let salt_hex = metadata_payload.salt;
        let recipient_hex = metadata_payload.recipient_id;
        let faucet_id_hex = metadata_payload.faucet_id;
        let amount = metadata_payload.amount.as_deref().map(|value| {
            value.parse::<u64>().map_err(|e| {
                MultisigError::InvalidConfig(format!(
                    "invalid metadata.amount value '{}': {}",
                    value, e
                ))
            })
        });
        let amount = match amount {
            Some(parsed) => Some(parsed?),
            None => None,
        };
        let note_type = metadata_payload.note_type;
        let reclaim_height = metadata_payload.reclaim_height;
        let timelock_height = metadata_payload.timelock_height;
        let note_ids_hex = metadata_payload.note_ids;
        let consume_notes_metadata_version = metadata_payload.consume_notes_metadata_version;
        let consume_notes_notes = metadata_payload
            .consume_notes_notes
            .into_iter()
            .map(SerializedNote::from_base64)
            .collect();

        let new_guardian_pubkey_hex = metadata_payload.new_guardian_pubkey;
        let new_guardian_endpoint = metadata_payload.new_guardian_endpoint;
        let target_procedure = metadata_payload.target_procedure;

        let mut metadata = ProposalMetadata {
            tx_summary_json: Some(payload.tx_summary.clone()),
            proposal_type: Some(proposal_type.clone()),
            new_threshold,
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex,
            recipient_hex: recipient_hex.clone(),
            faucet_id_hex: faucet_id_hex.clone(),
            amount,
            note_type,
            reclaim_height,
            timelock_height,
            note_ids_hex: note_ids_hex.clone(),
            consume_notes_metadata_version,
            consume_notes_notes,
            new_guardian_pubkey_hex: new_guardian_pubkey_hex.clone(),
            new_guardian_endpoint: new_guardian_endpoint.clone(),
            target_procedure: target_procedure.clone(),
            required_signatures: Some(required_signatures),
            signers: Vec::new(),
            chain_anchor_b64: metadata_payload.chain_anchor,
        };
        let transaction_type = metadata.to_transaction_type(&proposal_type)?;

        let mut seen_signers = HashSet::new();
        let mut signatures = Vec::with_capacity(payload.signatures.len());
        for signature in &payload.signatures {
            let (scheme, signature_hex, public_key_hex) = match &signature.signature {
                ProposalSignature::Falcon { signature } => {
                    (SignatureScheme::Falcon, signature.clone(), None)
                }
                ProposalSignature::Ecdsa {
                    signature,
                    public_key,
                } => (
                    SignatureScheme::Ecdsa,
                    signature.clone(),
                    public_key.clone(),
                ),
            };

            let entry = ProposalSignatureEntry {
                signer_commitment: signature.signer_id.clone(),
                signature_hex,
                scheme,
                public_key_hex,
            };
            entry.validate()?;

            if !seen_signers.insert(entry.signer_commitment.to_lowercase()) {
                return Err(MultisigError::InvalidConfig(format!(
                    "duplicate proposal signature for signer {}",
                    entry.signer_commitment
                )));
            }

            metadata.signers.push(entry.signer_commitment.clone());
            signatures.push(entry);
        }

        let commitment = tx_summary.to_commitment();
        let id = format!("0x{}", hex::encode(word_to_bytes(&commitment)));

        let mut proposal = Proposal {
            id,
            nonce: delta.nonce,
            transaction_type,
            status: ProposalStatus::Pending,
            tx_summary,
            signatures,
            metadata,
        };
        proposal.refresh_status();
        Ok(proposal)
    }

    /// Creates a new Proposal
    pub fn new(
        tx_summary: TransactionSummary,
        nonce: u64,
        transaction_type: TransactionType,
        mut metadata: ProposalMetadata,
    ) -> Self {
        let commitment = tx_summary.to_commitment();
        let id = format!("0x{}", hex::encode(word_to_bytes(&commitment)));

        let signatures_required = metadata
            .required_signatures
            .unwrap_or(metadata.signer_commitments_hex.len());
        metadata
            .required_signatures
            .get_or_insert(signatures_required);
        if metadata.proposal_type.is_none() {
            metadata.proposal_type = transaction_type.proposal_type().map(str::to_string);
        }

        let mut proposal = Self {
            id,
            nonce,
            transaction_type,
            status: ProposalStatus::Pending,
            tx_summary,
            signatures: Vec::new(),
            metadata,
        };
        proposal.refresh_status();
        proposal
    }

    pub fn has_signed(&self, signer_commitment_hex: &str) -> bool {
        self.metadata
            .signers
            .iter()
            .any(|s| s.eq_ignore_ascii_case(signer_commitment_hex))
    }

    pub fn signatures_collected(&self) -> usize {
        self.metadata.signers.len()
    }

    pub fn signatures_required(&self) -> usize {
        self.metadata
            .required_signatures
            .unwrap_or(self.metadata.signer_commitments_hex.len())
    }

    pub fn signature_counts(&self) -> (usize, usize) {
        (self.signatures_collected(), self.signatures_required())
    }

    pub fn signatures_needed(&self) -> usize {
        self.signatures_required()
            .saturating_sub(self.signatures_collected())
    }

    /// Returns the commitment hex strings of signers who haven't signed yet.
    pub fn missing_signers(&self) -> Vec<String> {
        if !self.status.is_pending() {
            return Vec::new();
        }

        let signed: HashSet<_> = self
            .metadata
            .signers
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        self.metadata
            .signer_commitments_hex
            .iter()
            .filter(|c| !signed.contains(&c.to_lowercase()))
            .cloned()
            .collect()
    }

    fn refresh_status(&mut self) {
        let signatures_required = self.signatures_required();
        self.status =
            if self.metadata.signers.len() >= signatures_required && signatures_required > 0 {
                ProposalStatus::Ready
            } else {
                ProposalStatus::Pending
            };
    }
}
/// Converts a Word to bytes.
fn word_to_bytes(word: &Word) -> Vec<u8> {
    word.iter()
        .flat_map(|felt| felt.as_canonical_u64().to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use miden_protocol::Felt;

    use super::*;
    use miden_protocol::account::AccountStoragePatch;
    use miden_protocol::account::delta::{AccountDelta, AccountVaultDelta};
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummaryUserParams};

    fn create_test_tx_summary() -> TransactionSummary {
        // Use a minimal valid account ID
        let account_id = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            Felt::ZERO,
        )
        .expect("Valid empty delta");

        TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            RawOutputNotes::new(Vec::new()).unwrap(),
            Word::default(),
            0,
            TransactionSummaryUserParams::new([Felt::ZERO; 7]),
        )
    }

    #[test]
    fn test_word_from_hex_roundtrip() {
        let original = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let word = word_from_hex(original).expect("hex should decode");
        let bytes = word_to_bytes(&word);
        let result = format!("0x{}", hex::encode(bytes));
        assert_eq!(original, result);
    }

    #[test]
    fn test_word_from_hex_rejects_non_canonical_field_element() {
        let invalid = format!("0x{}{}", "ff".repeat(8), "00".repeat(24));
        let err = word_from_hex(&invalid).expect_err("non-canonical field element should fail");
        assert!(err.contains("invalid field element"));
    }

    /// A proposal without an anchor cannot be verified or executed, and a
    /// present anchor must decode as a structurally valid `ChainAnchor` —
    /// garbage base64 or well-formed base64 of non-anchor bytes are both
    /// rejected before anything executes against them.
    #[test]
    fn chain_anchor_is_required_and_validated() {
        let missing = ProposalMetadata::default();
        let err = missing
            .chain_anchor()
            .expect_err("missing anchor must fail");
        assert!(err.to_string().contains("no chain_anchor"));

        let garbage = ProposalMetadata {
            chain_anchor_b64: Some("!!!not-base64!!!".to_string()),
            ..Default::default()
        };
        let err = garbage
            .chain_anchor()
            .expect_err("garbage base64 must fail");
        assert!(err.to_string().contains("invalid chain_anchor base64"));

        let non_anchor = ProposalMetadata {
            chain_anchor_b64: Some(BASE64.encode([0xAAu8; 16])),
            ..Default::default()
        };
        let err = non_anchor
            .chain_anchor()
            .expect_err("non-anchor bytes must fail");
        assert!(err.to_string().contains("invalid chain_anchor"));
    }

    #[test]
    fn test_proposal_status_checks() {
        let pending = ProposalStatus::Pending;
        assert!(pending.is_pending());
        assert!(!pending.is_ready());

        let ready = ProposalStatus::Ready;
        assert!(ready.is_ready());
        assert!(!ready.is_pending());
    }

    #[test]
    fn test_transaction_type_transfer() {
        // Use valid Miden AccountId format
        let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let faucet_id = AccountId::from_hex("0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c").unwrap();
        let amount = 1000u64;

        let tx = TransactionType::transfer(recipient, faucet_id, amount);

        assert_eq!(
            tx,
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
                note_type: NoteType::Public,
                heights: P2ideHeights::default(),
            }
        );
    }

    #[test]
    fn test_transaction_type_consume_notes() {
        let note_id = NoteId::from_raw(Word::default());
        let tx = TransactionType::consume_notes(vec![note_id]);

        assert_eq!(
            tx,
            TransactionType::ConsumeNotes {
                note_ids: vec![note_id],
                metadata_version: None,
                notes: Vec::new(),
            }
        );
    }

    #[test]
    fn test_transaction_type_add_cosigner() {
        let commitment = Word::default();
        let tx = TransactionType::add_cosigner(commitment);

        assert_eq!(
            tx,
            TransactionType::AddCosigner {
                new_commitment: commitment
            }
        );
    }

    #[test]
    fn test_transaction_type_remove_cosigner() {
        let commitment = Word::default();
        let tx = TransactionType::remove_cosigner(commitment);

        assert_eq!(tx, TransactionType::RemoveCosigner { commitment });
    }

    #[test]
    fn test_transaction_type_switch_guardian() {
        let endpoint = "http://new-guardian.example.com";
        let commitment = Word::default();

        let tx = TransactionType::switch_guardian(endpoint, commitment);

        assert_eq!(
            tx,
            TransactionType::SwitchGuardian {
                new_endpoint: endpoint.to_string(),
                new_commitment: commitment
            }
        );
    }

    #[test]
    fn test_transaction_type_switch_guardian_rejects_non_canonical_commitment() {
        let metadata = ProposalMetadata {
            new_guardian_pubkey_hex: Some(format!("0x{}{}", "ff".repeat(8), "00".repeat(24))),
            new_guardian_endpoint: Some("http://new-guardian.example.com".to_string()),
            ..Default::default()
        };

        let err = metadata
            .to_transaction_type("switch_guardian")
            .expect_err("non-canonical GUARDIAN commitment should be rejected");
        assert!(err.to_string().contains("invalid field element"));
    }

    #[test]
    fn test_transaction_type_update_signers() {
        let threshold = 2u32;
        let signers = vec![Word::default()];

        let tx = TransactionType::update_signers(threshold, signers.clone());

        assert_eq!(
            tx,
            TransactionType::UpdateSigners {
                new_threshold: threshold,
                signer_commitments: signers
            }
        );
    }

    #[test]
    fn test_transaction_type_requires_guardian_ack() {
        let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let faucet_id = AccountId::from_hex("0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c").unwrap();

        assert!(TransactionType::transfer(recipient, faucet_id, 1).requires_guardian_ack());
        assert!(
            !TransactionType::switch_guardian("http://new-guardian.example.com", Word::default())
                .requires_guardian_ack()
        );
    }

    #[test]
    fn test_transaction_type_supports_offline_execution() {
        let note_id = NoteId::from_raw(Word::default());
        assert!(!TransactionType::consume_notes(vec![note_id]).supports_offline_execution());
        assert!(
            TransactionType::switch_guardian("http://new-guardian.example.com", Word::default())
                .supports_offline_execution()
        );
    }

    #[test]
    fn test_proposal_signature_counts() {
        let pending = ProposalStatus::Pending;

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: pending,
            tx_summary: create_test_tx_summary(),
            signatures: Vec::new(),
            metadata: ProposalMetadata {
                required_signatures: Some(3),
                signers: vec!["0xabc".to_string()],
                signer_commitments_hex: vec![
                    "0xabc".to_string(),
                    "0xdef".to_string(),
                    "0x123".to_string(),
                ],
                ..Default::default()
            },
        };

        assert_eq!(proposal.signature_counts(), (1, 3));
        assert_eq!(proposal.signatures_needed(), 2);
    }

    #[test]
    fn test_proposal_missing_signers() {
        let pending = ProposalStatus::Pending;

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: pending,
            tx_summary: create_test_tx_summary(),
            signatures: Vec::new(),
            metadata: ProposalMetadata {
                signers: vec!["0xABC".to_string()], // uppercase to test case-insensitivity
                signer_commitments_hex: vec![
                    "0xabc".to_string(), // lowercase
                    "0xdef".to_string(),
                    "0x456".to_string(),
                ],
                ..Default::default()
            },
        };

        let missing = proposal.missing_signers();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&"0xdef".to_string()));
        assert!(missing.contains(&"0x456".to_string()));
        // 0xabc should NOT be in missing (already signed)
        assert!(!missing.contains(&"0xabc".to_string()));
    }

    #[test]
    fn test_proposal_signatures_needed_when_ready() {
        let ready = ProposalStatus::Ready;

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: ready,
            tx_summary: create_test_tx_summary(),
            signatures: Vec::new(),
            metadata: ProposalMetadata {
                required_signatures: Some(2),
                signers: vec!["0xabc".to_string(), "0xdef".to_string()],
                ..Default::default()
            },
        };

        assert_eq!(proposal.signatures_needed(), 0);
    }

    // ==================== US1 dispatch wiring tests (issue #229) ====================

    /// The v1 `consume_notes` constructor builds a TransactionType that
    /// the dispatch will route through the legacy local-store path.
    #[test]
    fn transaction_type_consume_notes_legacy_constructor_marks_v1() {
        let note_id = NoteId::from_raw(Word::default());
        let tx = TransactionType::consume_notes(vec![note_id]);
        match tx {
            TransactionType::ConsumeNotes {
                metadata_version,
                notes,
                ..
            } => {
                assert!(
                    metadata_version.is_none(),
                    "legacy constructor must not stamp a version"
                );
                assert!(notes.is_empty(), "legacy constructor must not embed notes");
            }
            other => panic!("expected ConsumeNotes, got {:?}", other),
        }
    }

    /// The v2 `consume_notes_v2` constructor stamps the discriminator
    /// and carries the embedded notes so the dispatch routes through
    /// the self-contained rebuild path.
    #[test]
    fn transaction_type_consume_notes_v2_constructor_stamps_version_and_notes() {
        let note_id = NoteId::from_raw(Word::default());
        let embedded = vec![SerializedNote::from_base64("YmFzZTY0Tm90ZQ==".to_string())];
        let tx = TransactionType::consume_notes_v2(vec![note_id], embedded.clone());
        match tx {
            TransactionType::ConsumeNotes {
                metadata_version,
                notes,
                ..
            } => {
                assert_eq!(metadata_version, Some(CONSUME_NOTES_METADATA_VERSION_V2));
                assert_eq!(notes, embedded);
            }
            other => panic!("expected ConsumeNotes, got {:?}", other),
        }
    }

    /// `ProposalMetadata::to_transaction_type` threads the v2 discriminator
    /// and embedded notes from wire metadata into the runtime
    /// TransactionType so the dispatch in `execution.rs` sees them.
    /// This is the wire→runtime bridge that the foundational tests in
    /// `payload.rs` exercise at the JSON layer.
    #[test]
    fn to_transaction_type_threads_v2_metadata() {
        let note_id_hex =
            "0x0100000000000000000000000000000000000000000000000000000000000000".to_string();
        let metadata = ProposalMetadata {
            note_ids_hex: vec![note_id_hex],
            consume_notes_metadata_version: Some(CONSUME_NOTES_METADATA_VERSION_V2),
            consume_notes_notes: vec![SerializedNote::from_base64("YmFzZTY0Tm90ZQ==".to_string())],
            ..Default::default()
        };

        let tx = metadata
            .to_transaction_type("consume_notes")
            .expect("to_transaction_type");

        match tx {
            TransactionType::ConsumeNotes {
                metadata_version,
                notes,
                ..
            } => {
                assert_eq!(metadata_version, Some(CONSUME_NOTES_METADATA_VERSION_V2));
                assert_eq!(notes.len(), 1);
                assert_eq!(notes[0].as_str(), "YmFzZTY0Tm90ZQ==");
            }
            other => panic!("expected ConsumeNotes, got {:?}", other),
        }
    }

    // ==================== SerializedNote tests (issue #229) ====================

    /// `SerializedNote` serializes transparently as a string (the base64
    /// of a Miden `Note`'s byte serialization). This is the wire format
    /// consumed by the v2 dispatch in US1.
    #[test]
    fn serialized_note_is_transparent_string_on_wire() {
        let sn = SerializedNote::from_base64("YmFzZTY0LWVuY29kZWQtbm90ZQ==".to_string());
        let json = serde_json::to_value(&sn).unwrap();
        assert_eq!(
            json,
            serde_json::Value::String("YmFzZTY0LWVuY29kZWQtbm90ZQ==".to_string())
        );

        let parsed: SerializedNote =
            serde_json::from_str(r#""YmFzZTY0LWVuY29kZWQtbm90ZQ==""#).unwrap();
        assert_eq!(parsed.0, sn.0);
    }

    /// Decoding garbage base64 yields a clean error, not a panic.
    #[test]
    fn serialized_note_rejects_invalid_base64() {
        let sn = SerializedNote::from_base64("@@@not-base64@@@".to_string());
        let err = sn.to_note().unwrap_err();
        assert!(matches!(err, MultisigError::InvalidConfig(_)));
    }

    /// `MAX_CONSUME_NOTES_METADATA_BYTES` is the 256 KiB limit from
    /// research.md Decision 4 — pinning the value so future changes
    /// are visible in test diffs.
    #[test]
    fn max_consume_notes_metadata_bytes_is_256_kib() {
        assert_eq!(MAX_CONSUME_NOTES_METADATA_BYTES, 256 * 1024);
        assert_eq!(MAX_CONSUME_NOTES_METADATA_BYTES, 262_144);
    }

    /// v1/v2 discriminator helpers on the internal `ProposalMetadata`
    /// struct: absence and `Some(1)` both signal v1; `Some(2)` is v2.
    #[test]
    fn proposal_metadata_consume_notes_version_helpers() {
        let v1_absent = ProposalMetadata::default();
        assert!(v1_absent.is_consume_notes_v1());
        assert!(!v1_absent.is_consume_notes_v2());

        let v1_explicit = ProposalMetadata {
            consume_notes_metadata_version: Some(1),
            ..Default::default()
        };
        assert!(v1_explicit.is_consume_notes_v1());
        assert!(!v1_explicit.is_consume_notes_v2());

        let v2 = ProposalMetadata {
            consume_notes_metadata_version: Some(CONSUME_NOTES_METADATA_VERSION_V2),
            ..Default::default()
        };
        assert!(v2.is_consume_notes_v2());
        assert!(!v2.is_consume_notes_v1());

        // Unknown future version is neither v1 nor v2; dispatch handles
        // rejection per spec FR-009.
        let unknown = ProposalMetadata {
            consume_notes_metadata_version: Some(99),
            ..Default::default()
        };
        assert!(!unknown.is_consume_notes_v1());
        assert!(!unknown.is_consume_notes_v2());
    }

    // ==================== ProposalMetadata parser tests ====================

    #[test]
    fn test_metadata_salt_valid() {
        let metadata = ProposalMetadata {
            salt_hex: Some(
                "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".to_string(),
            ),
            ..Default::default()
        };

        let salt = metadata.salt().expect("salt should parse");
        // Verify it's not the default Word
        assert_ne!(salt, Word::default());
    }

    #[test]
    fn test_metadata_salt_rejects_non_canonical_field_element() {
        let metadata = ProposalMetadata {
            salt_hex: Some(format!("0x{}{}", "ff".repeat(8), "00".repeat(24))),
            ..Default::default()
        };

        let err = metadata
            .salt()
            .expect_err("non-canonical salt should be rejected");
        assert!(err.to_string().contains("invalid field element"));
    }

    #[test]
    fn test_metadata_salt_none_is_an_error() {
        // It used to answer `Word::default()`. The request now declares this salt and
        // miden-client commits `hash(CONVERSION_INFO || SALT)` from it, so a substituted
        // zero would be committed as readily as the real one and reproduce a summary no
        // cosigner ever signed. Absent has no correct answer; say so.
        let metadata = ProposalMetadata::default();

        let error = metadata.salt().expect_err("an absent salt has no default");
        assert!(
            error.to_string().contains("no salt"),
            "the error must name the missing salt, got: {error}"
        );
    }

    #[test]
    fn test_metadata_signer_commitments_valid() {
        let hex1 = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let hex2 = "0x2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40";

        let metadata = ProposalMetadata {
            signer_commitments_hex: vec![hex1.to_string(), hex2.to_string()],
            ..Default::default()
        };

        let commitments = metadata.signer_commitments().expect("should parse");
        assert_eq!(commitments.len(), 2);
    }

    #[test]
    fn test_metadata_signer_commitments_invalid_hex() {
        let metadata = ProposalMetadata {
            signer_commitments_hex: vec!["not_valid_hex".to_string()],
            ..Default::default()
        };

        assert!(metadata.signer_commitments().is_err());
    }

    #[test]
    fn test_metadata_note_ids_valid() {
        // NoteId is 32 bytes = 64 hex chars
        let note_hex = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

        let metadata = ProposalMetadata {
            note_ids_hex: vec![note_hex.to_string()],
            ..Default::default()
        };

        let note_ids = metadata.note_ids().expect("should parse");
        assert_eq!(note_ids.len(), 1);
    }

    #[test]
    fn to_transaction_type_maps_unmodeled_label_to_custom() {
        let metadata = ProposalMetadata::default();

        let tx_type = metadata
            .to_transaction_type("b2agg")
            .expect("unmodeled proposal type should map to Custom");

        assert_eq!(tx_type, TransactionType::Custom);
        assert_eq!(tx_type.type_name(), "Custom");
        assert_eq!(tx_type.proposal_type(), None);
    }

    #[test]
    fn to_transaction_type_still_rejects_empty_label() {
        let metadata = ProposalMetadata::default();

        let err = metadata
            .to_transaction_type("")
            .expect_err("empty proposal type must be rejected");
        assert!(err.to_string().contains("proposal_type is required"));
    }

    // ---------- p2id note_type (issue #322) ----------

    fn p2id_metadata(note_type: Option<&str>) -> ProposalMetadata {
        ProposalMetadata {
            recipient_hex: Some("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string()),
            faucet_id_hex: Some("0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c".to_string()),
            amount: Some(1000),
            note_type: note_type.map(str::to_string),
            ..Default::default()
        }
    }

    /// Absent `note_type` must keep mapping to a public note — the only
    /// behavior that existed before the field, so pre-#322 proposals
    /// rebuild identically.
    #[test]
    fn to_transaction_type_p2id_defaults_to_public_note() {
        let tx_type = p2id_metadata(None)
            .to_transaction_type("p2id")
            .expect("to_transaction_type");
        assert!(matches!(
            tx_type,
            TransactionType::P2ID {
                note_type: NoteType::Public,
                ..
            }
        ));
    }

    #[test]
    fn to_transaction_type_p2id_threads_private_note_type() {
        let tx_type = p2id_metadata(Some("private"))
            .to_transaction_type("p2id")
            .expect("to_transaction_type");
        assert!(matches!(
            tx_type,
            TransactionType::P2ID {
                note_type: NoteType::Private,
                ..
            }
        ));
    }

    /// An unknown `note_type` must be rejected, not silently rebuilt as a
    /// public note that could never match the signed tx_summary commitment.
    #[test]
    fn to_transaction_type_p2id_rejects_unknown_note_type() {
        let err = p2id_metadata(Some("encrypted"))
            .to_transaction_type("p2id")
            .expect_err("unknown note_type must be rejected");
        assert!(err.to_string().contains("unsupported metadata.note_type"));
    }

    /// Absent heights must keep mapping to a plain P2ID note — the only
    /// behavior that existed before the fields, so pre-#366 proposals
    /// rebuild identically (issue #366).
    #[test]
    fn to_transaction_type_p2id_defaults_to_no_heights() {
        let tx_type = p2id_metadata(None)
            .to_transaction_type("p2id")
            .expect("to_transaction_type");
        assert!(matches!(
            tx_type,
            TransactionType::P2ID {
                heights: P2ideHeights {
                    reclaim: None,
                    timelock: None,
                },
                ..
            }
        ));
    }

    #[test]
    fn to_transaction_type_p2id_threads_p2ide_heights() {
        let metadata = ProposalMetadata {
            reclaim_height: NonZeroU32::new(12345),
            timelock_height: NonZeroU32::new(700),
            ..p2id_metadata(None)
        };
        let tx_type = metadata
            .to_transaction_type("p2id")
            .expect("to_transaction_type");
        let TransactionType::P2ID { heights, .. } = tx_type else {
            panic!("expected a P2ID transaction");
        };
        assert_eq!(heights.reclaim, NonZeroU32::new(12345));
        assert_eq!(heights.timelock, NonZeroU32::new(700));
    }

    #[test]
    fn builtin_proposal_types_are_recognized() {
        for label in [
            "add_signer",
            "remove_signer",
            "change_threshold",
            "update_procedure_threshold",
            "switch_guardian",
            "consume_notes",
            "p2id",
            "custom",
        ] {
            assert!(
                is_builtin_proposal_type(label),
                "{label} should be reserved"
            );
        }
    }

    #[test]
    fn custom_labels_are_not_builtin() {
        assert!(!is_builtin_proposal_type("b2agg"));
        assert!(!is_builtin_proposal_type(""));
        assert!(!is_builtin_proposal_type("P2ID"));
    }
}
