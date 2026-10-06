//! Verification of cosigner approvals on Miden proposals.
//!
//! Falcon and raw ECDSA approvals sign the transaction-summary commitment;
//! EIP-712 approvals sign the summary's typed-data hash. ECDSA approvals
//! carry their public key, which execution needs to build the advice map.

use crate::delta_object::ProposalSignature;
use crate::error::{GuardianError, Result};
use guardian_shared::{EcdsaMessageFormat, FromJson};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, Signature as EcdsaSignature,
};
use miden_protocol::crypto::dsa::falcon512_poseidon2::Signature as FalconSignature;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_standards::account::auth::Eip712TransactionSummary;

/// Parses the transaction summary stored in a Miden proposal payload.
pub(crate) fn proposal_tx_summary(delta_payload: &serde_json::Value) -> Result<TransactionSummary> {
    let tx_summary = delta_payload
        .get("tx_summary")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing transaction summary".to_string()))?;
    TransactionSummary::from_json(tx_summary).map_err(GuardianError::InvalidDelta)
}

/// Verifies a cosigner approval against the proposal's transaction summary
/// and returns the commitment of the public key that produced it.
pub(crate) fn verify_proposal_signature(
    tx_summary: &TransactionSummary,
    signature: &ProposalSignature,
) -> Result<String> {
    let message = tx_summary.to_commitment();
    match signature {
        ProposalSignature::Falcon { signature } => {
            let signature: FalconSignature = parse_hex(signature, "Falcon signature")?;
            let public_key = signature.public_key();
            if !public_key.verify(message, &signature) {
                return Err(invalid_signature());
            }
            Ok(commitment_hex(public_key.to_commitment()))
        }
        ProposalSignature::Ecdsa {
            signature,
            public_key,
            message_format,
        } => {
            let signature: EcdsaSignature = parse_hex(signature, "ECDSA signature")?;
            let public_key: EcdsaPublicKey = public_key
                .as_deref()
                .ok_or_else(|| {
                    GuardianError::InvalidProposalSignature(
                        "ECDSA signature requires a public key".to_string(),
                    )
                })
                .and_then(|public_key| parse_hex(public_key, "ECDSA public key"))?;
            let verified = match message_format {
                // Execution recovers the key from `v`, so the signature must
                // recover to the supplied key, not merely verify under it.
                EcdsaMessageFormat::Raw => {
                    public_key.verify(message, &signature)
                        && EcdsaPublicKey::recover_from(message, &signature).is_ok_and(
                            |recovered| recovered.to_commitment() == public_key.to_commitment(),
                        )
                }
                EcdsaMessageFormat::Eip712 => {
                    public_key.verify_prehash(tx_summary.eip712_hash().into_bytes(), &signature)
                }
            };
            if !verified {
                return Err(invalid_signature());
            }
            Ok(commitment_hex(public_key.to_commitment()))
        }
    }
}

fn parse_hex<T: Deserializable>(value: &str, label: &str) -> Result<T> {
    let bytes = hex::decode(value.trim_start_matches("0x"))
        .map_err(|e| GuardianError::InvalidProposalSignature(format!("Invalid {label}: {e}")))?;
    T::read_from_bytes(&bytes)
        .map_err(|e| GuardianError::InvalidProposalSignature(format!("Invalid {label}: {e}")))
}

fn commitment_hex(commitment: Word) -> String {
    format!("0x{}", hex::encode(commitment.to_bytes()))
}

