//! Guardian update transaction utilities.
//!
//! Functions for building transactions that update the GUARDIAN configuration,
//! such as switching to a different GUARDIAN provider.

use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_confidential_contracts::masm_builder::get_guardian_library;
use miden_protocol::{Felt, Word};

use crate::error::{MultisigError, Result};

/// Builds the update_guardian_public_key transaction script.
pub fn build_update_guardian_script() -> Result<TransactionScript> {
    let guardian_library = get_guardian_library().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to get GUARDIAN library: {}", e))
    })?;

    let tx_script_code = r#"
        use oz_guardian::guardian
        begin
            # The script_arg (key) is already on the operand stack
            # Push the value from advice map to advice stack
            adv.push_mapval

            # Drop the key from operand stack (it was duplicated by adv.push_mapval)
            dropw

            # Now call update_guardian_public_key which will use adv_loadw to read the new key
            call.guardian::update_guardian_public_key
        end
    "#;

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(guardian_library)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds a transaction request to update the GUARDIAN public key.
///
/// # Arguments
///
/// * `new_guardian_pubkey` - The new GUARDIAN public key commitment
/// * `salt` - Salt for replay protection
/// * `signature_advice` - Iterator of (key, values) pairs for cosigner signature advice
pub fn build_update_guardian_transaction_request<I>(
    new_guardian_pubkey: Word,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let script = build_update_guardian_script()?;

    let guardian_key = new_guardian_pubkey;
    let guardian_values: Vec<Felt> = new_guardian_pubkey.iter().copied().collect();

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(guardian_key)
        .extend_advice_map([(guardian_key, guardian_values)])
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}
