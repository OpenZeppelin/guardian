//! Multisig configuration transaction utilities.
//!
//! Functions for building transactions that modify the multisig configuration
//! (signers, threshold, etc.).

use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_protocol::account::auth::Signature;
use miden_protocol::{Felt, Hasher, Word};

use crate::error::{MultisigError, Result};
use crate::procedures::ProcedureName;

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

/// Builds the procedure-threshold advice map entry.
///
/// Returns (config_hash, config_values) tuple.
pub fn build_procedure_threshold_advice(
    procedure: ProcedureName,
    threshold: u32,
) -> (Word, Vec<Felt>) {
    let procedure_root = procedure.root();
    let mut payload = Vec::with_capacity(8);
    payload.extend_from_slice(procedure_root.as_elements());
    payload.extend_from_slice(&[
        Felt::new(threshold as u64),
        Felt::new(0),
        Felt::new(0),
        Felt::new(0),
    ]);

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
        use oz_multisig::multisig
        begin
            call.multisig::update_signers_and_threshold
        end
    ";

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(multisig_library)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds the update_procedure_threshold transaction script.
pub fn build_update_procedure_threshold_script(
    procedure: ProcedureName,
    threshold: u32,
) -> Result<TransactionScript> {
    let multisig_library = get_multisig_library().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to get multisig library: {}", e))
    })?;

    let procedure_root = procedure.root();
    let tx_script_code = format!(
        r#"
        use oz_multisig::multisig
        begin
            push.{procedure_root}
            push.{threshold}
            call.multisig::update_procedure_threshold
            dropw
            drop
        end
    "#
    );

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(multisig_library)
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

/// Builds an update_procedure_threshold transaction request.
///
/// Returns (TransactionRequest, config_hash) tuple.
pub fn build_update_procedure_threshold_transaction_request<I>(
    procedure: ProcedureName,
    threshold: u32,
    salt: Word,
    extra_advice: I,
) -> Result<(TransactionRequest, Word)>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let (config_hash, _) = build_procedure_threshold_advice(procedure, threshold);
    let script = build_update_procedure_threshold_script(procedure, threshold)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok((request, config_hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::account::auth::Signature as AccountSignature;
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;

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

    #[test]
    fn procedure_threshold_advice_contains_root_and_threshold_word() {
        let (config_hash, payload) = build_procedure_threshold_advice(ProcedureName::SendAsset, 3);

        assert_eq!(payload.len(), 8);
        assert_eq!(&payload[..4], ProcedureName::SendAsset.root().as_elements());
        assert_eq!(payload[4], Felt::new(3));
        assert_eq!(payload[5], Felt::new(0));
        assert_eq!(payload[6], Felt::new(0));
        assert_eq!(payload[7], Felt::new(0));

        let expected_hash: Word = Hasher::hash_elements(&payload);
        assert_eq!(config_hash, expected_hash);
    }
}
