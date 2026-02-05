//! Shared execution logic for proposal finalization.

use std::collections::HashSet;

use miden_client::account::Account;
use miden_client::transaction::TransactionRequest;
use miden_objects::account::auth::Signature as AccountSignature;
use miden_objects::asset::FungibleAsset;
use miden_objects::crypto::dsa::ecdsa_k256_keccak::Signature as EcdsaSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use miden_objects::utils::Deserializable;
use miden_objects::{Felt, Word};
use private_state_manager_shared::SignatureScheme;
use private_state_manager_shared::hex::FromHex;

use crate::error::{MultisigError, Result};
use crate::keystore::{commitment_from_hex, ensure_hex_prefix};
use crate::proposal::TransactionType;
use crate::transaction::{
    build_consume_notes_transaction_request, build_ecdsa_signature_advice_entry,
    build_p2id_transaction_request, build_signature_advice_entry,
    build_update_psm_transaction_request, build_update_signers_transaction_request,
};

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

        let signer_lower = sig_input.signer_commitment.to_lowercase();
        if !added_signers.insert(signer_lower) {
            continue;
        }

        let sig_hex = ensure_hex_prefix(&sig_input.signature_hex);
        let commitment =
            commitment_from_hex(&sig_input.signer_commitment).map_err(MultisigError::HexDecode)?;

        match sig_input.scheme {
            SignatureScheme::Falcon => {
                let rpo_sig = RpoFalconSignature::from_hex(&sig_hex).map_err(|e| {
                    MultisigError::Signature(format!("invalid Falcon signature: {}", e))
                })?;
                advice.push(build_signature_advice_entry(
                    commitment,
                    tx_summary_commitment,
                    &AccountSignature::from(rpo_sig),
                    None,
                ));
            }
            SignatureScheme::Ecdsa => {
                let hex_str = sig_hex.trim_start_matches("0x");
                let sig_bytes = hex::decode(hex_str).map_err(|e| {
                    MultisigError::Signature(format!("invalid ECDSA signature hex: {}", e))
                })?;
                let ecdsa_sig = EcdsaSignature::read_from_bytes(&sig_bytes).map_err(|e| {
                    MultisigError::Signature(format!(
                        "failed to deserialize ECDSA signature: {}",
                        e
                    ))
                })?;

                let pubkey_hex = sig_input.public_key_hex.as_ref().ok_or_else(|| {
                    MultisigError::Signature(
                        "ECDSA signature requires public key for advice preparation".to_string(),
                    )
                })?;

                advice.push(build_ecdsa_signature_advice_entry(
                    commitment,
                    tx_summary_commitment,
                    &AccountSignature::EcdsaK256Keccak(ecdsa_sig),
                    pubkey_hex,
                )?);
            }
        }
    }

    Ok(advice)
}

