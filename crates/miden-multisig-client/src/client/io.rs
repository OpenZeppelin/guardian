//! Export/import operations for MultisigClient.
//!
//! This module handles exporting proposals to files/strings and
//! importing them back for offline sharing workflows, as well as
//! exporting/importing note files for out-of-band note transfer
//! (issue #356).

use guardian_client::delta_status::Status;
use guardian_shared::{EcdsaMessageFormat, SignatureScheme};
use miden_client::note::{NoteFile, NoteSyncHint};
use miden_client::store::{NoteExportType, NoteFilter, OutputNoteRecord};
use miden_protocol::note::NoteId;
use miden_protocol::utils::serde::Serializable;

use super::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::export::{ExportedProposal, ExportedSignature};
use crate::proposal::Proposal;

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
        let mut proposal = crate::proposal::Proposal::from(raw_proposal)?;
        self.verify_proposal_summary_binding(&mut proposal).await?;

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
                    message_format: match sig.message_format.as_str() {
                        "eip712" => EcdsaMessageFormat::Eip712,
                        "" | "raw" => EcdsaMessageFormat::Raw,
                        other => {
                            return Err(MultisigError::Signature(format!(
                                "unknown signature format: {other}"
                            )));
                        }
                    },
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

        let mut proposal = exported.to_proposal()?;
        self.verify_proposal_summary_binding(&mut proposal).await?;
        self.known_proposals
            .record(exported.account_id()?, &proposal);

        Ok(exported)
    }

    /// Exports the note a P2ID proposal creates to a file for out-of-band
    /// delivery, whoever executed the proposal: this client or GUARDIAN.
    ///
    /// A private note publishes only its commitment on chain, so this file is
    /// how its recipient learns it; they load it with
    /// [`Self::import_note_from_file`]. See
    /// [`Self::export_p2id_note_to_bytes`].
    pub async fn export_p2id_note_to_file(
        &self,
        proposal: &Proposal,
        path: &std::path::Path,
    ) -> Result<()> {
        let bytes = self.export_p2id_note_to_bytes(proposal).await?;
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to write file: {}", e)))?;
        Ok(())
    }

    /// Exports the note a P2ID proposal creates as serialized `NoteFile`
    /// bytes, whoever executed the proposal: this client or GUARDIAN.
    ///
    /// The note is rebuilt from the proposal and must be one the summary the
    /// proposal id commits to creates. When the node holds it, the file carries
    /// the note with its inclusion proof; before the transaction commits it
    /// carries the note details, and the importer's client tracks the note
    /// from the block the summary binds until it commits.
    ///
    /// GUARDIAN deletes a proposal once its transaction is canonical, so keep
    /// the proposal, or its offline export, from before it is executed.
    pub async fn export_p2id_note_to_bytes(&self, proposal: &Proposal) -> Result<Vec<u8>> {
        let summary_commitment =
            crate::transaction::word_to_hex(&proposal.tx_summary.to_commitment());
        if !summary_commitment
            .trim_start_matches("0x")
            .eq_ignore_ascii_case(proposal.id.trim_start_matches("0x"))
        {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal {} carries a summary committing to {summary_commitment}",
                proposal.id
            )));
        }
        let note = self.p2id_note(proposal)?;
        if !proposal
            .tx_summary
            .output_notes()
            .iter()
            .any(|created| created.id() == note.id())
        {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal {} does not create note {}: its metadata does not reproduce its signed \
                 transaction",
                proposal.id,
                note.id().to_hex()
            )));
        }
        let fetched = self
            .node_rpc_client()
            .get_notes_by_id(&[note.id()])
            .await
            .map_err(|e| MultisigError::miden_rpc_with_context("failed to fetch the note", e))?;
        let note_file = match fetched.first() {
            Some(fetched) => NoteFile::Committed {
                proof: fetched.inclusion_proof().clone(),
                note,
            },
            None => NoteFile::ExpectedNote {
                sync_hint: NoteSyncHint::new(
                    proposal.tx_summary.block_number(),
                    note.metadata().tag(),
                ),
                details: note.into(),
            },
        };
        Ok(note_file.to_bytes())
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

        let record = self.output_note_record(note_id).await?.ok_or_else(|| {
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

    /// The output note `note_id` from the current store, or from a store a
    /// rollback or reset replaced this session.
    async fn output_note_record(&self, note_id: NoteId) -> Result<Option<OutputNoteRecord>> {
        if let Some(record) = self
            .miden_client
            .get_output_note(note_id)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to get output note: {}", e)))?
        {
            return Ok(Some(record));
        }
        for store in &self.replaced_stores {
            let found = store
                .get_output_notes(NoteFilter::Unique(note_id))
                .await
                .map_err(|e| {
                    MultisigError::MidenClient(format!(
                        "failed to get output note from a replaced store: {}",
                        e
                    ))
                })?;
            if let Some(record) = found.into_iter().next() {
                return Ok(Some(record));
            }
        }
        Ok(None)
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
        let note_file = NoteFile::try_from_bytes(bytes).map_err(|e| {
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
        NoteFile::Committed { note, .. } => Some(note.id().to_hex()),
        NoteFile::ExpectedNote { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use miden_client::note::NoteSyncHint;
    use miden_protocol::Word;
    use miden_protocol::account::AccountId;
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::block::BlockNumber;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::{Note, NoteType};
    use miden_standards::note::P2idNote;

    use super::*;

    fn build_test_note() -> Note {
        let sender = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let target = AccountId::from_hex("0x1b1b1b1a1b1b1b011b1b1b1b1b1b1b").unwrap();
        let mut rng = RandomCoin::new(Word::default());
        P2idNote::builder()
            .sender(sender)
            .target(target)
            .asset(FungibleAsset::mock(1))
            .note_type(NoteType::Private)
            .generate_serial_number(&mut rng)
            .build()
            .unwrap()
            .into()
    }

    mod p2id_export {
        use miden_protocol::account::delta::{AccountDelta, AccountVaultDelta};
        use miden_protocol::account::{AccountCodePatch, AccountStoragePatch};
        use miden_protocol::transaction::{
            InputNotes, RawOutputNote, RawOutputNotes, TransactionSummary,
            TransactionSummaryUserParams,
        };
        use miden_protocol::{Felt, Word};

        use crate::account::MultisigAccount;
        use crate::client::test_support::{
            chain_with_notes, multisig_account, offline_client_with_node,
        };
        use crate::proposal::{P2ideHeights, ProposalMetadata, TransactionType};

        use super::*;

        async fn client(dir: &std::path::Path) -> MultisigClient {
            let mut client = offline_client_with_node(dir, chain_with_notes(vec![])).await;
            client.account = Some(MultisigAccount::new(multisig_account(
                Word::from([1u32, 2, 3, 4]),
                Word::from([9u32, 9, 9, 9]),
                61,
            )));
            client
        }

        fn summary_creating(account_id: AccountId, notes: Vec<Note>) -> TransactionSummary {
            TransactionSummary::new(
                AccountDelta::new(
                    account_id,
                    AccountStoragePatch::default(),
                    AccountVaultDelta::default(),
                    AccountCodePatch::default(),
                    Felt::ZERO,
                )
                .unwrap(),
                InputNotes::new(Vec::new()).unwrap(),
                RawOutputNotes::new(notes.into_iter().map(RawOutputNote::Full).collect()).unwrap(),
                BlockNumber::from(1u32),
                Word::default(),
                0,
                TransactionSummaryUserParams::new([Felt::ZERO; 6]),
            )
        }

        /// A private P2ID proposal of `client`'s account, with a summary creating its note
        /// when `creates_its_note`.
        fn p2id_proposal(client: &MultisigClient, creates_its_note: bool) -> (Proposal, Note) {
            let account_id = client.account.as_ref().unwrap().id();
            let target = AccountId::from_hex("0x1b1b1b1a1b1b1b011b1b1b1b1b1b1b").unwrap();
            let faucet = AccountId::from_hex("0x3f3f3f3e3f3f3f013f3f3f3f3f3f3f").unwrap();
            let build = |summary: TransactionSummary| {
                Proposal::new(
                    summary,
                    1,
                    TransactionType::P2ID {
                        recipient: target,
                        faucet_id: faucet,
                        amount: 5,
                        note_type: NoteType::Private,
                        heights: P2ideHeights::default(),
                    },
                    ProposalMetadata {
                        salt_hex: Some(crate::transaction::word_to_hex(&Word::from([
                            7u32, 7, 7, 7,
                        ]))),
                        ..Default::default()
                    },
                )
            };
            let without_note = build(summary_creating(account_id, vec![]));
            let note = client.p2id_note(&without_note).unwrap();
            if creates_its_note {
                (
                    build(summary_creating(account_id, vec![note.clone()])),
                    note,
                )
            } else {
                (without_note, note)
            }
        }

        #[tokio::test]
        async fn a_note_the_chain_holds_is_exported_with_its_proof() {
            let dir = tempfile::tempdir().unwrap();
            let mut client = client(dir.path()).await;
            let (proposal, note) = p2id_proposal(&client, true);
            client.set_node_rpc_client(chain_with_notes(vec![RawOutputNote::Full(note.clone())]));

            let bytes = client.export_p2id_note_to_bytes(&proposal).await.unwrap();
            match NoteFile::try_from_bytes(&bytes).unwrap() {
                NoteFile::Committed { note: exported, .. } => assert_eq!(exported, note),
                other => panic!("expected a committed note, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn a_note_not_yet_on_chain_is_exported_as_its_details() {
            let dir = tempfile::tempdir().unwrap();
            let client = client(dir.path()).await;
            let (proposal, note) = p2id_proposal(&client, true);

            let bytes = client.export_p2id_note_to_bytes(&proposal).await.unwrap();
            match NoteFile::try_from_bytes(&bytes).unwrap() {
                NoteFile::ExpectedNote { details, sync_hint } => {
                    assert_eq!(details.commitment(), note.details_commitment());
                    assert_eq!(sync_hint.tag(), note.metadata().tag());
                    assert_eq!(sync_hint.after_block_num(), BlockNumber::from(1u32));
                }
                other => panic!("expected the note's details, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn a_note_the_signed_transaction_does_not_create_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let client = client(dir.path()).await;
            let (proposal, note) = p2id_proposal(&client, false);

            let error = client
                .export_p2id_note_to_bytes(&proposal)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(&note.id().to_hex()), "{error}");
        }

        #[derive(serde::Deserialize, serde::Serialize)]
        struct NoteFileVector {
            sender: String,
            recipient: String,
            faucet: String,
            amount: u64,
            note_type: String,
            salt: String,
            after_block: u32,
            note_id: String,
            note_file_hex: String,
        }

        fn note_file_vector_path() -> std::path::PathBuf {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/miden-multisig-client/p2id-note-file-vectors.json")
        }

        /// The note file a private P2ID exports before its transaction commits, pinned for the
        /// TypeScript SDK's `exportP2idNoteToBytes` (`tests/p2id-note-file-vectors.test.ts`).
        #[tokio::test]
        async fn a_note_file_matches_the_shared_vector() {
            let dir = tempfile::tempdir().unwrap();
            let client = client(dir.path()).await;
            let (proposal, note) = p2id_proposal(&client, true);
            let bytes = client.export_p2id_note_to_bytes(&proposal).await.unwrap();
            let TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
                ..
            } = proposal.transaction_type
            else {
                unreachable!("p2id_proposal builds a P2ID proposal");
            };
            let actual = NoteFileVector {
                sender: client.account.as_ref().unwrap().id().to_hex(),
                recipient: recipient.to_hex(),
                faucet: faucet_id.to_hex(),
                amount,
                note_type: "private".to_string(),
                salt: proposal.metadata.salt_hex.clone().unwrap(),
                after_block: 1,
                note_id: note.id().to_hex(),
                note_file_hex: hex::encode(bytes),
            };
            if std::env::var_os("WRITE_NOTE_FILE_VECTOR").is_some() {
                std::fs::write(
                    note_file_vector_path(),
                    serde_json::to_string_pretty(&vec![actual]).unwrap() + "\n",
                )
                .unwrap();
                return;
            }
            let pinned: Vec<NoteFileVector> =
                serde_json::from_str(&std::fs::read_to_string(note_file_vector_path()).unwrap())
                    .unwrap();
            assert_eq!(
                serde_json::to_value(&pinned[0]).unwrap(),
                serde_json::to_value(&actual).unwrap()
            );
        }

        #[tokio::test]
        async fn a_summary_the_proposal_id_does_not_commit_to_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let client = client(dir.path()).await;
            let (mut proposal, _) = p2id_proposal(&client, true);
            proposal.id = crate::transaction::word_to_hex(&Word::from([3u32, 3, 3, 3]));

            let error = client
                .export_p2id_note_to_bytes(&proposal)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("carries a summary committing to"), "{error}");
        }

        #[tokio::test]
        async fn a_proposal_other_than_a_p2id_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let client = client(dir.path()).await;
            let (mut proposal, _) = p2id_proposal(&client, true);
            proposal.transaction_type = TransactionType::Custom;

            assert!(client.export_p2id_note_to_bytes(&proposal).await.is_err());
        }
    }

    #[test]
    fn note_file_note_id_by_variant() {
        let note = build_test_note();
        let expected = note.id().to_hex();

        // An ID-only file resolves to the note ID.
        let id_file = NoteFile::NoteId(note.id());
        assert_eq!(note_file_note_id(&id_file), Some(expected));

        // A details-only file has no note ID yet.
        let sync_hint = NoteSyncHint::new(BlockNumber::from(0u32), note.metadata().tag());
        let details_file = NoteFile::ExpectedNote {
            details: note.into(),
            sync_hint,
        };
        assert_eq!(note_file_note_id(&details_file), None);
    }

    #[test]
    fn note_file_roundtrips_through_bytes() {
        let note = build_test_note();
        let sync_hint = NoteSyncHint::new(BlockNumber::from(7u32), note.metadata().tag());
        let file = NoteFile::ExpectedNote {
            details: note.into(),
            sync_hint,
        };

        let bytes = file.to_bytes();
        let decoded = NoteFile::try_from_bytes(&bytes).unwrap();
        match decoded {
            NoteFile::ExpectedNote { sync_hint, .. } => {
                assert_eq!(sync_hint.after_block_num(), BlockNumber::from(7u32));
            }
            _ => panic!("expected details variant"),
        }

        assert!(NoteFile::try_from_bytes(b"not a note file").is_err());
    }
}
