//! Guardian update transaction utilities.
//!
//! Functions for building transactions that rotate the GUARDIAN public key.
//!
//! The `AuthGuardedMultisig` component takes the new key and its scheme as
//! operand-stack arguments. Rotation requires only the multisig threshold
//! signatures — no current-guardian signature — matching `docs/CONCEPTS.md`
//! cold-key recovery.

use guardian_shared::SignatureScheme;
use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::AuthGuardedMultisig;

use super::MaybeFeeConversionInfo;
use crate::error::{MultisigError, Result};
use miden_standards::account::auth::FeeConversionInfo;

/// Builds the update_guardian_public_key transaction script.
///
/// Pushes the new guardian key word and scheme id as stack args, calls the
/// component procedure, then drops the five pushed felts (the call leaves them).
pub fn build_update_guardian_script(
    new_guardian_pubkey: Word,
    scheme: SignatureScheme,
) -> Result<TransactionScript> {
    let scheme_id = scheme.auth_scheme_id();
    let tx_script_code = format!(
        "@transaction_script\npub proc main\n    push.{new_guardian_pubkey}\n    push.{scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
    );

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_package(AuthGuardedMultisig::code())
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(&tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}

/// Builds a transaction request to rotate the GUARDIAN public key.
///
/// # Arguments
///
/// * `new_guardian_pubkey` - The new GUARDIAN public key commitment
/// * `scheme` - The signature scheme of the new guardian key
/// * `salt` - Salt for replay protection
/// * `signature_advice` - Iterator of (key, values) pairs for the multisig
///   threshold signature advice (no guardian signature is required)
pub fn build_update_guardian_transaction_request<I>(
    new_guardian_pubkey: Word,
    scheme: SignatureScheme,
    salt: Word,
    signature_advice: I,
    fee_conversion_info: Option<FeeConversionInfo>,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let script = build_update_guardian_script(new_guardian_pubkey, scheme)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .extend_advice_map(signature_advice)
        .maybe_fee_conversion_info(fee_conversion_info, salt)
        .build()?;

    Ok(request)
}
