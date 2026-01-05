//! PSM (Private State Manager) update transaction utilities.
//!
//! Functions for building transactions that update the PSM configuration,
//! such as switching to a different PSM provider.

use miden_client::ScriptBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_confidential_contracts::masm_builder::get_psm_library;
use miden_objects::{Felt, Word};

use crate::error::{MultisigError, Result};

/// Builds the update_psm_public_key transaction script.
pub fn build_update_psm_script() -> Result<TransactionScript> {
    let psm_library = get_psm_library().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to get PSM library: {}", e))
    })?;

    // The script:
    // 1. Takes the script_arg (key) on the operand stack
    // 2. Uses adv.push_mapval to push the corresponding value from the advice map to the advice stack
    // 3. Clears the operand stack
    // 4. Calls update_psm_public_key which uses adv_loadw to read the new key from the advice stack
    let tx_script_code = r#"
        begin
            # The script_arg (key) is already on the operand stack
            # Push the value from advice map to advice stack
            adv.push_mapval

            # Drop the key from operand stack (it was duplicated by adv.push_mapval)
            dropw

            # Now call update_psm_public_key which will use adv_loadw to read the new key
            call.::update_psm_public_key
        end
    "#;

    let tx_script = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&psm_library)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds a transaction request to update the PSM public key.
///
/// # Arguments
///
/// * `new_psm_pubkey` - The new PSM public key commitment
/// * `salt` - Salt for replay protection
/// * `signature_advice` - Iterator of (key, values) pairs for cosigner signature advice
pub fn build_update_psm_transaction_request<I>(
    new_psm_pubkey: Word,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let script = build_update_psm_script()?;

    let psm_key = new_psm_pubkey;
    let psm_values: Vec<Felt> = new_psm_pubkey.iter().copied().collect();

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(psm_key)
        .extend_advice_map([(psm_key, psm_values)])
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}
