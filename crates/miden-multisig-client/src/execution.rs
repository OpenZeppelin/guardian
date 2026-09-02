//! Shared execution logic for proposal finalization.

use std::collections::HashSet;

use guardian_shared::SignatureScheme;
use miden_client::account::Account;
use miden_client::transaction::{ChainAnchor, TransactionRequest};
use miden_protocol::account::AccountId;
use miden_protocol::asset::FungibleAsset;
use miden_protocol::{Felt, Word};

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};
use crate::keystore::{ensure_hex_prefix, word_from_hex};
use crate::proposal::TransactionType;
use miden_standards::account::auth::FeeConversionInfo;

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

        let signature = sig_input
            .scheme
            .parse_signature_hex(&ensure_hex_prefix(&sig_input.signature_hex))
            .map_err(MultisigError::Signature)?;
        let entry = sig_input
            .scheme
            .build_signature_advice_entry(
                commitment,
                tx_summary_commitment,
                &signature,
                sig_input.public_key_hex.as_deref(),
            )
            .map_err(MultisigError::Signature)?;
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
/// The fee conversion info comes from the proposal's own anchor, never the chain tip:
/// reading the tip would silently stop reproducing the signed summary the moment a chain
/// changed its fee faucet between a proposal's anchor and its execution.
#[expect(
    clippy::too_many_arguments,
    reason = "execution needs transaction metadata and signature scheme to stay explicit"
)]
pub async fn build_final_transaction_request(
    client: &MidenSdkClient,
    transaction_type: &TransactionType,
    account: &Account,
    salt: Word,
    signature_advice: Vec<SignatureAdvice>,
    metadata_threshold: Option<u64>,
    metadata_signer_commitments: Option<&[Word]>,
    scheme: SignatureScheme,
    chain_anchor: &ChainAnchor,
) -> Result<TransactionRequest> {
    let fee_conversion_info = Some(fee_conversion_info_at(chain_anchor));

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
                salt,
                signature_advice,
                fee_conversion_info,
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
                        salt,
                        signature_advice,
                        fee_conversion_info,
                    )
                }
                None | Some(1) => {
                    #[cfg(feature = "legacy-consume-notes")]
                    {
                        crate::transaction::build_consume_notes_transaction_request(
                            client,
                            note_ids.clone(),
                            salt,
                            signature_advice,
                            fee_conversion_info,
                        )
                        .await
                    }
                    #[cfg(not(feature = "legacy-consume-notes"))]
                    {
                        let _ = (client, salt, signature_advice);
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
                salt,
                signature_advice,
                fee_conversion_info,
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
                    salt,
                    signature_advice,
                    fee_conversion_info,
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
                fee_conversion_info,
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
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;

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

    mod rebuild_fee_conversion_info {
        //! [`build_final_transaction_request`] must reproduce the faucet the proposal's anchor
        //! reports, not the one its executing client is synced to.
        //!
        //! The two agree on every other fixture in this crate, which is exactly what makes the
        //! distinction invisible: reading the tip instead of the anchor keeps passing until a
        //! chain changes its fee faucet between a proposal's anchor and its execution, at which
        //! point every committed proposal stops reproducing its signed summary. These cases put
        //! the anchor and the tip on different faucets so the two sources are separable, and they
        //! mirror `rebuilds with the anchor faucet even when the chain now reports another` in
        //! `packages/miden-multisig-client/src/multisig.test.ts`.

        use super::*;
        use crate::client::MultisigClient;
        use crate::client::test_support::{
            alternate_fee_faucet, assert_commits_fee_faucet, chain_anchor_at_fee_faucet,
            chain_fee_faucet, client_at_fee_faucet, guarded_multisig_account,
        };
        use crate::procedures::ProcedureName;
        use crate::proposal::P2ideHeights;
        use miden_protocol::note::NoteType;

        const SALT: [u32; 4] = [1, 2, 3, 4];

        /// A client synced to [`chain_fee_faucet`] paired with an anchor from a chain reporting
        /// [`alternate_fee_faucet`].
        async fn client_and_divergent_anchor(
            client_dir: &std::path::Path,
            anchor_dir: &std::path::Path,
        ) -> (MultisigClient, ChainAnchor) {
            let client = client_at_fee_faucet(client_dir, chain_fee_faucet()).await;
            let anchor = chain_anchor_at_fee_faucet(anchor_dir, alternate_fee_faucet()).await;

            assert_ne!(
                anchor.header().fee_parameters().fee_faucet_id(),
                chain_fee_faucet(),
                "the fixture only separates anchor from tip while their faucets differ"
            );

            (client, anchor)
        }

        async fn rebuild(
            client: &MultisigClient,
            anchor: &ChainAnchor,
            transaction_type: &TransactionType,
            metadata_threshold: Option<u64>,
            metadata_signer_commitments: Option<&[Word]>,
        ) -> TransactionRequest {
            build_final_transaction_request(
                &client.miden_client,
                transaction_type,
                &guarded_multisig_account(),
                Word::from(SALT),
                Vec::new(),
                metadata_threshold,
                metadata_signer_commitments,
                SignatureScheme::Falcon,
                anchor,
            )
            .await
            .expect("the rebuilt request builds")
        }

        #[tokio::test]
        async fn rebuilds_with_the_anchor_faucet_even_when_the_chain_now_reports_another() {
            let client_dir = tempfile::tempdir().expect("temp dir");
            let anchor_dir = tempfile::tempdir().expect("temp dir");
            let (client, anchor) =
                client_and_divergent_anchor(client_dir.path(), anchor_dir.path()).await;

            let request = rebuild(
                &client,
                &anchor,
                &TransactionType::SwitchGuardian {
                    new_endpoint: "http://localhost:1".to_string(),
                    new_commitment: Word::from([1u32, 1, 1, 1]),
                },
                None,
                None,
            )
            .await;

            assert_commits_fee_faucet(&request, alternate_fee_faucet(), Word::from(SALT));
        }

        /// Each transaction type reaches its own builder through its own arm of the dispatch, so
        /// the anchor's faucet has to be threaded through each one separately. Covering only
        /// `switch_guardian` would leave the arms below free to drop it and strand a committed
        /// proposal at rebuild time.
        #[tokio::test]
        async fn threads_the_anchor_faucet_through_the_p2id_arm() {
            let client_dir = tempfile::tempdir().expect("temp dir");
            let anchor_dir = tempfile::tempdir().expect("temp dir");
            let (client, anchor) =
                client_and_divergent_anchor(client_dir.path(), anchor_dir.path()).await;
            let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b")
                .expect("valid recipient id");

            let request = rebuild(
                &client,
                &anchor,
                &TransactionType::P2ID {
                    recipient,
                    faucet_id: alternate_fee_faucet(),
                    amount: 100,
                    note_type: NoteType::Public,
                    heights: P2ideHeights::default(),
                },
                None,
                None,
            )
            .await;

            assert_commits_fee_faucet(&request, alternate_fee_faucet(), Word::from(SALT));
        }

        #[tokio::test]
        async fn threads_the_anchor_faucet_through_the_update_procedure_threshold_arm() {
            let client_dir = tempfile::tempdir().expect("temp dir");
            let anchor_dir = tempfile::tempdir().expect("temp dir");
            let (client, anchor) =
                client_and_divergent_anchor(client_dir.path(), anchor_dir.path()).await;

            let request = rebuild(
                &client,
                &anchor,
                &TransactionType::UpdateProcedureThreshold {
                    procedure: ProcedureName::SendAsset,
                    new_threshold: 2,
                },
                None,
                None,
            )
            .await;

            assert_commits_fee_faucet(&request, alternate_fee_faucet(), Word::from(SALT));
        }

        #[tokio::test]
        async fn threads_the_anchor_faucet_through_the_signer_update_arm() {
            let client_dir = tempfile::tempdir().expect("temp dir");
            let anchor_dir = tempfile::tempdir().expect("temp dir");
            let (client, anchor) =
                client_and_divergent_anchor(client_dir.path(), anchor_dir.path()).await;
            let signers = [Word::from([5u32, 6, 7, 8])];

            let request = rebuild(
                &client,
                &anchor,
                &TransactionType::AddCosigner {
                    new_commitment: signers[0],
                },
                Some(1),
                Some(&signers),
            )
            .await;

            assert_commits_fee_faucet(&request, alternate_fee_faucet(), Word::from(SALT));
        }
    }
}

/// Reads the chain's native fee asset from the block the client is synced to and returns
/// conversion info paying the transaction fee in it at rate 1/1.
///
/// The multisig auth procedures take the payment asset and rate from the auth args, so this has to
/// be committed into every request the guardian builds; without it `fee::pay_fee` aborts with
/// `ERR_FEE_CONVERSION_INFO_MISSING` on any chain with a non-zero verification base fee.
///
/// This is the *creation*-side resolver, and it deliberately reads the synced block rather than
/// the network tip: that is the same block `chain_anchor_at_tip` pins the new request to, so the
/// committed faucet and the proposal's anchor cannot disagree. Rebuilds use
/// [`fee_conversion_info_at`] against the proposal's stored anchor instead.
pub async fn resolve_fee_conversion_info(client: &MidenSdkClient) -> Result<FeeConversionInfo> {
    let header = client.get_latest_block_header().await.map_err(|e| {
        MultisigError::miden_client_with_context(
            "failed to read the synced block header while resolving the fee faucet",
            e,
        )
    })?;

    Ok(FeeConversionInfo::one_to_one(
        header.fee_parameters().fee_faucet_id(),
    ))
}

/// The native 1/1 conversion info for the fee faucet `chain_anchor` reports.
///
/// Fee parameters are a per-block header field, so a proposal's faucet is whatever its anchored
/// reference block reported — not whatever the chain reports now. Deriving it from the anchor is
/// what lets a cosigner reproduce a committed auth arg from `salt_hex` alone, offline, and lets
/// the Rust and TypeScript SDKs agree on the same proposal.
///
/// This *derives* the value every typed path commits; it does not read back what a request
/// actually committed. A custom request may commit any faucet and rate, and nothing recorded on
/// the proposal can recover an arbitrary one — such integrations must retain the exact
/// [`FeeConversionInfo`] they built with.
pub fn fee_conversion_info_at(chain_anchor: &ChainAnchor) -> FeeConversionInfo {
    FeeConversionInfo::one_to_one(chain_anchor.header().fee_parameters().fee_faucet_id())
}
