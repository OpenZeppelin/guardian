//! Export/import operations for MultisigClient.
//!
//! This module handles exporting proposals to files/strings and
//! importing them back for offline sharing workflows, as well as
//! exporting/importing note files for out-of-band note transfer
//! (issue #356).

use guardian_client::delta_status::Status;
use guardian_shared::SignatureScheme;
use miden_client::note::NoteFile;
use miden_client::store::NoteExportType;
use miden_protocol::note::NoteId;
use miden_protocol::utils::serde::{Deserializable, Serializable};

use super::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::export::{ExportedProposal, ExportedSignature};

impl MultisigClient {
    /// Exports a proposal to a file for offline sharing.
    ///
    /// This fetches the proposal from GUARDIAN, including all collected signatures,
    /// and writes it to the specified file path as JSON.
    ///
    /// # Example
    ///
    /// ```ignore
    /// client.export_proposal(&proposal_id, "/tmp/proposal.json").await?;
    /// ```
    pub async fn export_proposal(
        &mut self,
        proposal_id: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        let exported = self.export_proposal_to_exported(proposal_id).await?;
        let json = exported.to_json()?;
        std::fs::write(path, json)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to write file: {}", e)))?;
        Ok(())
    }

    /// Exports a proposal to a JSON string for programmatic use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let json = client.export_proposal_to_string(&proposal_id).await?;
    /// println!("{}", json);
    /// ```
    pub async fn export_proposal_to_string(&mut self, proposal_id: &str) -> Result<String> {
        let exported = self.export_proposal_to_exported(proposal_id).await?;
        exported.to_json()
    }

    /// Internal helper to create an ExportedProposal from GUARDIAN data.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The proposal is not found in GUARDIAN
    /// - The raw delta cannot be found in GUARDIAN response
    /// - The delta has no pending status with signature data
    async fn export_proposal_to_exported(&mut self, proposal_id: &str) -> Result<ExportedProposal> {
        let account = self.require_account()?.clone();
        let account_id = account.id();
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let response = guardian_client
            .get_delta_proposal(&account_id, proposal_id)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("failed to get proposal: {}", e)))?;
        let raw_proposal = response
            .proposal
            .as_ref()
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;
        Self::ensure_proposal_account_id(&raw_proposal.account_id, &account_id)?;
        let proposal = crate::proposal::Proposal::from(raw_proposal)?;
        self.verify_proposal_summary_binding(&proposal).await?;

        // Extract signatures - fail if status structure is missing
        let status = raw_proposal.status.as_ref().ok_or_else(|| {
            MultisigError::GuardianServer(format!("proposal {} has no status field", proposal_id))
        })?;

        let status_oneof = status.status.as_ref().ok_or_else(|| {
            MultisigError::GuardianServer(format!("proposal {} has empty status", proposal_id))
        })?;

        let pending = match status_oneof {
            Status::Pending(p) => p,
            _ => {
                return Err(MultisigError::GuardianServer(format!(
                    "proposal {} is not in pending state",
                    proposal_id
                )));
            }
        };

        let mut signatures = Vec::new();
        for cosigner_sig in pending.cosigner_sigs.iter() {
            if let Some(ref sig) = cosigner_sig.signature {
                let scheme = if sig.scheme.eq_ignore_ascii_case("ecdsa") {
                    SignatureScheme::Ecdsa
                } else {
                    SignatureScheme::Falcon
                };
                signatures.push(ExportedSignature {
                    signer_commitment: cosigner_sig.signer_id.clone(),
                    signature: sig.signature.clone(),
                    scheme,
                    public_key_hex: sig.public_key.clone(),
                });
            }
        }

        let exported =
            ExportedProposal::from_proposal(&proposal, account_id)?.with_signatures(signatures);

        Ok(exported)
    }

    /// Imports a proposal from a file.
    ///
    /// The proposal can then be signed with `sign_imported_proposal`
    /// or executed with `execute_imported_proposal`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal("/tmp/proposal.json").await?;
    /// println!("Imported proposal: {}", proposal.id);
    /// ```
    pub async fn import_proposal(&mut self, path: &std::path::Path) -> Result<ExportedProposal> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to read file: {}", e)))?;
        self.import_proposal_from_string(&json).await
    }

    /// Imports a proposal from a JSON string.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal_from_string(&json).await?;
    /// ```
    pub async fn import_proposal_from_string(&mut self, json: &str) -> Result<ExportedProposal> {
        let exported = ExportedProposal::from_json(json)?;
        exported.validate(self.account.as_ref().map(|account| account.id()))?;

        let proposal = exported.to_proposal()?;
        self.verify_proposal_summary_binding(&proposal).await?;

        Ok(exported)
    }

    /// Exports a note created by this account to a file for out-of-band
    /// delivery (issue #356).
    ///
    /// A private note publishes only its commitment on chain, so the recipient
    /// can never learn its contents via sync; the sender must hand them the
    /// note file produced here, which they load with
    /// [`Self::import_note_from_file`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// client.export_note_to_file(&note_id_hex, Path::new("note.mno")).await?;
    /// ```
    pub async fn export_note_to_file(&self, note_id: &str, path: &std::path::Path) -> Result<()> {
        let bytes = self.export_note_to_bytes(note_id).await?;
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to write file: {}", e)))?;
        Ok(())
    }

    /// Exports a note created by this account to serialized `NoteFile` bytes
    /// for programmatic out-of-band delivery (issue #356).
    ///
    /// The note must be an output note of this client (i.e. created by a
    /// transaction this client executed). When the note's on-chain inclusion
    /// proof is already known (after a post-commit sync) the full note with
    /// proof is exported; otherwise the note details are exported and the
    /// importer's client tracks the note until it commits on chain.
    pub async fn export_note_to_bytes(&self, note_id: &str) -> Result<Vec<u8>> {
        let note_id = NoteId::try_from_hex(note_id.trim())
            .map_err(|e| MultisigError::InvalidConfig(format!("invalid note id: {}", e)))?;

        let record = self
            .miden_client
            .get_output_note(note_id)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to get output note: {}", e)))?
            .ok_or_else(|| {
                MultisigError::MidenClient(format!(
                    "output note {} not found in the local store; only notes created by \
                     this client can be exported",
                    note_id.to_hex()
                ))
            })?;

        let export_type = if record.inclusion_proof().is_some() {
            NoteExportType::NoteWithProof
        } else {
            NoteExportType::NoteDetails
        };

        let note_file = record.into_note_file(&export_type).map_err(|e| {
            MultisigError::MidenClient(format!("failed to convert note for export: {}", e))
        })?;

        Ok(note_file.to_bytes())
    }

    /// Imports a note file received out-of-band (issue #356) so the note can
    /// be consumed by this account.
    ///
    /// Returns the note ID when the file carries one (full note with proof or
    /// ID-only file), or the note's details commitment for a details-only
    /// file. Sync afterwards so the note's on-chain commitment is tracked and
    /// the note shows up in [`Self::list_consumable_notes`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// let note_id = client.import_note_from_file(Path::new("note.mno")).await?;
    /// client.sync().await?;
    /// ```
    pub async fn import_note_from_file(&mut self, path: &std::path::Path) -> Result<String> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to read file: {}", e)))?;
        self.import_note_from_bytes(&bytes).await
    }

    /// Imports a note from serialized `NoteFile` bytes (issue #356).
    ///
    /// See [`Self::import_note_from_file`] for the returned identifier
    /// semantics.
    pub async fn import_note_from_bytes(&mut self, bytes: &[u8]) -> Result<String> {
        let note_file = NoteFile::read_from_bytes(bytes).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to decode note file: {}", e))
        })?;

        let known_id = note_file_note_id(&note_file);

        let commitments = self
            .miden_client
            .import_notes(std::slice::from_ref(&note_file))
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to import note: {}", e)))?;

        match known_id {
            Some(id) => Ok(id),
            None => commitments
                .first()
                .map(|c| format!("0x{}", hex::encode(c.to_bytes())))
                .ok_or_else(|| {
                    MultisigError::MidenClient("note import reported no imported notes".to_string())
                }),
        }
    }
}

