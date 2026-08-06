//! Payment transaction utilities.
//!
//! Functions for building P2ID (pay-to-id) and other payment transactions.

use miden_client::account::{Account, AccountInterfaceExt};
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::NoteType;
use miden_protocol::{Felt, Word};
use miden_standards::account::interface::AccountInterface;
use miden_standards::note::{P2idNote, P2ideNote, P2ideNoteStorage};

use crate::error::{MultisigError, Result};

/// Builds a P2ID transaction request.
///
/// Creates a pay-to-id note of the given `note_type` and builds a transaction
/// request to send it. Presence of `reclaim_height` and/or `timelock_height`
/// creates a P2IDE note instead of a plain P2ID note (issue #366); the note's
/// serial number is drawn from the same salt-seeded rng either way, so
/// cosigners rebuild the identical note.
#[expect(
    clippy::too_many_arguments,
    reason = "request building mirrors the proposal metadata fields one-to-one"
)]
pub fn build_p2id_transaction_request<I>(
    sender_account: &Account,
    recipient: AccountId,
    assets: Vec<Asset>,
    note_type: NoteType,
    reclaim_height: Option<u32>,
    timelock_height: Option<u32>,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let mut rng = RandomCoin::new(salt);

    let note = if reclaim_height.is_some() || timelock_height.is_some() {
        let storage = P2ideNoteStorage::new(
            recipient,
            reclaim_height.map(BlockNumber::from),
            timelock_height.map(BlockNumber::from),
        );
        P2ideNote::create(
            sender_account.id(),
            storage,
            assets,
            note_type,
            Default::default(),
            &mut rng,
        )
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to create P2IDE note: {}", e))
        })?
    } else {
        P2idNote::create(
            sender_account.id(),
            recipient,
            assets,
            note_type,
            Default::default(),
            &mut rng,
        )
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to create P2ID note: {}", e))
        })?
    };

    let send_script = AccountInterface::from_account(sender_account)
        .build_send_notes_script(&[note.clone().into()], None)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to build P2ID send script: {}", e))
        })?;

    let request = TransactionRequestBuilder::new()
        .custom_script(send_script)
        .expected_output_recipients(vec![note.recipient().clone()])
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_client::transaction::TransactionScriptTemplate;
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::account::auth::AuthScheme;
    use miden_protocol::account::{AccountId, AccountType};
    use miden_protocol::asset::{AssetAmount, TokenSymbol};
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_standards::AuthMethod;
    use miden_standards::account::access::AccessControl;
    use miden_standards::account::faucets::{FungibleFaucet, TokenName, create_fungible_faucet};
    use miden_standards::account::policies::TokenPolicyManager;

    #[test]
    fn build_p2id_transaction_request_uses_custom_send_script() {
        let secret_key = SecretKey::new();
        let signer_commitment = secret_key.public_key().to_commitment();
        let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
            1,
            vec![signer_commitment],
            Word::from([9u32, 8, 7, 6]),
        ))
        .build()
        .unwrap();
        let faucet_definition = FungibleFaucet::builder()
            .name(TokenName::new("test token").unwrap())
            .symbol(TokenSymbol::try_from("TST").unwrap())
            .decimals(8)
            .max_supply(AssetAmount::from(1_000_000u32))
            .build()
            .unwrap();
        let faucet = create_fungible_faucet(
            [5u8; 32],
            faucet_definition,
            AccountType::Public,
            AuthMethod::SingleSig {
                approver: (
                    secret_key.public_key().to_commitment().into(),
                    AuthScheme::Falcon512Poseidon2,
                ),
            },
            AccessControl::AuthControlled,
            TokenPolicyManager::new(),
        )
        .unwrap();
        let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let asset = miden_protocol::asset::FungibleAsset::new(faucet.id(), 100)
            .unwrap()
            .into();

        let request = build_p2id_transaction_request(
            &account,
            recipient,
            vec![asset],
            NoteType::Public,
            None,
            None,
            Word::from([1u32, 2, 3, 4]),
            std::iter::empty::<(Word, Vec<Felt>)>(),
        )
        .unwrap();

        assert!(matches!(
            request.script_template(),
            Some(TransactionScriptTemplate::CustomScript(_))
        ));
        assert_eq!(request.expected_output_recipients().count(), 1);
    }

    #[test]
    fn build_p2id_transaction_request_respects_note_type() {
        let secret_key = SecretKey::new();
        let signer_commitment = secret_key.public_key().to_commitment();
        let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
            1,
            vec![signer_commitment],
            Word::from([9u32, 8, 7, 6]),
        ))
        .build()
        .unwrap();
        let faucet_definition = FungibleFaucet::builder()
            .name(TokenName::new("test token").unwrap())
            .symbol(TokenSymbol::try_from("TST").unwrap())
            .decimals(8)
            .max_supply(AssetAmount::from(1_000_000u32))
            .build()
            .unwrap();
        let faucet = create_fungible_faucet(
            [5u8; 32],
            faucet_definition,
            AccountType::Public,
            AuthMethod::SingleSig {
                approver: (
                    secret_key.public_key().to_commitment().into(),
                    AuthScheme::Falcon512Poseidon2,
                ),
            },
            AccessControl::AuthControlled,
            TokenPolicyManager::new(),
        )
        .unwrap();
        let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let salt = Word::from([1u32, 2, 3, 4]);
        let build = |note_type: NoteType| {
            let asset: Asset = miden_protocol::asset::FungibleAsset::new(faucet.id(), 100)
                .unwrap()
                .into();
            build_p2id_transaction_request(
                &account,
                recipient,
                vec![asset],
                note_type,
                None,
                None,
                salt,
                std::iter::empty::<(Word, Vec<Felt>)>(),
            )
            .unwrap()
        };

        let private_request = build(NoteType::Private);
        let public_request = build(NoteType::Public);

        // The note type feeds the generated send script, so identically
        // parameterized public and private requests must not be identical.
        use miden_protocol::utils::serde::Serializable;
        assert_ne!(private_request.to_bytes(), public_request.to_bytes());
    }

    /// Presence of a reclaim/timelock height must switch the output note to
    /// P2IDE (issue #366): the note script and storage change, so the built
    /// request differs from a plain P2ID request; and the build must stay
    /// deterministic in the salt so cosigners rebuild the identical note.
    #[test]
    fn build_p2id_transaction_request_heights_select_p2ide() {
        let secret_key = SecretKey::new();
        let signer_commitment = secret_key.public_key().to_commitment();
        let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
            1,
            vec![signer_commitment],
            Word::from([9u32, 8, 7, 6]),
        ))
        .build()
        .unwrap();
        let faucet_definition = FungibleFaucet::builder()
            .name(TokenName::new("test token").unwrap())
            .symbol(TokenSymbol::try_from("TST").unwrap())
            .decimals(8)
            .max_supply(AssetAmount::from(1_000_000u32))
            .build()
            .unwrap();
        let faucet = create_fungible_faucet(
            [5u8; 32],
            faucet_definition,
            AccountType::Public,
            AuthMethod::SingleSig {
                approver: (
                    secret_key.public_key().to_commitment().into(),
                    AuthScheme::Falcon512Poseidon2,
                ),
            },
            AccessControl::AuthControlled,
            TokenPolicyManager::new(),
        )
        .unwrap();
        let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let salt = Word::from([1u32, 2, 3, 4]);
        let build = |reclaim: Option<u32>, timelock: Option<u32>| {
            let asset: Asset = miden_protocol::asset::FungibleAsset::new(faucet.id(), 100)
                .unwrap()
                .into();
            build_p2id_transaction_request(
                &account,
                recipient,
                vec![asset],
                NoteType::Public,
                reclaim,
                timelock,
                salt,
                std::iter::empty::<(Word, Vec<Felt>)>(),
            )
            .unwrap()
        };

        let recipient_digests = |request: &TransactionRequest| -> Vec<Word> {
            request
                .expected_output_recipients()
                .map(|r| r.digest())
                .collect()
        };

        let plain = recipient_digests(&build(None, None));
        let with_reclaim = recipient_digests(&build(Some(12345), None));
        let with_timelock = recipient_digests(&build(None, Some(700)));

        assert_ne!(plain, with_reclaim);
        assert_ne!(plain, with_timelock);
        assert_ne!(with_reclaim, with_timelock);

        // Deterministic in (salt, heights): a cosigner rebuilding from the
        // same metadata produces the identical output note.
        assert_eq!(recipient_digests(&build(Some(12345), None)), with_reclaim);
    }
}
