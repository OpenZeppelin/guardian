//! Transaction building and execution for multisig operations.

pub mod configure;
pub mod p2id;
pub mod switch_psm;

use miden_client::transaction::{
    TransactionExecutorError, TransactionRequest, TransactionRequestBuilder, TransactionScript,
    TransactionSummary,
};
use miden_client::{Client, ClientError, ScriptBuilder};
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_objects::account::AccountId;
use miden_objects::account::auth::Signature;
use miden_objects::{Felt, FieldElement, Hasher, Word};

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
    let key = Hasher::merge(&[pubkey_commitment, message]);
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

/// Executes a transaction to get its summary (expects Unauthorized error).
///
/// This is used to get the TransactionSummary for signing before creating a proposal.
pub async fn execute_for_summary(
    client: &mut Client<()>,
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
        felts[i] = Felt::new(u64::from_le_bytes(arr));
    }
    felts.into()
}

/// Converts a Word to hex string with 0x prefix.
pub fn word_to_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_int().to_le_bytes())
        .collect();
    format!("0x{}", hex::encode(bytes))
}
