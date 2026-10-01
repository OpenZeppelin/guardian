//! The stored `TransactionRequest` of a Guardian-executable proposal.
//!
//! `miden-client` turns a request into executor inputs only through crate-private helpers, and
//! keeps the pinned input notes and the expiration delta without public accessors. Guardian
//! therefore decodes the bytes itself, in the exact layout of the `miden-client` version the
//! server admits. The request envelope names that version and the server refuses any other
//! before decoding, so this layout is only ever applied to bytes it was written for; a
//! round-trip test against the pinned client guards it across pin bumps.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use miden_client::transaction::ForeignAccount;
use miden_protocol::Word;
use miden_protocol::account::Account;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::merkle::store::MerkleStore;
use miden_protocol::note::{
    Note, NoteDetails, NoteDetailsCommitment, NoteId, NoteRecipient, NoteScript, NoteTag,
    PartialNote,
};
use miden_protocol::transaction::{
    InputNote, InputNotes, TransactionArgs, TransactionScript, TransactionSummary,
};
use miden_protocol::utils::serde::{
    ByteReader, ByteWriter, Deserializable, DeserializationError, Serializable,
};
use miden_protocol::vm::AdviceMap;
use miden_standards::tx_script::{ExpirationTransactionScript, SendNotesTransactionScript};

use crate::storage::execution::RequestInvalidReason;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptTemplate {
    None,
    Custom(TransactionScript),
    SendNotes(Vec<PartialNote>),
}

/// A decoded `TransactionRequest`, field for field.
#[derive(Debug, Clone)]
pub struct StoredRequest {
    block_numbers: BTreeSet<BlockNumber>,
    input_notes: Vec<Note>,
    input_notes_args: Vec<(NoteId, Option<Word>)>,
    explicit_input_notes: BTreeMap<NoteId, InputNote>,
    script_template: ScriptTemplate,
    expected_output_recipients: BTreeMap<Word, NoteRecipient>,
    expected_future_notes: BTreeMap<NoteDetailsCommitment, (NoteDetails, NoteTag)>,
    advice_map: AdviceMap,
    merkle_store: MerkleStore,
    foreign_accounts: Vec<ForeignAccount>,
    expiration_delta: Option<u16>,
    ignore_invalid_input_notes: bool,
    script_arg: Option<Word>,
    auth_arg: Option<Word>,
    fee_conversion_salt: Option<Word>,
    expected_ntx_scripts: Vec<NoteScript>,
}

/// What the executor runs a stored request with.
pub struct ExecutionInputs {
    pub tx_args: TransactionArgs,
    pub input_notes: InputNotes<InputNote>,
    pub note_scripts: Vec<NoteScript>,
}

/// Why a structurally valid request could not become executor inputs.
#[derive(Debug, thiserror::Error)]
pub enum RequestInputsError {
    #[error("{0:?}")]
    Invalid(RequestInvalidReason),
    #[error("request does not build: {0}")]
    Malformed(String),
}

impl StoredRequest {
    pub fn block_numbers(&self) -> &BTreeSet<BlockNumber> {
        &self.block_numbers
    }

    /// Checks the request can be executed by a storeless executor against the signed summary.
    pub fn check_against(&self, summary: &TransactionSummary) -> Result<(), RequestInvalidReason> {
        if !self.block_numbers.contains(&summary.block_number()) {
            return Err(RequestInvalidReason::BoundBlockNotDeclared);
        }
        match self.auth_arg {
            Some(auth_arg)
                if auth_arg != Word::empty() && self.advice_map.get(&auth_arg).is_some() => {}
            _ => return Err(RequestInvalidReason::AuthArgsMissing),
        }
        if approval_expiration_block(summary) == 0 {
            return Err(RequestInvalidReason::ApprovalExpirationMissing);
        }
        if self
            .input_notes
            .iter()
            .any(|note| !self.explicit_input_notes.contains_key(&note.id()))
        {
            return Err(RequestInvalidReason::InputNotesNotPinned);
        }
        Ok(())
    }

