//! Multisig configuration advice and transaction building.

use guardian_shared::SignatureScheme;
use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_protocol::assembly::Library;
use miden_protocol::{Felt, Hasher, Word};
use miden_standards::StandardsLib;

use crate::error::{MultisigError, Result};
use crate::procedures::ProcedureName;

/// Builds the multisig configuration advice map entry.
///
/// Layout matches the upstream `update_signers_and_threshold` reader: a config
/// header word `[threshold, num_approvers, 0, 0]` followed by each approver as an
/// interleaved `[PUB_KEY(4), SCHEME_ID(4)]` pair, iterated in reverse index order.
/// Every approver carries the account's single signature scheme.
///
/// Returns (config_hash, config_values) tuple.
pub fn build_multisig_config_advice(
    threshold: u64,
    signer_commitments: &[Word],
    scheme: SignatureScheme,
) -> (Word, Vec<Felt>) {
    let num_approvers = signer_commitments.len() as u64;
    let scheme_id = Felt::new_unchecked(scheme.auth_scheme_id());

    let mut payload = Vec::with_capacity(4 + signer_commitments.len() * 8);
    payload.extend_from_slice(&[
        Felt::new_unchecked(threshold),
        Felt::new_unchecked(num_approvers),
        Felt::new_unchecked(0),
        Felt::new_unchecked(0),
    ]);

    for commitment in signer_commitments.iter().rev() {
        payload.extend_from_slice(commitment.as_elements());
        payload.extend_from_slice(&[
            scheme_id,
            Felt::new_unchecked(0),
            Felt::new_unchecked(0),
            Felt::new_unchecked(0),
        ]);
    }

    let digest = Hasher::hash_elements(&payload);
    let config_hash: Word = digest;
    (config_hash, payload)
}

/// Builds the update_signers transaction script.
pub fn build_update_signers_script(_scheme: SignatureScheme) -> Result<TransactionScript> {
    // The guarded-multisig library is scheme-agnostic (per-signer scheme lives in account
    // storage), so the script does not branch on `scheme`; the param is kept for API stability.
    let standards_lib: Library = StandardsLib::default().into();

    let tx_script_code = "
        use miden::standards::auth::multisig
        begin
            call.multisig::update_signers_and_threshold
        end
    ";

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(standards_lib)
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
    let (config_hash, config_values) =
        build_multisig_config_advice(threshold, signer_commitments, scheme);
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
    _scheme: SignatureScheme,
) -> Result<TransactionScript> {
    let standards_lib: Library = StandardsLib::default().into();

    let procedure_root = procedure.root();
    let tx_script_code = format!(
        r#"
        use miden::standards::auth::multisig
        begin
            push.{procedure_root}
            push.{threshold}
            call.multisig::set_procedure_threshold
            dropw
            drop
        end
    "#
    );

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(standards_lib)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(&tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds an update_procedure_threshold transaction request.
///
/// `set_procedure_threshold` reads its `[proc_threshold, PROC_ROOT]` inputs from the operand
/// stack (pushed by the script), so only the auth signature advice is attached here.
pub fn build_update_procedure_threshold_transaction_request<I>(
    procedure: ProcedureName,
    threshold: u32,
    salt: Word,
    extra_advice: I,
    scheme: SignatureScheme,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let script = build_update_procedure_threshold_script(procedure, threshold, scheme)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}
