//! The stored `TransactionRequest` of a Guardian-executable proposal.
//!
//! `miden-client` turns a request into executor inputs only through crate-private helpers, and
//! keeps the pinned input notes and the expiration delta without public accessors. Guardian
//! therefore decodes the bytes itself, in the exact layout of the server's pinned `miden-client`.
//! The bytes are also decoded by the pinned client itself, so its validation applies, and bytes
//! left over after the last known field are refused rather than dropped. Bytes written by
//! another version either fail to decode or reproduce a different summary, and execution refuses
//! both before proving; a round-trip test that sets every field guards the layout across pin
//! bumps.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use miden_client::transaction::{ForeignAccount, TransactionRequest};
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountCodeUpgrade};
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
    ByteReader, ByteWriter, Deserializable, DeserializationError, Serializable, SliceReader,
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
    account_code_upgrade: Option<AccountCodeUpgrade>,
}

/// What the executor runs a stored request with.
pub struct ExecutionInputs {
    pub tx_args: TransactionArgs,
    pub input_notes: InputNotes<InputNote>,
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
    /// Decodes `bytes` as the pinned client's request.
    pub fn decode(bytes: &[u8]) -> Result<Self, DeserializationError> {
        TransactionRequest::read_from_bytes(bytes)?;
        let mut reader = SliceReader::new(bytes);
        let request = Self::read_from(&mut reader)?;
        if reader.has_more_bytes() {
            return Err(DeserializationError::InvalidValue(
                "the request carries fields this server's client version does not know".to_string(),
            ));
        }
        Ok(request)
    }

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
        if let Some(upgrade) = &self.account_code_upgrade {
            tx_args = tx_args.with_account_code_upgrade(upgrade.clone());
        }
        tx_args.extend_output_note_recipients(
            self.expected_output_recipients
                .values()
                .cloned()
                .map(Box::new),
        );
        tx_args.extend_merkle_store(self.merkle_store.inner_nodes());
        tx_args.extend_advice_map(extra_advice);

        Ok(ExecutionInputs {
            tx_args,
            input_notes,
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
        self.account_code_upgrade.write_into(target);
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
        let account_code_upgrade = Option::<AccountCodeUpgrade>::read_from(source)?;
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
            account_code_upgrade,
        })
    }
}

#[cfg(test)]
mod tests {
    use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
    use miden_protocol::Word;
    use miden_protocol::account::{AccountBuilder, AccountType};
    use miden_protocol::utils::serde::{Deserializable, Serializable};
    use miden_standards::account::auth::NoAuth;
    use miden_standards::account::wallets::BasicWallet;

    use super::StoredRequest;

    fn word(seed: u64) -> Word {
        Word::from([seed, seed + 1, seed + 2, seed + 3].map(miden_protocol::Felt::new_unchecked))
    }

    fn request_with_every_scalar_field() -> TransactionRequest {
        let code = AccountBuilder::new([0xAB; 32])
            .account_type(AccountType::Public)
            .with_component(BasicWallet)
            .with_component(NoAuth)
            .build()
            .unwrap()
            .code()
            .clone();
        TransactionRequestBuilder::new()
            .expiration_delta(256)
            .ignore_invalid_input_notes()
            .script_arg(word(1))
            .auth_arg(word(5))
            .fee_conversion_salt(word(9))
            .account_code_upgrade(code)
            .build()
            .unwrap()
    }

    /// MAST deserialization is not canonical, so the mirror is held to semantic equality under
    /// the pinned client's own decoder.
    #[test]
    fn the_mirror_round_trips_every_field_the_pinned_client_writes() {
        let request = request_with_every_scalar_field();
        assert!(request.account_code_upgrade().is_some());
        let bytes = request.to_bytes();
        let mirrored = StoredRequest::decode(&bytes).unwrap();
        assert_eq!(
            TransactionRequest::read_from_bytes(&mirrored.to_bytes()).unwrap(),
            TransactionRequest::read_from_bytes(&bytes).unwrap()
        );
    }

    #[test]
    fn bytes_after_the_last_known_field_are_refused() {
        let mut bytes = request_with_every_scalar_field().to_bytes();
        bytes.push(0);
        assert!(StoredRequest::decode(&bytes).is_err());
    }
}
