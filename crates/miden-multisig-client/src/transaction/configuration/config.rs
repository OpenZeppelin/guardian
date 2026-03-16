//! Multisig configuration advice and transaction building.

use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_confidential_contracts::masm_builder::{
    get_multisig_ecdsa_library, get_multisig_library,
};
use miden_protocol::{Felt, Hasher, Word};
use private_state_manager_shared::SignatureScheme;

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

/// Builds the update_signers transaction script.
pub fn build_update_signers_script(scheme: SignatureScheme) -> Result<TransactionScript> {
    let multisig_library = match scheme {
        SignatureScheme::Falcon => get_multisig_library(),
        SignatureScheme::Ecdsa => get_multisig_ecdsa_library(),
    }
    .map_err(|e| {
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

/// Builds an update_signers transaction request.
///
/// Returns (TransactionRequest, config_hash) tuple.
pub fn build_update_signers_transaction_request<I>(
    threshold: u64,
    signer_commitments: &[Word],
    salt: Word,
    extra_advice: I,
    scheme: SignatureScheme,
) -> Result<(TransactionRequest, Word)>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let (config_hash, config_values) = build_multisig_config_advice(threshold, signer_commitments);
    let script = build_update_signers_script(scheme)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(config_hash)
        .extend_advice_map([(config_hash, config_values)])
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok((request, config_hash))
}

/// Builds the update_procedure_threshold transaction script.
pub fn build_update_procedure_threshold_script(
    procedure: ProcedureName,
    threshold: u32,
    scheme: SignatureScheme,
) -> Result<TransactionScript> {
    let multisig_library = match scheme {
        SignatureScheme::Falcon => get_multisig_library(),
        SignatureScheme::Ecdsa => get_multisig_ecdsa_library(),
    }
    .map_err(|e| {
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
        .compile_tx_script(&tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds an update_procedure_threshold transaction request.
///
/// Returns (TransactionRequest, config_hash) tuple.
pub fn build_update_procedure_threshold_transaction_request<I>(
    procedure: ProcedureName,
    threshold: u32,
    salt: Word,
    extra_advice: I,
    scheme: SignatureScheme,
) -> Result<(TransactionRequest, Word)>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let (config_hash, _) = build_procedure_threshold_advice(procedure, threshold);
    let script = build_update_procedure_threshold_script(procedure, threshold, scheme)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok((request, config_hash))
}
