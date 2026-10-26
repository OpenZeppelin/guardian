//! Multisig configuration advice and transaction building.

use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_protocol::assembly::Package;
use miden_protocol::{Felt, Hasher, Word};
use miden_standards::StandardsLib;
use miden_standards::account::auth::MultisigAuthArgs;

use crate::error::{MultisigError, Result};
use crate::procedures::ProcedureName;
use crate::signers::SignerSpec;
use crate::transaction::TransactionRequestBuilderExt;

/// Builds the multisig configuration advice map entry.
///
/// Layout matches the upstream `update_signers_and_threshold` reader: a config
/// header word `[threshold, num_approvers, 0, 0]` followed by each approver as an
/// interleaved `[PUB_KEY(4), SCHEME_ID(4)]` pair, iterated in reverse index order.
/// Each approver carries its own scheme.
///
/// Returns (config_hash, config_values) tuple.
pub fn build_multisig_config_advice(threshold: u64, signers: &[SignerSpec]) -> (Word, Vec<Felt>) {
    let num_approvers = signers.len() as u64;

    let mut payload = Vec::with_capacity(4 + signers.len() * 8);
    payload.extend_from_slice(&[
        Felt::new_unchecked(threshold),
        Felt::new_unchecked(num_approvers),
        Felt::new_unchecked(0),
        Felt::new_unchecked(0),
    ]);

    for signer in signers.iter().rev() {
        payload.extend_from_slice(signer.commitment.as_elements());
        payload.extend_from_slice(&[
            Felt::new_unchecked(signer.scheme.auth_scheme_id()),
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
///
/// The guarded-multisig library is scheme-agnostic: each signer's scheme lives in
/// account storage, so the script is identical for Falcon and ECDSA accounts.
pub fn build_update_signers_script() -> Result<TransactionScript> {
    let standards_lib: Package = StandardsLib::default().into();

    let tx_script_code = "
        use miden::standards::auth::multisig
        @transaction_script
        pub proc main
            call.multisig::update_signers_and_threshold
        end
    ";

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_package(standards_lib)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds an update_signers transaction request.
///
/// `signers` is the new approver set in storage order, each approver with the
/// scheme the account will verify it under. Read the schemes of approvers the
/// update keeps from the account ([`crate::MultisigAccount::cosigners`]):
/// writing one under another scheme locks that approver out.
///
/// Returns (TransactionRequest, config_hash) tuple.
pub fn build_update_signers_transaction_request<I>(
    threshold: u64,
    signers: &[SignerSpec],
    auth_args: &MultisigAuthArgs,
    extra_advice: I,
) -> Result<(TransactionRequest, Word)>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let (config_hash, config_values) = build_multisig_config_advice(threshold, signers);
    let script = build_update_signers_script()?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(config_hash)
        .extend_advice_map([(config_hash, config_values)])
        .extend_advice_map(extra_advice)
        .multisig_auth_args(auth_args)
        .build()?;

    Ok((request, config_hash))
}

/// Builds the update_procedure_threshold transaction script.
pub fn build_update_procedure_threshold_script(
    procedure: ProcedureName,
    threshold: u32,
) -> Result<TransactionScript> {
    let standards_lib: Package = StandardsLib::default().into();

    let procedure_root = procedure.root();
    let tx_script_code = format!(
        r#"
        use miden::standards::auth::multisig
        @transaction_script
        pub proc main
            push.{procedure_root}
            push.{threshold}
            call.multisig::set_procedure_threshold
            dropw
            drop
        end
    "#
    );

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_package(standards_lib)
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
    auth_args: &MultisigAuthArgs,
    extra_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let script = build_update_procedure_threshold_script(procedure, threshold)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .extend_advice_map(extra_advice)
        .multisig_auth_args(auth_args)
        .build()?;

    Ok(request)
}

#[cfg(test)]
mod tests {
    use guardian_shared::SignatureScheme;

    use super::*;

    /// Each approver's pair carries its own scheme id, in reverse index order
    /// after the config header, so a mixed set keeps every approver's scheme.
    #[test]
    fn config_advice_writes_each_approver_under_its_own_scheme() {
        let falcon = Word::from([1u32, 2, 3, 4]);
        let ecdsa = Word::from([5u32, 6, 7, 8]);
        let signers = [
            SignerSpec::new(falcon, SignatureScheme::Falcon),
            SignerSpec::new(ecdsa, SignatureScheme::Ecdsa),
        ];

        let (config_hash, payload) = build_multisig_config_advice(2, &signers);

        let scheme_word = |scheme: SignatureScheme| {
            [
                Felt::new_unchecked(scheme.auth_scheme_id()),
                Felt::new_unchecked(0),
                Felt::new_unchecked(0),
                Felt::new_unchecked(0),
            ]
        };
        let mut expected = vec![
            Felt::new_unchecked(2),
            Felt::new_unchecked(2),
            Felt::new_unchecked(0),
            Felt::new_unchecked(0),
        ];
        expected.extend_from_slice(ecdsa.as_elements());
        expected.extend_from_slice(&scheme_word(SignatureScheme::Ecdsa));
        expected.extend_from_slice(falcon.as_elements());
        expected.extend_from_slice(&scheme_word(SignatureScheme::Falcon));
        assert_eq!(payload, expected);
        assert_eq!(config_hash, Hasher::hash_elements(&expected));
        assert_ne!(
            SignatureScheme::Falcon.auth_scheme_id(),
            SignatureScheme::Ecdsa.auth_scheme_id()
        );
    }
}
