//! Verification of cosigner approvals on Miden proposals.
//!
//! Falcon and raw ECDSA approvals sign the transaction-summary commitment;
//! EIP-712 approvals sign the summary's typed-data hash. ECDSA approvals
//! carry their public key, which execution needs to build the advice map.

use crate::delta_object::ProposalSignature;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Auth;
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

/// Accepts an approval only from a cosigner of `auth`, made under the
/// scheme the account verifies that cosigner's signatures with. A key's
/// commitment is derived under its own scheme, so a correctly registered
/// cosigner always passes; the scheme check refuses an approval from a key
/// the account registers under the other scheme, which the chain would
/// refuse on every execution.
pub(crate) fn ensure_cosigner_approval(
    auth: &Auth,
    approval_signer: &str,
    signature: &ProposalSignature,
) -> Result<()> {
    let Some(cosigner) = auth
        .cosigners()
        .into_iter()
        .find(|cosigner| cosigner.commitment.eq_ignore_ascii_case(approval_signer))
    else {
        return Err(GuardianError::InvalidProposalSignature(
            "Approval signer is not a cosigner of this account".to_string(),
        ));
    };
    if cosigner.scheme != signature.scheme() {
        return Err(GuardianError::InvalidProposalSignature(format!(
            "Approval is signed under {}, but this account verifies its signer under {}",
            signature.scheme(),
            cosigner.scheme
        )));
    }
    Ok(())
}

/// Decodes an optionally `0x`-prefixed hex string into `T`.
fn parse_hex<T: Deserializable>(value: &str, label: &str) -> Result<T> {
    let bytes = hex::decode(value.trim_start_matches("0x"))
        .map_err(|e| GuardianError::InvalidProposalSignature(format!("Invalid {label}: {e}")))?;
    T::read_from_bytes(&bytes)
        .map_err(|e| GuardianError::InvalidProposalSignature(format!("Invalid {label}: {e}")))
}

/// Formats a key commitment the way account metadata stores it.
fn commitment_hex(commitment: Word) -> String {
    format!("0x{}", hex::encode(commitment.to_bytes()))
}

/// The rejection for a well-formed signature that does not verify.
fn invalid_signature() -> GuardianError {
    GuardianError::InvalidProposalSignature("Signature does not match the proposal".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::helpers::{TestEcdsaSigner, TestSigner, create_test_delta_payload};

    const ACCOUNT_ID: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

    /// The transaction summary every test approval signs.
    fn summary() -> TransactionSummary {
        TransactionSummary::from_json(&create_test_delta_payload(ACCOUNT_ID)).unwrap()
    }

    /// A raw ECDSA approval.
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

    fn falcon_approval(signer: &TestSigner) -> ProposalSignature {
        ProposalSignature::Falcon {
            signature: signer.sign_word(summary().to_commitment()),
        }
    }

    fn ecdsa_approval(signer: &TestEcdsaSigner) -> ProposalSignature {
        ecdsa(
            signer.sign_word(summary().to_commitment()),
            Some(signer.pubkey_hex.clone()),
        )
    }

    fn mixed(falcon: &TestSigner, ecdsa: &TestEcdsaSigner) -> Auth {
        Auth::Miden {
            cosigners: vec![
                crate::metadata::auth::MidenCosigner {
                    commitment: falcon.commitment_hex.clone(),
                    scheme: guardian_shared::SignatureScheme::Falcon,
                },
                crate::metadata::auth::MidenCosigner {
                    commitment: ecdsa.commitment_hex.clone(),
                    scheme: guardian_shared::SignatureScheme::Ecdsa,
                },
            ],
            guardian_scheme: guardian_shared::SignatureScheme::Ecdsa,
        }
    }

    #[test]
    fn cosigner_approval_under_its_registered_scheme_is_accepted() {
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let single_falcon = Auth::MidenFalconRpo {
            cosigner_commitments: vec![falcon.commitment_hex.clone()],
        };
        let single_ecdsa = Auth::MidenEcdsa {
            cosigner_commitments: vec![ecdsa.commitment_hex.clone()],
        };

        ensure_cosigner_approval(
            &single_falcon,
            &falcon.commitment_hex,
            &falcon_approval(&falcon),
        )
        .unwrap();
        ensure_cosigner_approval(
            &single_ecdsa,
            &ecdsa.commitment_hex,
            &ecdsa_approval(&ecdsa),
        )
        .unwrap();
        let mixed = mixed(&falcon, &ecdsa);
        ensure_cosigner_approval(&mixed, &falcon.commitment_hex, &falcon_approval(&falcon))
            .unwrap();
        ensure_cosigner_approval(&mixed, &ecdsa.commitment_hex, &ecdsa_approval(&ecdsa)).unwrap();
        // Commitments match case-insensitively, as membership always has.
        ensure_cosigner_approval(
            &mixed,
            &ecdsa
                .commitment_hex
                .to_ascii_uppercase()
                .replacen("0X", "0x", 1),
            &ecdsa_approval(&ecdsa),
        )
        .unwrap();
    }

    #[test]
    fn approval_from_a_non_cosigner_is_refused() {
        let falcon = TestSigner::new();
        let outsider = TestSigner::new();
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec![falcon.commitment_hex.clone()],
        };

        let error =
            ensure_cosigner_approval(&auth, &outsider.commitment_hex, &falcon_approval(&outsider))
                .unwrap_err();

        assert!(
            matches!(&error, GuardianError::InvalidProposalSignature(message) if message.contains("not a cosigner")),
            "{error:?}"
        );
    }

    #[test]
    fn approval_under_the_other_scheme_than_registered_is_refused() {
        // An ECDSA key registered under Falcon (a mis-built client): its
        // ECDSA approval verifies to the registered commitment, but the
        // chain would verify it as Falcon and refuse the transaction.
        let ecdsa = TestEcdsaSigner::new();
        let falcon = TestSigner::new();
        let registered_as_falcon = Auth::MidenFalconRpo {
            cosigner_commitments: vec![ecdsa.commitment_hex.clone()],
        };
        let mixed_wrong = Auth::Miden {
            cosigners: vec![
                crate::metadata::auth::MidenCosigner {
                    commitment: ecdsa.commitment_hex.clone(),
                    scheme: guardian_shared::SignatureScheme::Falcon,
                },
                crate::metadata::auth::MidenCosigner {
                    commitment: falcon.commitment_hex.clone(),
                    scheme: guardian_shared::SignatureScheme::Falcon,
                },
            ],
            guardian_scheme: guardian_shared::SignatureScheme::Ecdsa,
        };

        for auth in [registered_as_falcon, mixed_wrong] {
            let error =
                ensure_cosigner_approval(&auth, &ecdsa.commitment_hex, &ecdsa_approval(&ecdsa))
                    .unwrap_err();
            assert!(
                matches!(
                    &error,
                    GuardianError::InvalidProposalSignature(message)
                        if message == "Approval is signed under ecdsa, but this account verifies its signer under falcon"
                ),
                "{error:?}"
            );
        }
    }
}
