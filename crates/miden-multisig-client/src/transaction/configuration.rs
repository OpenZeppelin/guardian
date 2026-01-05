//! Multisig configuration transaction utilities.
//!
//! Functions for building transactions that modify the multisig configuration
//! (signers, threshold, etc.).

use miden_client::ScriptBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_objects::account::auth::Signature;
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

/// Builds an advice entry for a signature.
///
/// The key is Hash(pubkey_commitment, message) and the value is the prepared signature.
pub fn build_signature_advice_entry(
    pubkey_commitment: Word,
    message: Word,
    signature: &Signature,
) -> (Word, Vec<Felt>) {
    let mut elements = Vec::with_capacity(8);
    elements.extend_from_slice(pubkey_commitment.as_elements());
    elements.extend_from_slice(message.as_elements());
    let key: Word = Hasher::hash_elements(&elements);
    let values = signature.to_prepared_signature(message);
    (key, values)
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

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::account::auth::Signature as AccountSignature;
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;

    #[test]
    fn signature_advice_key_matches_hash_elements_concat() {
        let pubkey_commitment =
            Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
        let message = Word::from([Felt::new(5), Felt::new(6), Felt::new(7), Felt::new(8)]);

        let secret_key = SecretKey::new();
        let rpo_sig = secret_key.sign(message);
        let signature = AccountSignature::from(rpo_sig);
        let (key, _) = build_signature_advice_entry(pubkey_commitment, message, &signature);

        let mut elements = Vec::with_capacity(8);
        elements.extend_from_slice(pubkey_commitment.as_elements());
        elements.extend_from_slice(message.as_elements());
        let expected: Word = Hasher::hash_elements(&elements);

        assert_eq!(key, expected);
    }
}
