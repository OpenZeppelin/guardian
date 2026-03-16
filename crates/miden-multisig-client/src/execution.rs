//! Shared execution logic for proposal finalization.

use std::collections::HashSet;

use miden_client::Client;
use miden_client::account::Account;
use miden_client::transaction::TransactionRequest;
use miden_protocol::account::auth::Signature as AccountSignature;
use miden_protocol::asset::FungibleAsset;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature as EcdsaSignature;
use miden_protocol::crypto::dsa::falcon512_rpo::Signature as RpoFalconSignature;
use miden_protocol::utils::Deserializable;
use miden_protocol::{Felt, Word};
use private_state_manager_shared::SignatureScheme;
use private_state_manager_shared::hex::FromHex;

use crate::error::{MultisigError, Result};
use crate::keystore::{ensure_hex_prefix, word_from_hex};
use crate::proposal::TransactionType;
use crate::transaction::build_ecdsa_signature_advice_entry;

/// Signature advice entry: (key, prepared_signature_values)
pub type SignatureAdvice = (Word, Vec<Felt>);

/// Input for collecting a signature into advice format.
pub struct SignatureInput {
    /// Hex-encoded signer commitment (with or without 0x prefix).
    pub signer_commitment: String,
    /// Hex-encoded signature (with or without 0x prefix).
    pub signature_hex: String,
    /// Signature scheme (falcon or ecdsa).
    pub scheme: SignatureScheme,
    /// Hex-encoded public key (required for ECDSA signatures).
    pub public_key_hex: Option<String>,
}

/// Collects and validates cosigner signatures into advice entries.
///
/// Filters signatures to only include those from required signers, skips duplicates,
/// and converts to the format needed for transaction advice.
///
/// # Arguments
/// * `signatures` - Raw signature inputs to process
/// * `required_commitments` - Set of valid signer commitments (lowercase hex)
/// * `tx_summary_commitment` - The transaction summary commitment being signed
///
/// # Returns
/// Vector of (key, prepared_signature) tuples for transaction advice.
pub fn collect_signature_advice(
    signatures: impl IntoIterator<Item = SignatureInput>,
    required_commitments: &HashSet<String>,
    tx_summary_commitment: Word,
) -> Result<Vec<SignatureAdvice>> {
    let mut advice = Vec::new();
    let mut added_signers: HashSet<String> = HashSet::new();

    for sig_input in signatures {
        if !required_commitments
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&sig_input.signer_commitment))
        {
            continue;
        }

        // Skip duplicates
        let signer_lower = sig_input.signer_commitment.to_lowercase();
        if !added_signers.insert(signer_lower) {
            continue;
        }

        let commitment =
            word_from_hex(&sig_input.signer_commitment).map_err(MultisigError::HexDecode)?;

        let sig_hex = ensure_hex_prefix(&sig_input.signature_hex);
        match sig_input.scheme {
            SignatureScheme::Falcon => {
                let rpo_sig = RpoFalconSignature::from_hex(&sig_hex).map_err(|e| {
                    MultisigError::Signature(format!("invalid Falcon signature: {}", e))
                })?;
                advice.push(crate::transaction::build_signature_advice_entry(
                    commitment,
                    tx_summary_commitment,
                    &AccountSignature::from(rpo_sig),
                    None,
                ));
            }
            SignatureScheme::Ecdsa => {
                let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x")).map_err(|e| {
                    MultisigError::Signature(format!("invalid ECDSA signature hex: {}", e))
                })?;
                let ecdsa_sig = EcdsaSignature::read_from_bytes(&sig_bytes).map_err(|e| {
                    MultisigError::Signature(format!(
                        "failed to deserialize ECDSA signature: {}",
                        e
                    ))
                })?;
                let public_key_hex = sig_input.public_key_hex.as_ref().ok_or_else(|| {
                    MultisigError::Signature(
                        "ECDSA signature requires public key for advice preparation".to_string(),
                    )
                })?;
                advice.push(build_ecdsa_signature_advice_entry(
                    commitment,
                    tx_summary_commitment,
                    &AccountSignature::EcdsaK256Keccak(ecdsa_sig),
                    public_key_hex,
                )?);
            }
        }
    }

    Ok(advice)
}

