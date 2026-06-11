//! Transaction building and execution for multisig operations.

mod builder;
mod configuration;
mod consume;
mod guardian;
mod payment;

pub use builder::ProposalBuilder;
pub use configuration::{
    build_update_procedure_threshold_transaction_request, build_update_signers_transaction_request,
};
pub use consume::{
    build_consume_notes_transaction_request, build_consume_notes_transaction_request_from_notes,
};
pub use guardian::build_update_guardian_transaction_request;
pub use payment::build_p2id_transaction_request;

use miden_client::ClientError;
use miden_client::transaction::{TransactionExecutorError, TransactionRequest, TransactionSummary};
use miden_protocol::account::AccountId;
use miden_protocol::{Felt, Word};

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};

/// Deserializes a producer-supplied transaction request bytes (issue #266 producer
/// API). The bytes are the serialized form of a Miden `TransactionRequest`.
pub fn deserialize_transaction_request(bytes: &[u8]) -> Result<TransactionRequest> {
    use miden_client::Deserializable;
    TransactionRequest::read_from_bytes(bytes).map_err(|e| {
        MultisigError::InvalidConfig(format!("failed to decode transaction request: {e}"))
    })
}

/// Executes a transaction to get its summary (expects Unauthorized error).
pub async fn execute_for_summary(
    client: &mut MidenSdkClient,
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
        felts[i] = guardian_shared::felt::felt_from_u64_reduced(u64::from_le_bytes(arr));
    }
    felts.into()
}

/// Converts a Word to hex string with 0x prefix.
pub fn word_to_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_canonical_u64().to_le_bytes())
        .collect();
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_transaction_request_rejects_garbage_bytes() {
        let err = deserialize_transaction_request(&[0xde, 0xad, 0xbe, 0xef])
            .expect_err("garbage bytes must not deserialize");
        assert!(
            err.to_string()
                .contains("failed to decode transaction request")
        );
    }

    #[test]
    fn deserialize_transaction_request_rejects_empty_bytes() {
        let err =
            deserialize_transaction_request(&[]).expect_err("empty bytes must not deserialize");
        assert!(
            err.to_string()
                .contains("failed to decode transaction request")
        );
    }
}