/// Builds the final transaction request based on transaction type.
pub fn build_final_transaction_request(
    transaction_type: &TransactionType,
    account: &Account,
    salt: Word,
    signature_advice: Vec<SignatureAdvice>,
    metadata_threshold: Option<u64>,
    metadata_signer_commitments: Option<&[Word]>,
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

            build_p2id_transaction_request(
                account,
                *recipient,
                vec![asset.into()],
                salt,
                signature_advice,
            )
        }
        TransactionType::ConsumeNotes { note_ids } => {
            build_consume_notes_transaction_request(note_ids.clone(), salt, signature_advice)
        }
        TransactionType::SwitchPsm { new_commitment, .. } => {
            build_update_psm_transaction_request(*new_commitment, salt, signature_advice)
        }
        TransactionType::AddCosigner { .. }
        | TransactionType::RemoveCosigner { .. }
        | TransactionType::UpdateSigners { .. } => {
            let signer_commitments = metadata_signer_commitments.ok_or_else(|| {
                MultisigError::MissingConfig("signer_commitments for signer update".to_string())
            })?;
            let new_threshold = metadata_threshold
                .ok_or_else(|| MultisigError::MissingConfig("new_threshold".to_string()))?;

            let (tx_request, _) = build_update_signers_transaction_request(
                new_threshold,
                signer_commitments,
                salt,
                signature_advice,
            )?;

            Ok(tx_request)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_client::Serializable;
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;

    #[test]
    fn test_collect_signature_advice_filters_by_required() {
        let required: HashSet<String> = ["0xabc", "0xdef"].iter().map(|s| s.to_string()).collect();

        let signatures = vec![SignatureInput {
            signer_commitment: "0xunknown".to_string(),
            signature_hex: "0x1234".to_string(),
            scheme: SignatureScheme::Falcon,
            public_key_hex: None,
        }];

        let result = collect_signature_advice(signatures, &required, Word::default());
        assert!(result.is_ok());
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

        let result = collect_signature_advice(signatures, &required, Word::default());
        assert!(result.is_err());
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

    #[test]
    fn test_collect_signature_advice_with_valid_ecdsa_signature() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;

        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let commitment = pk.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let pk_hex = format!("0x{}", hex::encode(pk.to_bytes()));

        let msg = Word::default();
        let signature = sk.sign(msg);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let required: HashSet<String> = [commitment_hex.clone()].into_iter().collect();
        let signatures = vec![SignatureInput {
            signer_commitment: commitment_hex,
            signature_hex,
            scheme: SignatureScheme::Ecdsa,
            public_key_hex: Some(pk_hex),
        }];

        let advice = collect_signature_advice(signatures, &required, msg).expect("valid advice");
        assert_eq!(advice.len(), 1);
    }

    #[test]
    fn test_collect_signature_advice_ecdsa_missing_pubkey() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;

        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let commitment = pk.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let msg = Word::default();
        let signature = sk.sign(msg);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let required: HashSet<String> = [commitment_hex.clone()].into_iter().collect();
        let signatures = vec![SignatureInput {
            signer_commitment: commitment_hex,
            signature_hex,
            scheme: SignatureScheme::Ecdsa,
            public_key_hex: None, // missing!
        }];

        let result = collect_signature_advice(signatures, &required, msg);
        assert!(result.is_err());
    }

    #[test]
    fn test_collect_signature_advice_empty_input() {
        let required: HashSet<String> = ["0xabc"].iter().map(|s| s.to_string()).collect();
        let signatures: Vec<SignatureInput> = vec![];

        let advice =
            collect_signature_advice(signatures, &required, Word::default()).expect("valid advice");
        assert!(advice.is_empty());
    }

    #[test]
    fn test_collect_signature_advice_case_insensitive_matching() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let msg = Word::default();
        let signature = secret_key.sign(msg);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        // Required is lowercase
        let required: HashSet<String> = [commitment_hex.to_lowercase()].into_iter().collect();
        // Input has uppercase hex digits (but keeps "0x" prefix)
        let upper_commitment = format!(
            "0x{}",
            commitment_hex.strip_prefix("0x").unwrap().to_uppercase()
        );
        let signatures = vec![SignatureInput {
            signer_commitment: upper_commitment,
            signature_hex,
            scheme: SignatureScheme::Falcon,
            public_key_hex: None,
        }];

        let advice = collect_signature_advice(signatures, &required, msg).expect("valid advice");
        assert_eq!(advice.len(), 1);
    }

    #[test]
    fn test_collect_signature_advice_invalid_falcon_signature() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let required: HashSet<String> = [commitment_hex.clone()].into_iter().collect();
        let signatures = vec![SignatureInput {
            signer_commitment: commitment_hex,
            signature_hex: "0xdeadbeef".to_string(), // invalid
            scheme: SignatureScheme::Falcon,
            public_key_hex: None,
        }];

        let result = collect_signature_advice(signatures, &required, Word::default());
        assert!(result.is_err());
    }
}