/// Returns the note ID a note file resolves to, when it carries one. A
/// details-only file has no metadata and therefore no note ID yet.
fn note_file_note_id(note_file: &NoteFile) -> Option<String> {
    match note_file {
        NoteFile::NoteId(id) => Some(id.to_hex()),
        NoteFile::NoteWithProof(note, _) => Some(note.id().to_hex()),
        NoteFile::NoteDetails { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::Word;
    use miden_protocol::account::AccountId;
    use miden_protocol::block::BlockNumber;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::{Note, NoteType};
    use miden_standards::note::P2idNote;

    use super::*;

    fn build_test_note() -> Note {
        let sender = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let target = AccountId::from_hex("0x1b1b1b1a1b1b1b011b1b1b1b1b1b1b").unwrap();
        let mut rng = RandomCoin::new(Word::default());
        P2idNote::create(
            sender,
            target,
            vec![],
            NoteType::Private,
            Default::default(),
            &mut rng,
        )
        .unwrap()
    }

    #[test]
    fn note_file_note_id_by_variant() {
        let note = build_test_note();
        let expected = note.id().to_hex();

        // An ID-only file resolves to the note ID.
        let id_file = NoteFile::NoteId(note.id());
        assert_eq!(note_file_note_id(&id_file), Some(expected));

        // A details-only file has no note ID yet.
        let details_file = NoteFile::NoteDetails {
            details: note.into(),
            after_block_num: BlockNumber::from(0u32),
            tag: None,
        };
        assert_eq!(note_file_note_id(&details_file), None);
    }

    #[test]
    fn note_file_roundtrips_through_bytes() {
        let note = build_test_note();
        let file = NoteFile::NoteDetails {
            details: note.into(),
            after_block_num: BlockNumber::from(7u32),
            tag: None,
        };

        let bytes = file.to_bytes();
        let decoded = NoteFile::read_from_bytes(&bytes).unwrap();
        match decoded {
            NoteFile::NoteDetails {
                after_block_num, ..
            } => assert_eq!(after_block_num, BlockNumber::from(7u32)),
            _ => panic!("expected details variant"),
        }

        assert!(NoteFile::read_from_bytes(b"not a note file").is_err());
    }
}
