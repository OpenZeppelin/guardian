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
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);

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

    /// Guards against silent transaction-kernel drift.
    ///
    /// The transaction kernel is re-assembled from `miden-protocol`'s MASM at build
    /// time, and its commitment is derived from hashes computed by transitive crates
    /// (notably the Plonky3 `p3-*` family, pulled in via `miden-crypto`). If a stale
    /// or mismatched `Cargo.lock` resolves those crates to a version different from the
    /// network's, the client computes a kernel the node doesn't recognise, and every
    /// transaction aborts in the prologue with "value for key ... not present in the
    /// advice map" (the missing key being the network's kernel commitment).
    ///
    /// This value must equal the live network's "Proof Commitment" (kernel commitment).
    /// If this test fails after a dependency bump, the kernel has drifted: align the
    /// kernel-affecting crates (run `cargo update`) until it matches the network again,
    /// then update this constant in the same change.
    #[test]
    fn transaction_kernel_commitment_matches_network() {
        use miden_protocol::transaction::TransactionKernel;

        const EXPECTED_KERNEL_COMMITMENT: &str =
            "0x9b3876970730deff3fc4e1d90d68b0578ce19c6e5bd58a0ac5774dc65dbea1d7";

        let actual = word_to_hex(&TransactionKernel.to_commitment());
        assert_eq!(
            actual, EXPECTED_KERNEL_COMMITMENT,
            "transaction kernel commitment drifted from the network kernel; a transitive \
             hashing crate (e.g. Plonky3 `p3-*`) likely changed in Cargo.lock"
        );
    }
}
