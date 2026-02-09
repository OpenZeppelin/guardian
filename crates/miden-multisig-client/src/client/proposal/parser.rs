//! Helpers for extracting and preparing proposal signature inputs.

use std::collections::HashSet;

use private_state_manager_shared::{DeltaPayload, ProposalSignature, SignatureScheme};

use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::execution::SignatureInput;

/// Parses signature inputs from delta payload JSON and deduplicates by signer commitment.
pub(crate) fn parse_unique_signature_inputs(
    delta_payload_json: &str,
) -> Result<Vec<SignatureInput>> {
    let payload: DeltaPayload = serde_json::from_str(delta_payload_json).map_err(|e| {
        MultisigError::MidenClient(format!("failed to parse delta payload signatures: {}", e))
    })?;

    let mut inputs: Vec<SignatureInput> = payload
        .signatures
        .iter()
        .map(|delta_signature| {
            let (scheme, signature_hex, public_key_hex) = match &delta_signature.signature {
                ProposalSignature::Falcon { signature } => {
                    (SignatureScheme::Falcon, signature.clone(), None)
                }
                ProposalSignature::Ecdsa {
                    signature,
                    public_key,
                } => (
                    SignatureScheme::Ecdsa,
                    signature.clone(),
                    public_key.clone(),
                ),
            };

            SignatureInput {
                signer_commitment: delta_signature.signer_id.clone(),
                signature_hex,
                scheme,
                public_key_hex,
            }
        })
        .collect();

    inputs.sort_by(|a, b| a.signer_commitment.cmp(&b.signer_commitment));
    inputs.dedup_by(|a, b| {
        a.signer_commitment
            .eq_ignore_ascii_case(&b.signer_commitment)
    });

    Ok(inputs)
}

/// Returns the set of required cosigner commitments for signature advice filtering.
pub(crate) fn required_commitments(account: &MultisigAccount) -> HashSet<String> {
    account.cosigner_commitments_hex().into_iter().collect()
}
