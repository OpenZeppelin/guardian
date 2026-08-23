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

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use miden_client::ClientError;
use miden_client::transaction::{
    ChainAnchor, TransactionExecutorError, TransactionRequest, TransactionSummary,
};
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

/// Serializes a [`ChainAnchor`] to base64 for the proposal wire payload.
pub fn chain_anchor_to_base64(anchor: &ChainAnchor) -> String {
    use miden_client::Serializable;
    BASE64.encode(anchor.to_bytes())
}

/// Deserializes a [`ChainAnchor`] from its base64 wire form. `ChainAnchor`
/// deserialization validates the header/chain consistency internally, so a
/// decoded anchor only needs its block commitment checked against the signed
/// transaction summary before it is safe to execute against.
pub fn chain_anchor_from_base64(anchor_b64: &str) -> Result<ChainAnchor> {
    use miden_client::Deserializable;
    let bytes = BASE64
        .decode(anchor_b64)
        .map_err(|e| MultisigError::InvalidConfig(format!("invalid chain_anchor base64: {e}")))?;
    ChainAnchor::read_from_bytes(&bytes)
        .map_err(|e| MultisigError::InvalidConfig(format!("invalid chain_anchor: {e}")))
}

/// Captures a [`ChainAnchor`] for the request at the current sync height and
/// executes the transaction against it to get its summary (expects the
/// Unauthorized error). The anchor is returned alongside the summary so the
/// proposer can ship it with the signed data; cosigners and the executor then
/// reproduce the summary — which binds the reference block commitment since
/// protocol 0.16 — with [`execute_for_summary_at`] regardless of their own
/// sync height.
pub async fn execute_for_summary(
    client: &mut MidenSdkClient,
    account_id: AccountId,
    request: TransactionRequest,
) -> Result<(TransactionSummary, ChainAnchor)> {
    let anchor = client
        .chain_anchor_for_request(&request)
        .await
        .map_err(|e| MultisigError::MidenClient(format!("failed to capture chain anchor: {e}")))?;
    let summary = execute_for_summary_at(client, account_id, request, anchor.clone()).await?;
    Ok((summary, anchor))
}

/// Executes a transaction at the given [`ChainAnchor`]'s reference block to
/// get its summary (expects Unauthorized error).
pub async fn execute_for_summary_at(
    client: &mut MidenSdkClient,
    account_id: AccountId,
    request: TransactionRequest,
    anchor: ChainAnchor,
) -> Result<TransactionSummary> {
    match client
        .execute_transaction_at(account_id, request, anchor)
        .await
    {
        Ok(_) => Err(MultisigError::UnexpectedSuccess),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(ClientError::TransactionExecutorError(err)) => {
            Err(MultisigError::TransactionExecution(err.to_string()))
        }
        Err(err) => Err(MultisigError::from(err)),
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
            "0xeb141480ed70ab3d2bf3bb1ec8e84358c41ca11045aecbbd95881c5a2f95ca43";

        let actual = word_to_hex(&TransactionKernel.to_commitment());
        assert_eq!(
            actual, EXPECTED_KERNEL_COMMITMENT,
            "transaction kernel commitment drifted from the network kernel; a transitive \
             hashing crate (e.g. Plonky3 `p3-*`) likely changed in Cargo.lock"
        );
    }
}
