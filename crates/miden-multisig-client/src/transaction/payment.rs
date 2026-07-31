//! Payment transaction utilities.
//!
//! Functions for building P2ID (pay-to-id) and other payment transactions.

use miden_client::account::Account;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder, TransactionScript};
use miden_protocol::account::{AccountCodeInterface, AccountId};
use miden_protocol::asset::Asset;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::NoteType;
use miden_protocol::{Felt, Word};
use miden_standards::note::P2idNote;
use miden_standards::tx_script::SendNotesTransactionScript;

use crate::error::{MultisigError, Result};

/// Builds a P2ID transaction request.
///
/// Creates a pay-to-id note of the given `note_type` and builds a transaction
/// request to send it.
pub fn build_p2id_transaction_request<I>(
    sender_account: &Account,
    recipient: AccountId,
    assets: Vec<Asset>,
    note_type: NoteType,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let mut rng = RandomCoin::new(salt);

    let note = P2idNote::builder()
        .sender(sender_account.id())
        .target(recipient)
        .assets(assets)
        .note_type(note_type)
        .generate_serial_number(&mut rng)
        .build()
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to create P2ID note: {}", e))
        })?;
    let note = miden_protocol::note::Note::from(note);

    let interface = AccountCodeInterface::new(
        sender_account.id(),
        sender_account.code().procedures().iter().copied().collect(),
    )
    .map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to build account interface: {}", e))
    })?;

    let send_script: TransactionScript =
        SendNotesTransactionScript::new(&interface, &[note.clone().into()])
            .map_err(|e| {
                MultisigError::TransactionExecution(format!(
                    "failed to build P2ID send script: {}",
                    e
                ))
            })?
            .into();

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
    use miden_standards::account::auth::{Approver, AuthSingleSigAcl, AuthSingleSigAclConfig};
    use miden_standards::account::faucets::{
        FungibleFaucet, TokenName, create_singlesig_user_fungible_faucet,
    };
    use miden_standards::account::policies::{
        BurnPolicy, MintPolicy, TokenPolicyManager, TransferPolicy,
    };

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
        let auth_component = AuthSingleSigAcl::new(
            Approver::new(
                secret_key.public_key().to_commitment().into(),
                AuthScheme::Falcon512Poseidon2,
            ),
            AuthSingleSigAclConfig::new(std::collections::BTreeSet::from([
                FungibleFaucet::receive_and_burn_root(),
            ]))
            .unwrap(),
        );
        let policy_manager = TokenPolicyManager::builder()
            .active_mint_policy(MintPolicy::allow_all())
            .active_burn_policy(BurnPolicy::allow_all())
            .active_send_policy(TransferPolicy::allow_all())
            .active_receive_policy(TransferPolicy::allow_all())
            .build();
        let faucet = create_singlesig_user_fungible_faucet(
            [5u8; 32],
            faucet_definition,
            auth_component,
            policy_manager,
            AccountType::Public,
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
        let auth_component = AuthSingleSigAcl::new(
            Approver::new(
                secret_key.public_key().to_commitment().into(),
                AuthScheme::Falcon512Poseidon2,
            ),
            AuthSingleSigAclConfig::new(std::collections::BTreeSet::from([
                FungibleFaucet::receive_and_burn_root(),
            ]))
            .unwrap(),
        );
        let policy_manager = TokenPolicyManager::builder()
            .active_mint_policy(MintPolicy::allow_all())
            .active_burn_policy(BurnPolicy::allow_all())
            .active_send_policy(TransferPolicy::allow_all())
            .active_receive_policy(TransferPolicy::allow_all())
            .build();
        let faucet = create_singlesig_user_fungible_faucet(
            [5u8; 32],
            faucet_definition,
            auth_component,
            policy_manager,
            AccountType::Public,
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
}
