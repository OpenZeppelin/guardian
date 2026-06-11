//! Payment transaction utilities.
//!
//! Functions for building P2ID (pay-to-id) and other payment transactions.

use miden_client::account::{Account, AccountInterfaceExt};
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::NoteType;
use miden_protocol::{Felt, Word};
use miden_standards::account::interface::AccountInterface;
use miden_standards::note::P2idNote;

use crate::error::{MultisigError, Result};

/// Builds a P2ID transaction request.
///
/// Creates a pay-to-id note and builds a transaction request to send it.
pub fn build_p2id_transaction_request<I>(
    sender_account: &Account,
    recipient: AccountId,
    assets: Vec<Asset>,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let mut rng = RandomCoin::new(salt);

    let note = P2idNote::create(
        sender_account.id(),
        recipient,
        assets,
        NoteType::Public,
        Default::default(),
        &mut rng,
    )
    .map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to create P2ID note: {}", e))
    })?;

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
}
