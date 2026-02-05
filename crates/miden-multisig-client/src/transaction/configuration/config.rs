//! Multisig configuration advice and transaction building.

use miden_client::ScriptBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_objects::{Felt, Hasher, Word};

use crate::error::{MultisigError, Result};

/// Builds the multisig configuration advice map entry.
///
/// Returns (config_hash, config_values) tuple.
pub fn build_multisig_config_advice(
    threshold: u64,
    signer_commitments: &[Word],
) -> (Word, Vec<Felt>) {
    let num_approvers = signer_commitments.len() as u64;

    let mut payload = Vec::with_capacity(4 + signer_commitments.len() * 4);
    payload.extend_from_slice(&[
        Felt::new(threshold),
        Felt::new(num_approvers),
        Felt::new(0),
        Felt::new(0),
    ]);

    for commitment in signer_commitments.iter().rev() {
        payload.extend_from_slice(commitment.as_elements());
    }

    let digest = Hasher::hash_elements(&payload);
    let config_hash: Word = digest;
    (config_hash, payload)
}

/// Builds the update_signers transaction script.
pub fn build_update_signers_script() -> Result<TransactionScript> {
    let multisig_library = get_multisig_library().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to get multisig library: {}", e))
    })?;

    let tx_script_code = "
        begin
            call.::update_signers_and_threshold
        end
    ";

    let tx_script = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&multisig_library)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds an update_signers transaction request.
///
/// Returns (TransactionRequest, config_hash) tuple.
pub fn build_update_signers_transaction_request<I>(
    threshold: u64,
    signer_commitments: &[Word],
    salt: Word,
    extra_advice: I,
) -> Result<(TransactionRequest, Word)>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let (config_hash, config_values) = build_multisig_config_advice(threshold, signer_commitments);
    let script = build_update_signers_script()?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(config_hash)
        .extend_advice_map([(config_hash, config_values)])
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok((request, config_hash))
}