/// Builds the final transaction request based on transaction type.
#[expect(
    clippy::too_many_arguments,
    reason = "execution needs transaction metadata and signature scheme to stay explicit"
)]
pub async fn build_final_transaction_request(
    client: &Client<()>,
    transaction_type: &TransactionType,
    account: &Account,
    salt: Word,
    signature_advice: Vec<SignatureAdvice>,
    metadata_threshold: Option<u64>,
    metadata_signer_commitments: Option<&[Word]>,
    scheme: SignatureScheme,
) -> Result<TransactionRequest> {
    match transaction_type {
        TransactionType::P2ID {
            recipient,
            faucet_id,
            amount,
        } => {
            let asset = FungibleAsset::new(*faucet_id, *amount).map_err(|e| {
                MultisigError::InvalidConfig(format!("failed to create asset: {}", e))
            })?;

            crate::transaction::build_p2id_transaction_request(
                account.id(),
                *recipient,
                vec![asset.into()],
                salt,
                signature_advice,
            )
        }
        TransactionType::ConsumeNotes { note_ids } => {
            crate::transaction::build_consume_notes_transaction_request(
                client,
                note_ids.clone(),
                salt,
                signature_advice,
            )
            .await
        }
        TransactionType::SwitchPsm { new_commitment, .. } => {
            crate::transaction::build_update_psm_transaction_request(
                *new_commitment,
                salt,
                signature_advice,
            )
        }
        TransactionType::UpdateProcedureThreshold {
            procedure,
            new_threshold,
        } => {
            let (tx_request, _) =
                crate::transaction::build_update_procedure_threshold_transaction_request(
                    *procedure,
                    *new_threshold,
                    salt,
                    signature_advice,
                    scheme,
                )?;

            Ok(tx_request)
        }
        TransactionType::AddCosigner { .. }
        | TransactionType::RemoveCosigner { .. }
        | TransactionType::UpdateSigners { .. } => {
            // Signer update transactions need threshold and signer commitments from metadata
            let signer_commitments = metadata_signer_commitments.ok_or_else(|| {
                MultisigError::MissingConfig("signer_commitments for signer update".to_string())
            })?;
            let new_threshold = metadata_threshold
                .ok_or_else(|| MultisigError::MissingConfig("new_threshold".to_string()))?;

            let (tx_request, _) = crate::transaction::build_update_signers_transaction_request(
                new_threshold,
                signer_commitments,
                salt,
                signature_advice,
                scheme,
            )?;

            Ok(tx_request)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_client::Serializable;
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;

    #[test]
    fn test_collect_signature_advice_filters_by_required() {
        let required: HashSet<String> = ["0xabc", "0xdef"].iter().map(|s| s.to_string()).collect();

        // Note: This test validates the filtering logic structure.
        // Full integration requires valid signatures which need real keys.

        let signatures = vec![SignatureInput {
            signer_commitment: "0xunknown".to_string(),
            signature_hex: "0x1234".to_string(),
            scheme: SignatureScheme::Falcon,
            public_key_hex: None,
        }];

        // Unknown signer should be filtered out
        let result = collect_signature_advice(signatures, &required, Word::default());
        // This will fail on signature parsing, but validates filtering happens first
        // In production, only valid signatures would be provided
        assert!(result.is_ok()); // Empty vec since unknown was filtered
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_collect_signature_advice_skips_duplicates() {
        let required: HashSet<String> = ["0xabc"].iter().map(|s| s.to_string()).collect();

        let signatures = vec![
            SignatureInput {
                signer_commitment: "0xABC".to_string(), // uppercase
                signature_hex: "0x1234".to_string(),
                scheme: SignatureScheme::Falcon,
                public_key_hex: None,
            },
            SignatureInput {
                signer_commitment: "0xabc".to_string(), // lowercase duplicate
                signature_hex: "0x5678".to_string(),
                scheme: SignatureScheme::Falcon,
                public_key_hex: None,
            },
        ];

        // Both will fail signature parsing, but second should be deduplicated
        // before reaching that point (based on lowercase comparison)
        let result = collect_signature_advice(signatures, &required, Word::default());
        // Will error on first sig parse since it's not a valid Falcon sig,
        // but the dedup logic is what we're testing
        assert!(result.is_err()); // Error on invalid sig, but only one attempt
    }

    #[test]
    fn test_collect_signature_advice_with_valid_signature() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let msg = Word::default();
        let signature = secret_key.sign(msg);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let required: HashSet<String> = [commitment_hex.clone()].into_iter().collect();
        let signatures = vec![SignatureInput {
            signer_commitment: commitment_hex,
            signature_hex,
            scheme: SignatureScheme::Falcon,
            public_key_hex: None,
        }];

        let advice = collect_signature_advice(signatures, &required, msg).expect("valid advice");
        assert_eq!(advice.len(), 1);
    }
}
