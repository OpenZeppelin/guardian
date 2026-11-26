//! The stored `TransactionRequest` of a Guardian-executable proposal.
//!
//! The request is decoded with the protobuf encoding of the server's `miden-client`, and turned
//! into executor inputs with the same public helpers that the client uses to execute it. A request
//! that a client of the same version prepared therefore reproduces the summary that it signed.

use std::collections::BTreeSet;

use miden_client::transaction::{InputNoteAuthentication, RequestAuthArgs, TransactionRequest};
use miden_protocol::Word;
use miden_protocol::account::Account;
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::{InputNote, InputNotes, TransactionArgs, TransactionSummary};
use miden_protocol::utils::serde::DeserializationError;

use crate::storage::execution::RequestInvalidReason;

/// The most blocks a stored request may declare. Replay fetches each declared block's header and
/// proof from the node, so the cap bounds that work per attempt.
pub const MAX_DECLARED_BLOCKS: usize = 16;

/// A decoded `TransactionRequest`.
#[derive(Debug, Clone)]
pub struct StoredRequest {
    request: TransactionRequest,
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
    /// Decodes `bytes` as the protobuf encoding of the server's client request.
    pub fn decode(bytes: &[u8]) -> Result<Self, DeserializationError> {
        let request = miden_client_proto::decode::<TransactionRequest>(bytes)
            .map_err(|e| DeserializationError::InvalidValue(e.to_string()))?;
        Ok(Self { request })
    }

    pub fn block_numbers(&self) -> &BTreeSet<BlockNumber> {
        self.request.block_numbers()
    }

    /// Checks the request can be executed by a storeless executor against the signed summary.
    pub fn check_against(&self, summary: &TransactionSummary) -> Result<(), RequestInvalidReason> {
        if !self
            .request
            .block_numbers()
            .contains(&summary.block_number())
        {
            return Err(RequestInvalidReason::BoundBlockNotDeclared);
        }
        if self.request.block_numbers().len() > MAX_DECLARED_BLOCKS {
            return Err(RequestInvalidReason::TooManyDeclaredBlocks);
        }
        match self.request.auth_args() {
            RequestAuthArgs::Explicit(auth_arg)
                if *auth_arg != Word::empty()
                    && self.request.advice_map().get(auth_arg).is_some() => {}
            _ => return Err(RequestInvalidReason::AuthArgsMissing),
        }
        if approval_expiration_block(summary) == 0 {
            return Err(RequestInvalidReason::ApprovalExpirationMissing);
        }
        if !self.input_notes_pinned() {
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
        if !self.input_notes_pinned() {
            return Err(RequestInputsError::Invalid(
                RequestInvalidReason::InputNotesNotPinned,
            ));
        }

        // Every input note is pinned, so no store record is needed to authenticate it.
        let input_notes = self
            .request
            .build_input_notes(Vec::new())
            .map_err(|e| RequestInputsError::Malformed(e.to_string()))?;
        let tx_script = self
            .request
            .build_transaction_script(&account.code_interface())
            .map_err(|e| RequestInputsError::Malformed(e.to_string()))?;
        let mut tx_args = self.request.clone().into_transaction_args(tx_script);
        tx_args.extend_advice_map(extra_advice);

        Ok(ExecutionInputs {
            tx_args,
            input_notes,
        })
    }

    fn input_notes_pinned(&self) -> bool {
        self.request
            .input_notes()
            .iter()
            .all(|note| matches!(note.authentication(), InputNoteAuthentication::Explicit(_)))
    }
}

/// The absolute block at which the cosigners' approval lapses, from the summary's first user
/// parameter; `0` when the approval never expires.
pub fn approval_expiration_block(summary: &TransactionSummary) -> u64 {
    summary.user_params().as_elements()[0].as_canonical_u64()
}

#[cfg(test)]
mod tests {
    use miden_client::transaction::TransactionRequestBuilder;
    use miden_protocol::block::BlockNumber;

    use super::StoredRequest;

    #[test]
    fn a_request_round_trips_through_its_protobuf_encoding() {
        let request = TransactionRequestBuilder::new()
            .block_numbers([BlockNumber::from(3)])
            .expiration_delta(256)
            .ignore_invalid_input_notes()
            .build()
            .unwrap();
        let decoded = StoredRequest::decode(&miden_client_proto::encode(&request)).unwrap();
        assert_eq!(decoded.request, request);
    }

    #[test]
    fn bytes_without_a_request_version_are_refused() {
        assert!(StoredRequest::decode(&[]).is_err());
    }
}