    /// The executor inputs, with `extra_advice` (cosigner signatures and the acknowledgment)
    /// merged into the advice map.
    pub fn execution_inputs(
        &self,
        account: &Account,
        extra_advice: impl IntoIterator<Item = (Word, Vec<miden_protocol::Felt>)>,
    ) -> Result<ExecutionInputs, RequestInputsError> {
        let input_notes = InputNotes::new(
            self.input_notes
                .iter()
                .map(|note| {
                    self.explicit_input_notes.get(&note.id()).cloned().ok_or(
                        RequestInputsError::Invalid(RequestInvalidReason::InputNotesNotPinned),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(|e| RequestInputsError::Malformed(e.to_string()))?;

        let note_args = self
            .input_notes_args
            .iter()
            .filter_map(|(note, args)| args.map(|args| (*note, args)))
            .collect();
        let mut tx_args = TransactionArgs::new(self.advice_map.clone()).with_note_args(note_args);
        if let Some((script, template_args)) = self.transaction_script(account)? {
            let script_args = template_args.or(self.script_arg).unwrap_or_default();
            tx_args = tx_args.with_tx_script_and_args(script, script_args);
        }
        if let Some(auth_arg) = self.auth_arg {
            tx_args = tx_args.with_auth_args(auth_arg);
        }
        tx_args.extend_output_note_recipients(
            self.expected_output_recipients
                .values()
                .cloned()
                .map(Box::new),
        );
        tx_args.extend_merkle_store(self.merkle_store.inner_nodes());
        tx_args.extend_advice_map(extra_advice);

        let note_scripts = self
            .input_notes
            .iter()
            .map(|note| note.script().clone())
            .chain(self.expected_ntx_scripts.iter().cloned())
            .collect();
        Ok(ExecutionInputs {
            tx_args,
            input_notes,
            note_scripts,
        })
    }

    fn transaction_script(
        &self,
        account: &Account,
    ) -> Result<Option<(TransactionScript, Option<Word>)>, RequestInputsError> {
        let delta = self.expiration_delta.and_then(NonZeroU16::new);
        match &self.script_template {
            ScriptTemplate::Custom(script) => Ok(Some((script.clone(), None))),
            ScriptTemplate::SendNotes(notes) => {
                let interface = account.code_interface();
                let script = match delta {
                    Some(delta) => {
                        SendNotesTransactionScript::with_expiration_delta(&interface, notes, delta)
                    }
                    None => SendNotesTransactionScript::new(&interface, notes),
                }
                .map_err(|e| RequestInputsError::Malformed(e.to_string()))?;
                Ok(Some((
                    script.tx_script().clone(),
                    Some(script.tx_script_args()),
                )))
            }
            ScriptTemplate::None => Ok(delta.map(|delta| {
                let script = ExpirationTransactionScript::new(delta);
                let args = script.tx_script_args();
                (script.into(), Some(args))
            })),
        }
    }
}

/// The absolute block at which the cosigners' approval lapses, from the summary's first user
/// parameter; `0` when the approval never expires.
pub fn approval_expiration_block(summary: &TransactionSummary) -> u64 {
    summary.user_params().as_elements()[0].as_canonical_u64()
}

impl Serializable for StoredRequest {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        self.block_numbers.write_into(target);
        self.input_notes.write_into(target);
        self.input_notes_args.write_into(target);
        self.explicit_input_notes.write_into(target);
        match &self.script_template {
            ScriptTemplate::None => target.write_u8(0),
            ScriptTemplate::Custom(script) => {
                target.write_u8(1);
                script.write_into(target);
            }
            ScriptTemplate::SendNotes(notes) => {
                target.write_u8(2);
                notes.write_into(target);
            }
        }
        self.expected_output_recipients.write_into(target);
        self.expected_future_notes.write_into(target);
        self.advice_map.write_into(target);
        self.merkle_store.write_into(target);
        self.foreign_accounts.write_into(target);
        self.expiration_delta.write_into(target);
        target.write_u8(u8::from(self.ignore_invalid_input_notes));
        self.script_arg.write_into(target);
        self.auth_arg.write_into(target);
        self.fee_conversion_salt.write_into(target);
        self.expected_ntx_scripts.write_into(target);
    }
}

impl Deserializable for StoredRequest {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let block_numbers = BTreeSet::<BlockNumber>::read_from(source)?;
        let input_notes = Vec::<Note>::read_from(source)?;
        let input_notes_args = Vec::<(NoteId, Option<Word>)>::read_from(source)?;
        let explicit_input_notes = BTreeMap::<NoteId, InputNote>::read_from(source)?;
        for (note_id, input_note) in &explicit_input_notes {
            if *note_id != input_note.id() || !input_notes.contains(input_note.note()) {
                return Err(DeserializationError::InvalidValue(format!(
                    "explicit input note {note_id} does not match a request input note"
                )));
            }
        }
        let script_template = match source.read_u8()? {
            0 => ScriptTemplate::None,
            1 => ScriptTemplate::Custom(TransactionScript::read_from(source)?),
            2 => ScriptTemplate::SendNotes(Vec::<PartialNote>::read_from(source)?),
            other => {
                return Err(DeserializationError::InvalidValue(format!(
                    "invalid script template type {other}"
                )));
            }
        };
        let expected_output_recipients = BTreeMap::<Word, NoteRecipient>::read_from(source)?;
        let expected_future_notes =
            BTreeMap::<NoteDetailsCommitment, (NoteDetails, NoteTag)>::read_from(source)?;
        let advice_map = AdviceMap::read_from(source)?;
        let merkle_store = MerkleStore::read_from(source)?;
        let mut seen = BTreeSet::new();
        let foreign_accounts = Vec::<ForeignAccount>::read_from(source)?
            .into_iter()
            .filter(|account| seen.insert(account.account_id()))
            .collect();
        let expiration_delta = Option::<u16>::read_from(source)?;
        let ignore_invalid_input_notes = source.read_u8()? == 1;
        let script_arg = Option::<Word>::read_from(source)?;
        let auth_arg = Option::<Word>::read_from(source)?;
        let fee_conversion_salt = Option::<Word>::read_from(source)?;
        let expected_ntx_scripts = Vec::<NoteScript>::read_from(source)?;
        Ok(Self {
            block_numbers,
            input_notes,
            input_notes_args,
            explicit_input_notes,
            script_template,
            expected_output_recipients,
            expected_future_notes,
            advice_map,
            merkle_store,
            foreign_accounts,
            expiration_delta,
            ignore_invalid_input_notes,
            script_arg,
            auth_arg,
            fee_conversion_salt,
            expected_ntx_scripts,
        })
    }
}