fn invalid_signature() -> GuardianError {
    GuardianError::InvalidProposalSignature("Signature does not match the proposal".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::helpers::{TestEcdsaSigner, TestSigner, create_test_delta_payload};

    const ACCOUNT_ID: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

    fn summary() -> TransactionSummary {
        TransactionSummary::from_json(&create_test_delta_payload(ACCOUNT_ID)).unwrap()
    }

    fn ecdsa(signature: String, public_key: Option<String>) -> ProposalSignature {
        ProposalSignature::Ecdsa {
            signature,
            public_key,
            message_format: EcdsaMessageFormat::Raw,
        }
    }

    #[test]
    fn falcon_signature_over_summary_returns_signer() {
        let summary = summary();
        let signer = TestSigner::new();
        let signature = ProposalSignature::Falcon {
            signature: signer.sign_word(summary.to_commitment()),
        };

        let verified = verify_proposal_signature(&summary, &signature).unwrap();

        assert_eq!(verified, signer.commitment_hex);
    }

    #[test]
    fn falcon_signature_over_other_message_is_rejected() {
        let summary = summary();
        let signature = ProposalSignature::Falcon {
            signature: TestSigner::new().sign_word(Word::default()),
        };

        let error = verify_proposal_signature(&summary, &signature).unwrap_err();

        assert!(matches!(error, GuardianError::InvalidProposalSignature(_)));
    }

    #[test]
    fn malformed_signature_is_rejected() {
        let summary = summary();
        let signature = ProposalSignature::Falcon {
            signature: format!("0x{}", "a".repeat(666)),
        };

        let error = verify_proposal_signature(&summary, &signature).unwrap_err();

        assert!(matches!(error, GuardianError::InvalidProposalSignature(_)));
    }

    #[test]
    fn ecdsa_signature_over_summary_returns_signer() {
        let summary = summary();
        let signer = TestEcdsaSigner::new();
        let signature = ecdsa(
            signer.sign_word(summary.to_commitment()),
            Some(signer.pubkey_hex.clone()),
        );

        let verified = verify_proposal_signature(&summary, &signature).unwrap();

        assert_eq!(verified, signer.commitment_hex);
    }

    #[test]
    fn ecdsa_signature_without_public_key_is_rejected() {
        let summary = summary();
        let signer = TestEcdsaSigner::new();
        let signature = ecdsa(signer.sign_word(summary.to_commitment()), None);

        let error = verify_proposal_signature(&summary, &signature).unwrap_err();

        assert!(matches!(error, GuardianError::InvalidProposalSignature(_)));
    }

    #[test]
    fn ecdsa_signature_with_wrong_recovery_id_is_rejected() {
        let summary = summary();
        let signer = TestEcdsaSigner::new();
        let signature_hex = signer.sign_word(summary.to_commitment());
        let mut bytes = hex::decode(signature_hex.trim_start_matches("0x")).unwrap();
        bytes[64] ^= 1;
        let signature = ecdsa(
            format!("0x{}", hex::encode(bytes)),
            Some(signer.pubkey_hex.clone()),
        );

        let error = verify_proposal_signature(&summary, &signature).unwrap_err();

        assert!(matches!(error, GuardianError::InvalidProposalSignature(_)));
    }

    #[test]
    fn ecdsa_signature_with_another_public_key_is_rejected() {
        let summary = summary();
        let signer = TestEcdsaSigner::new();
        let other = TestEcdsaSigner::new();
        let signature = ecdsa(
            signer.sign_word(summary.to_commitment()),
            Some(other.pubkey_hex),
        );

        let error = verify_proposal_signature(&summary, &signature).unwrap_err();

        assert!(matches!(error, GuardianError::InvalidProposalSignature(_)));
    }

    #[test]
    fn eip712_signature_signs_the_typed_summary_hash() {
        let summary = summary();
        let signer = TestEcdsaSigner::new();
        let eip712 = |signature: String| ProposalSignature::Ecdsa {
            signature,
            public_key: Some(signer.pubkey_hex.clone()),
            message_format: EcdsaMessageFormat::Eip712,
        };

        let typed = eip712(signer.sign_prehash(summary.eip712_hash().into_bytes()));
        let raw = eip712(signer.sign_word(summary.to_commitment()));

        assert_eq!(
            verify_proposal_signature(&summary, &typed).unwrap(),
            signer.commitment_hex
        );
        assert!(matches!(
            verify_proposal_signature(&summary, &raw),
            Err(GuardianError::InvalidProposalSignature(_))
        ));
    }
}
