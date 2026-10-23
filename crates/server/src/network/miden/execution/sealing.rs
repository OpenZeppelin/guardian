//! Sealing the transaction inputs a 0.17 submission carries.
//!
//! The node publishes the validator set's encryption key through an operator-served endpoint, so
//! the key is only used once an attestation by a validator named in a header Guardian read
//! verifies. Sealing happens before the no-retry boundary: a failure here is an ordinary
//! fail-and-release, never an account held for a transaction that was never sent.

use miden_client::rpc::NodeRpcClient;
use miden_client::rpc::encryption::{
    AttestedTransactionEncryptionKey, SealedTransactionInputs, TransactionEncryptionKey,
    seal_transaction_inputs,
};
use miden_protocol::transaction::{TransactionId, TransactionInputs};

use super::chain::ChainView;

/// Why the submission inputs could not be sealed.
#[derive(Debug, thiserror::Error)]
pub enum SealingFailed {
    #[error("fetching the transaction encryption key failed: {0}")]
    KeyFetch(String),
    #[error("the transaction encryption key did not verify: {0}")]
    Attestation(String),
    #[error("sealing the transaction inputs failed: {0}")]
    Seal(String),
}

/// Seals `inputs` for the transaction `tx_id` against the validator key attested for the chain
/// the attempt executed on.
pub async fn seal_for_submission(
    rpc: &dyn NodeRpcClient,
    chain: &ChainView,
    tx_id: TransactionId,
    inputs: &TransactionInputs,
) -> Result<SealedTransactionInputs, SealingFailed> {
    let attested = rpc
        .get_transaction_encryption_key()
        .await
        .map_err(|e| SealingFailed::KeyFetch(e.to_string()))?;
    let key = trusted_key(attested, chain)?;
    seal_transaction_inputs(&mut rand::rng(), &key, tx_id, inputs)
        .map_err(|e| SealingFailed::Seal(e.to_string()))
}

/// The served key, once an attestation by a validator in the reference header's set verifies.
pub(super) fn trusted_key(
    attested: AttestedTransactionEncryptionKey,
    chain: &ChainView,
) -> Result<TransactionEncryptionKey, SealingFailed> {
    attested
        .verify(
            chain.genesis_commitment(),
            chain.reference_header().validator_config(),
        )
        .map_err(|e| SealingFailed::Attestation(e.to_string()))
}
