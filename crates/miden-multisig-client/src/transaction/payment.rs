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
    use miden_protocol::Felt;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::AccountStorageMode;
    use miden_protocol::account::auth::AuthScheme;
    use miden_protocol::asset::TokenSymbol;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_standards::AuthMethod;
    use miden_standards::account::faucets::create_basic_fungible_faucet;

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
        let faucet = create_basic_fungible_faucet(
            [5u8; 32],
            TokenSymbol::try_from("TST").unwrap(),
            8,
            Felt::from(1_000_000u32),
            AccountStorageMode::Public,
            AuthMethod::SingleSig {
                approver: (
                    secret_key.public_key().to_commitment().into(),
                    AuthScheme::Falcon512Poseidon2,
                ),
            },
        )
        .unwrap();
        let recipient = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();
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
