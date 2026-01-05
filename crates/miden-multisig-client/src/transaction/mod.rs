//! Transaction building and execution for multisig operations.

mod builder;
mod configuration;
mod consume;
mod payment;
mod psm;

pub use builder::ProposalBuilder;
pub use configuration::{build_signature_advice_entry, build_update_signers_transaction_request};
pub use consume::build_consume_notes_transaction_request;
pub use payment::build_p2id_transaction_request;
pub use psm::build_update_psm_transaction_request;

use miden_client::transaction::{TransactionExecutorError, TransactionRequest, TransactionSummary};
use miden_client::{Client, ClientError};
use miden_objects::account::AccountId;
use miden_objects::{Felt, FieldElement, Word};

use crate::error::{MultisigError, Result};

/// Executes a transaction to get its summary (expects Unauthorized error).
pub async fn execute_for_summary(
    client: &mut Client<()>,
    account_id: AccountId,
    request: TransactionRequest,
) -> Result<TransactionSummary> {
    match client.execute_transaction(account_id, request).await {
        Ok(_) => Err(MultisigError::UnexpectedSuccess),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(ClientError::TransactionExecutorError(err)) => {
            Err(MultisigError::TransactionExecution(err.to_string()))
        }
        Err(err) => Err(MultisigError::MidenClient(err.to_string())),
    }
}

/// Generates a random salt word.
pub fn generate_salt() -> Word {
    let mut bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::rng(), &mut bytes);

    let mut felts = [Felt::ZERO; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(chunk);
        felts[i] = Felt::new(u64::from_le_bytes(arr));
    }
    felts.into()
}

/// Converts a Word to hex string with 0x prefix.
pub fn word_to_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_int().to_le_bytes())
        .collect();
    format!("0x{}", hex::encode(bytes))
}
