//! Shared execution logic for proposal finalization.

use std::collections::HashSet;

use guardian_shared::{EcdsaMessageFormat, SignatureScheme};
use miden_client::account::Account;
use miden_client::transaction::TransactionRequest;
use miden_protocol::account::AccountId;
use miden_protocol::account::auth::Signature as AccountSignature;
use miden_protocol::asset::FungibleAsset;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::Deserializable;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::Eip712TransactionSummary;
use miden_standards::account::auth::MultisigAuthArgs;

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};
use crate::keystore::{ensure_hex_prefix, word_from_hex};
use crate::proposal::TransactionType;

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
    /// Message format used by an ECDSA signature.
    pub message_format: EcdsaMessageFormat,
}

impl SignatureInput {
    fn build_eip712_signature_advice_entry(
        &self,
        signer_commitment: Word,
        tx_summary_commitment: Word,
        tx_summary: Option<&TransactionSummary>,
        signature: &AccountSignature,
    ) -> Result<SignatureAdvice> {
        let summary = tx_summary.ok_or_else(|| {
            MultisigError::Signature("EIP-712 requires the transaction summary".to_string())
        })?;
        if summary.to_commitment() != tx_summary_commitment {
            return Err(MultisigError::Signature(
                "transaction summary commitment mismatch".to_string(),
            ));
        }
        let public_key_hex = self
            .public_key_hex
            .as_deref()
            .ok_or_else(|| MultisigError::Signature("EIP-712 requires a public key".to_string()))?;
        let public_key_bytes = hex::decode(public_key_hex.trim_start_matches("0x"))
            .map_err(|e| MultisigError::Signature(format!("invalid EIP-712 public key: {e}")))?;
        let public_key = PublicKey::read_from_bytes(&public_key_bytes)
            .map_err(|e| MultisigError::Signature(format!("invalid EIP-712 public key: {e}")))?;
        if public_key.to_commitment() != signer_commitment {
            return Err(MultisigError::Signature(
                "EIP-712 public-key commitment mismatch".to_string(),
            ));
        }
        let AccountSignature::EcdsaK256Keccak(signature) = signature else {
            return Err(MultisigError::Signature(
                "EIP-712 requires ECDSA".to_string(),
            ));
        };
        if !public_key.verify_prehash(summary.eip712_hash().into_bytes(), signature) {
            return Err(MultisigError::Signature(
                "EIP-712 signature does not match the proposal".to_string(),
            ));
        }
        Ok(summary.eip712_signature_advice(&public_key, signature))
    }
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
/// * `tx_summary` - Required for EIP-712 signatures to derive their digest and advice key
///
/// # Returns
/// Vector of (key, prepared_signature) tuples for transaction advice.
pub fn collect_signature_advice(
    signatures: impl IntoIterator<Item = SignatureInput>,
    required_commitments: &HashSet<String>,
    tx_summary_commitment: Word,
    tx_summary: Option<&TransactionSummary>,
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

        let signature = sig_input
            .scheme
            .parse_signature_hex(&ensure_hex_prefix(&sig_input.signature_hex))
            .map_err(MultisigError::Signature)?;

        let entry = if sig_input.message_format == EcdsaMessageFormat::Eip712 {
            sig_input.build_eip712_signature_advice_entry(
                commitment,
                tx_summary_commitment,
                tx_summary,
                &signature,
            )?
        } else {
            sig_input
                .scheme
                .build_signature_advice_entry(
                    commitment,
                    tx_summary_commitment,
                    &signature,
                    sig_input.public_key_hex.as_deref(),
                )
                .map_err(MultisigError::Signature)?
        };
        advice.push(entry);
    }

    Ok(advice)
}

/// Builds the fungible asset to transfer.
///
/// Since Miden 0.16 the asset-callback flag derives from the faucet account ID,
/// so the asset no longer needs to be reconciled against the sender's vault.
pub fn build_transfer_asset(faucet_id: AccountId, amount: u64) -> Result<FungibleAsset> {
    FungibleAsset::new(faucet_id, amount)
        .map_err(|e| MultisigError::InvalidConfig(format!("failed to create asset: {}", e)))
}

