//! Payment transaction utilities.
//!
//! Functions for building P2ID (pay-to-id) and other payment transactions.

use miden_client::account::Account;
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
use miden_protocol::account::{AccountCodeInterface, AccountId};
use miden_protocol::asset::Asset;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::NoteType;
use miden_protocol::{Felt, Word};
use miden_standards::note::{P2idNote, P2ideNote};
use miden_standards::tx_script::SendNotesTransactionScript;

use super::MaybeFeeConversionInfo;
use crate::error::{MultisigError, Result};
use crate::proposal::P2ideHeights;
use miden_standards::account::auth::FeeConversionInfo;

/// Builds a P2ID transaction request.
///
/// Creates a pay-to-id note of the given `note_type` and builds a transaction
/// request to send it. When `heights` carries a reclaim and/or timelock
/// constraint, a P2IDE note is created instead of a plain P2ID note (issue
/// #366); the note's serial number is drawn from the same salt-seeded rng
/// either way, so cosigners rebuild the identical note.
///
/// `fee_conversion_info` decides what the auth arg carries. `Some(info)` commits
/// `hash(CONVERSION_INFO || SALT)` and supplies the preimage through the advice map, which is
/// what the guarded auth procedure's `fee::pay_fee` requires; `None` passes the salt through
/// bare, reproducing the pre-0.16 behaviour, which aborts at proving with
/// `ERR_FEE_CONVERSION_INFO_MISSING` wherever the verification base fee is non-zero.
///
/// Resolve the value with [`crate::resolve_fee_conversion_info`] when building a new request,
/// or with [`crate::fee_conversion_info_at`] when rebuilding one for an existing proposal —
/// the auth arg commits it, so a rebuild has to reproduce what was committed rather than what
/// the chain reports now.
pub fn build_p2id_transaction_request<I>(
    sender_account: &Account,
    recipient: AccountId,
    assets: Vec<Asset>,
    note_type: NoteType,
    heights: P2ideHeights,
    salt: Word,
    signature_advice: I,
    fee_conversion_info: Option<FeeConversionInfo>,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let mut rng = RandomCoin::new(salt);

    let note: miden_protocol::note::Note = if heights.is_p2ide() {
        P2ideNote::builder()
            .sender(sender_account.id())
            .target(recipient)
            .maybe_reclaim_height(heights.reclaim.map(|h| BlockNumber::from(h.get())))
            .maybe_timelock_height(heights.timelock.map(|h| BlockNumber::from(h.get())))
            .assets(assets)
            .note_type(note_type)
            .generate_serial_number(&mut rng)
            .build()
            .map_err(|e| {
                MultisigError::TransactionExecution(format!("failed to create P2IDE note: {}", e))
            })?
            .into()
    } else {
        P2idNote::builder()
            .sender(sender_account.id())
            .target(recipient)
            .assets(assets)
            .note_type(note_type)
            .generate_serial_number(&mut rng)
            .build()
            .map_err(|e| {
                MultisigError::TransactionExecution(format!("failed to create P2ID note: {}", e))
            })?
            .into()
    };

    let interface = AccountCodeInterface::new(
        sender_account.id(),
        sender_account.code().procedures().iter().copied().collect(),
    )
    .map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to build account interface: {}", e))
    })?;

    let send_notes_script = SendNotesTransactionScript::new(&interface, &[note.clone().into()])
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to build P2ID send script: {}", e))
        })?;

    let request = TransactionRequestBuilder::new()
        .custom_script(send_notes_script.tx_script().clone())
        .script_arg(send_notes_script.tx_script_args())
        .expected_output_recipients(vec![note.recipient().clone()])
        .extend_advice_map(signature_advice)
        .maybe_fee_conversion_info(fee_conversion_info, salt)
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
    use miden_standards::account::auth::{
        Approver, AuthSingleSig, FeeConversionInfo, commit_fee_conversion_info,
    };
    use miden_standards::account::faucets::{
        FungibleFaucet, TokenName, create_singlesig_user_fungible_faucet,
    };
    use miden_standards::account::policies::{
        BurnPolicy, MintPolicy, TokenPolicyManager, TransferPolicy,
    };

    /// Builds a guarded multisig account and a fungible faucet for the request builders.
    fn guarded_account_and_faucet() -> (
        miden_client::account::Account,
        miden_client::account::Account,
    ) {
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
        let auth_component = AuthSingleSig::new(Approver::new(
            secret_key.public_key().to_commitment().into(),
            AuthScheme::Falcon512Poseidon2,
        ));
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
        (account, faucet)
    }

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
        let auth_component = AuthSingleSig::new(Approver::new(
            secret_key.public_key().to_commitment().into(),
            AuthScheme::Falcon512Poseidon2,
        ));
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
            P2ideHeights::default(),
            Word::from([1u32, 2, 3, 4]),
            std::iter::empty::<(Word, Vec<Felt>)>(),
            None,
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
        let auth_component = AuthSingleSig::new(Approver::new(
            secret_key.public_key().to_commitment().into(),
            AuthScheme::Falcon512Poseidon2,
        ));
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
                P2ideHeights::default(),
                salt,
                std::iter::empty::<(Word, Vec<Felt>)>(),
                None,
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

    /// A request built with fee conversion info must commit it via the auth args, not pass the
    /// salt through bare.
    ///
    /// Since protocol 0.16 the guarded multisig auth procedure pays the transaction fee and reads
    /// the conversion info from the auth args, so a bare salt aborts in `fee::pay_fee` with
    /// `ERR_FEE_CONVERSION_INFO_MISSING` on any chain with a non-zero verification base fee.
    #[test]
    fn p2id_request_commits_fee_conversion_info() {
        let (account, faucet) = guarded_account_and_faucet();
        let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let asset = miden_protocol::asset::FungibleAsset::new(faucet.id(), 100)
            .unwrap()
            .into();
        let salt = Word::from([1u32, 2, 3, 4]);
        let conversion_info = FeeConversionInfo::one_to_one(faucet.id());

        let request = build_p2id_transaction_request(
            &account,
            recipient,
            vec![asset],
            NoteType::Public,
            P2ideHeights::default(),
            salt,
            std::iter::empty::<(Word, Vec<Felt>)>(),
            Some(conversion_info),
        )
        .unwrap();

        let (expected_auth_arg, expected_preimage) =
            commit_fee_conversion_info(conversion_info, salt);

        assert_eq!(*request.auth_arg(), Some(expected_auth_arg));
        assert_ne!(
            *request.auth_arg(),
            Some(salt),
            "the salt must not be passed through bare"
        );
        assert_eq!(
            request
                .advice_map()
                .get(&expected_auth_arg)
                .map(|v| v.to_vec()),
            Some(expected_preimage),
            "the commitment preimage must be reachable from the advice map"
        );
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
        let auth_component = AuthSingleSig::new(Approver::new(
            secret_key.public_key().to_commitment().into(),
            AuthScheme::Falcon512Poseidon2,
        ));
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
        let build = |reclaim: Option<u32>, timelock: Option<u32>| {
            let asset: Asset = miden_protocol::asset::FungibleAsset::new(faucet.id(), 100)
                .unwrap()
                .into();
            let heights = P2ideHeights {
                reclaim: reclaim.and_then(std::num::NonZeroU32::new),
                timelock: timelock.and_then(std::num::NonZeroU32::new),
            };
            build_p2id_transaction_request(
                &account,
                recipient,
                vec![asset],
                NoteType::Public,
                heights,
                salt,
                std::iter::empty::<(Word, Vec<Felt>)>(),
                None,
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