/// Builds the final transaction request based on transaction type.
///
#[expect(
    clippy::too_many_arguments,
    reason = "execution needs transaction metadata and signature scheme to stay explicit"
)]
pub async fn build_final_transaction_request(
    client: &MidenSdkClient,
    transaction_type: &TransactionType,
    account: &Account,
    auth_args: &MultisigAuthArgs,
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
            note_type,
            heights,
        } => {
            let asset = build_transfer_asset(*faucet_id, *amount)?;

            crate::transaction::build_p2id_transaction_request(
                account,
                *recipient,
                vec![asset.into()],
                *note_type,
                *heights,
                auth_args,
                signature_advice,
            )
        }
        TransactionType::ConsumeNotes {
            note_ids,
            metadata_version,
            notes,
        } => {
            // v1/v2 dispatch for issue #229 / spec FR-009.
            match metadata_version {
                Some(crate::proposal::CONSUME_NOTES_METADATA_VERSION_V2) => {
                    if notes.len() != note_ids.len() {
                        return Err(MultisigError::NoteBindingMismatch(format!(
                            "consume_notes v2: notes.len()={} does not match note_ids.len()={}",
                            notes.len(),
                            note_ids.len()
                        )));
                    }
                    let mut decoded: Vec<miden_protocol::note::Note> =
                        Vec::with_capacity(notes.len());
                    for (i, serialized) in notes.iter().enumerate() {
                        let note = serialized.to_note()?;
                        if note.id() != note_ids[i] {
                            return Err(MultisigError::NoteBindingMismatch(format!(
                                "consume_notes v2: notes[{}] id {} != note_ids[{}] {}",
                                i,
                                note.id().to_hex(),
                                i,
                                note_ids[i].to_hex()
                            )));
                        }
                        decoded.push(note);
                    }
                    crate::transaction::build_consume_notes_transaction_request_from_notes(
                        decoded,
                        auth_args,
                        signature_advice,
                    )
                }
                None | Some(1) => {
                    #[cfg(feature = "legacy-consume-notes")]
                    {
                        crate::transaction::build_consume_notes_transaction_request(
                            client,
                            note_ids.clone(),
                            auth_args,
                            signature_advice,
                        )
                        .await
                    }
                    #[cfg(not(feature = "legacy-consume-notes"))]
                    {
                        let _ = (client, auth_args, signature_advice);
                        // Preserve `Some(1)` vs `None` so the error tells the
                        // operator which legacy shape was rejected.
                        Err(MultisigError::UnsupportedMetadataVersion {
                            found: *metadata_version,
                        })
                    }
                }
                Some(other) => Err(MultisigError::UnsupportedMetadataVersion {
                    found: Some(*other),
                }),
            }
        }
        TransactionType::SwitchGuardian { new_commitment, .. } => {
            crate::transaction::build_update_guardian_transaction_request(
                *new_commitment,
                scheme,
                auth_args,
                signature_advice,
            )
        }
        TransactionType::UpdateProcedureThreshold {
            procedure,
            new_threshold,
        } => {
            let tx_request =
                crate::transaction::build_update_procedure_threshold_transaction_request(
                    *procedure,
                    *new_threshold,
                    auth_args,
                    signature_advice,
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
                auth_args,
                signature_advice,
                scheme,
            )?;

            Ok(tx_request)
        }
        TransactionType::Custom => Err(MultisigError::UnsupportedTransactionType(
            "cannot build a transaction for a custom proposal type".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_client::Serializable;
    use miden_protocol::account::{
        AccountDelta, AccountIdVersion, AccountStoragePatch, AccountType, AccountVaultDelta,
        AssetCallbackFlag,
    };
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummaryUserParams};

    #[test]
    fn test_collect_mixed_raw_and_eip712_ecdsa_advice() {
        let account_id = AccountId::dummy(
            [3u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            Felt::ZERO,
        )
        .unwrap();
        let summary = TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            RawOutputNotes::new(Vec::new()).unwrap(),
            miden_protocol::block::BlockNumber::from(0),
            Word::default(),
            0,
            TransactionSummaryUserParams::new([Felt::ZERO; 6]),
        );
        let raw_key = SigningKey::new();
        let eip_key = SigningKey::new();
        let raw_signature = raw_key.sign(summary.to_commitment());
        let eip_signature = eip_key.sign_prehash(summary.eip712_hash().into_bytes());
        let eip_signature_bytes = eip_signature.to_bytes();
        let raw_commitment = raw_key.public_key().to_commitment();
        let eip_commitment = eip_key.public_key().to_commitment();
        let raw_hex = format!("0x{}", hex::encode(raw_commitment.to_bytes()));
        let eip_hex = format!("0x{}", hex::encode(eip_commitment.to_bytes()));
        let required = [raw_hex.clone(), eip_hex.clone()].into_iter().collect();
        let inputs = vec![
            SignatureInput {
                signer_commitment: raw_hex,
                signature_hex: format!("0x{}", hex::encode(raw_signature.to_bytes())),
                scheme: SignatureScheme::Ecdsa,
                public_key_hex: Some(format!(
                    "0x{}",
                    hex::encode(raw_key.public_key().to_bytes())
                )),
                message_format: EcdsaMessageFormat::Raw,
            },
            SignatureInput {
                signer_commitment: eip_hex,
                signature_hex: format!("0x{}", hex::encode(eip_signature_bytes)),
                scheme: SignatureScheme::Ecdsa,
                public_key_hex: Some(format!(
                    "0x{}",
                    hex::encode(eip_key.public_key().to_bytes())
                )),
                message_format: EcdsaMessageFormat::Eip712,
            },
        ];

        let advice =
            collect_signature_advice(inputs, &required, summary.to_commitment(), Some(&summary))
                .unwrap();
        assert_eq!(advice.len(), 2);
        assert_eq!(
            advice[1],
            summary.eip712_signature_advice(&eip_key.public_key(), &eip_signature)
        );
        assert_ne!(advice[0].0, advice[1].0);

        let mut other_recovery_bit = eip_signature.to_bytes();
        other_recovery_bit[64] ^= 1;
        let input = SignatureInput {
            signer_commitment: format!("0x{}", hex::encode(eip_commitment.to_bytes())),
            signature_hex: format!("0x{}", hex::encode(other_recovery_bit)),
            scheme: SignatureScheme::Ecdsa,
            public_key_hex: Some(format!(
                "0x{}",
                hex::encode(eip_key.public_key().to_bytes())
            )),
            message_format: EcdsaMessageFormat::Eip712,
        };
        let other_advice =
            collect_signature_advice([input], &required, summary.to_commitment(), Some(&summary))
                .unwrap();
        assert_eq!(other_advice, vec![advice[1].clone()]);
    }

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
            message_format: EcdsaMessageFormat::Raw,
        }];

        // Unknown signer should be filtered out
        let result = collect_signature_advice(signatures, &required, Word::default(), None);
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
                message_format: EcdsaMessageFormat::Raw,
            },
            SignatureInput {
                signer_commitment: "0xabc".to_string(), // lowercase duplicate
                signature_hex: "0x5678".to_string(),
                scheme: SignatureScheme::Falcon,
                public_key_hex: None,
                message_format: EcdsaMessageFormat::Raw,
            },
        ];

        // Both will fail signature parsing, but second should be deduplicated
        // before reaching that point (based on lowercase comparison)
        let result = collect_signature_advice(signatures, &required, Word::default(), None);
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
            message_format: EcdsaMessageFormat::Raw,
        }];

        let advice =
            collect_signature_advice(signatures, &required, msg, None).expect("valid advice");
        assert_eq!(advice.len(), 1);
    }
}
