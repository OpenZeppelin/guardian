use std::collections::BTreeSet;
use std::sync::Arc;

use miden_client::rpc::NodeRpcClient;
use miden_client::testing::mock::MockRpcApi;
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::testing::account_id::ACCOUNT_ID_PRIVATE_SENDER;
use miden_testing::{Auth, MockChainBuilder};
use miden_tx::DataStore;

use super::{
    ChainViewError, ExecutionDataStore, ForeignAccountUnavailable, ForeignAccounts,
    build_chain_view,
};

struct Fixture {
    rpc: Arc<MockRpcApi>,
    wallet: miden_protocol::account::Account,
    faucet: miden_protocol::account::Account,
}

fn fixture(blocks: u32) -> Fixture {
    let mut builder = MockChainBuilder::new();
    let wallet = builder.add_existing_wallet(Auth::IncrNonce).unwrap();
    let faucet = builder
        .add_existing_basic_faucet(Auth::IncrNonce, "GRD", 1_000_000, Some(0))
        .unwrap();
    let rpc = Arc::new(MockRpcApi::new(builder.build().unwrap()));
    rpc.advance_blocks(blocks);
    Fixture {
        rpc,
        wallet,
        faucet,
    }
}

fn tracked(blocks: &[u32]) -> BTreeSet<BlockNumber> {
    blocks.iter().copied().map(BlockNumber::from).collect()
}

/// The wire value of the one IES scheme the client accepts; the client keeps its enum private.
const X25519_XCHACHA20_POLY1305: u32 = 1;

fn served_key(
    attestations: Vec<miden_client::rpc::encryption::ValidatorAttestation>,
) -> miden_client::rpc::encryption::AttestedTransactionEncryptionKey {
    miden_client::rpc::encryption::AttestedTransactionEncryptionKey {
        scheme: X25519_XCHACHA20_POLY1305,
        key_id: vec![1],
        public_key: vec![7; 32],
        attestations,
        next_key: None,
    }
}

#[tokio::test]
async fn a_served_key_without_any_attestation_fails_sealing() {
    let f = fixture(3);
    let view = build_chain_view(f.rpc.as_ref(), &BTreeSet::new())
        .await
        .unwrap();

    match super::sealing::trusted_key(served_key(Vec::new()), &view) {
        Err(super::sealing::SealingFailed::Attestation(message)) => {
            assert!(message.contains("attestation"), "{message}")
        }
        other => panic!("expected an attestation failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_key_attested_by_a_validator_the_chain_does_not_name_fails_sealing() {
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;

    let f = fixture(3);
    let view = build_chain_view(f.rpc.as_ref(), &BTreeSet::new())
        .await
        .unwrap();
    let outsider = SigningKey::new();
    let unsigned = served_key(Vec::new());
    let commitment = miden_client::rpc::encryption::attestation_commitment(
        unsigned.scheme,
        &unsigned.key_id,
        view.genesis_commitment(),
        &unsigned.public_key,
        None,
    );
    assert!(
        !view
            .reference_header()
            .validator_config()
            .keys()
            .contains(&outsider.public_key())
    );
    let attested = served_key(vec![miden_client::rpc::encryption::ValidatorAttestation {
        validator_key: outsider.public_key(),
        signature: outsider.sign(commitment),
    }]);

    assert!(matches!(
        super::sealing::trusted_key(attested, &view),
        Err(super::sealing::SealingFailed::Attestation(_))
    ));
}

#[tokio::test]
async fn the_chain_view_executes_at_the_tip_and_proves_an_old_bound_block() {
    let f = fixture(60);
    let tip = f.rpc.get_chain_tip_block_num();
    let bound = BlockNumber::from(3);
    let view = build_chain_view(f.rpc.as_ref(), &BTreeSet::from([bound]))
        .await
        .unwrap();

    assert_eq!(view.reference_block(), tip);
    assert!(tip.as_u32() >= bound.as_u32() + 50);
    assert!(view.authenticates(bound));
    assert!(view.authenticates(tip));
    assert!(!view.authenticates(BlockNumber::from(4)));
    assert_eq!(
        view.blockchain().peaks().hash_peaks(),
        view.reference_header().chain_commitment()
    );
    assert_eq!(
        view.blockchain().get_block(bound).unwrap().commitment(),
        f.rpc
            .get_block_header_by_number(Some(bound), false)
            .await
            .unwrap()
            .0
            .commitment()
    );
}

#[tokio::test]
async fn the_protocol_config_is_the_one_the_reference_header_commits_to() {
    let f = fixture(5);
    let view = build_chain_view(f.rpc.as_ref(), &BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(view.protocol_config(), &f.rpc.protocol_config());
    assert_eq!(
        view.protocol_config().to_commitment(),
        view.reference_header().protocol_config_commitment()
    );
}

#[tokio::test]
async fn a_node_behind_the_bound_block_is_reported_as_chain_behind() {
    let f = fixture(5);
    let tip = f.rpc.get_chain_tip_block_num();
    let ahead = BlockNumber::from(tip.as_u32() + 10);
    match build_chain_view(f.rpc.as_ref(), &BTreeSet::from([ahead])).await {
        Err(ChainViewError::ChainBehind {
            tip: seen,
            required,
        }) => {
            assert_eq!(seen, tip);
            assert_eq!(required, ahead);
        }
        other => panic!("expected chain behind, got {other:?}"),
    }
}

#[tokio::test]
async fn the_data_store_serves_only_the_attempt_reference_block_and_tracked_blocks() {
    let f = fixture(20);
    let view = build_chain_view(f.rpc.as_ref(), &tracked(&[2, 7]))
        .await
        .unwrap();
    let reference = view.reference_block();
    let expected_config = view.protocol_config().clone();
    let store = ExecutionDataStore::new(f.wallet.clone(), view, f.rpc.clone(), &[]);

    let (account, header, config, blockchain) = store
        .get_transaction_inputs(
            f.wallet.id(),
            BTreeSet::from([BlockNumber::from(7), reference]),
        )
        .await
        .unwrap();
    assert_eq!(account.id(), f.wallet.id());
    assert_eq!(header.block_num(), reference);
    assert_eq!(config, expected_config);
    assert!(blockchain.contains_block(BlockNumber::from(2)));
    assert!(blockchain.contains_block(BlockNumber::from(7)));

    assert!(
        store
            .get_transaction_inputs(
                f.wallet.id(),
                BTreeSet::from([BlockNumber::from(5), reference])
            )
            .await
            .is_err(),
        "an untracked block cannot be authenticated"
    );
    assert!(
        store
            .get_transaction_inputs(
                f.wallet.id(),
                BTreeSet::from([BlockNumber::from(reference.as_u32() + 1)])
            )
            .await
            .is_err(),
        "the reference block is fixed for the attempt"
    );
    assert!(
        store
            .get_transaction_inputs(f.faucet.id(), BTreeSet::from([reference]))
            .await
            .is_err(),
        "only the executing account is served as the native account"
    );
}

#[tokio::test]
async fn own_vault_witnesses_require_the_current_vault_root() {
    let f = fixture(2);
    let view = build_chain_view(f.rpc.as_ref(), &BTreeSet::new())
        .await
        .unwrap();
    let store = ExecutionDataStore::new(f.wallet.clone(), view, f.rpc.clone(), &[]);
    let root = f.wallet.vault().root();
    assert!(
        store
            .get_vault_asset_witnesses(f.wallet.id(), root, BTreeSet::new())
            .await
            .is_ok()
    );
    assert!(
        store
            .get_vault_asset_witnesses(
                f.wallet.id(),
                miden_protocol::Word::default(),
                BTreeSet::new()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_public_foreign_account_loads_at_the_reference_block() {
    let f = fixture(4);
    let view = build_chain_view(f.rpc.as_ref(), &BTreeSet::new())
        .await
        .unwrap();
    let reference = view.reference_block();
    let store = ExecutionDataStore::new(f.wallet.clone(), view, f.rpc.clone(), &[]);

    let inputs = store
        .get_foreign_account_inputs(f.faucet.id(), reference)
        .await
        .unwrap();
    assert_eq!(inputs.id(), f.faucet.id());
    assert_eq!(inputs.code().commitment(), f.faucet.code().commitment());
    let again = store
        .get_foreign_account_inputs(f.faucet.id(), reference)
        .await
        .unwrap();
    assert_eq!(again.id(), inputs.id());
    assert!(
        store
            .get_vault_asset_witnesses(f.faucet.id(), f.faucet.vault().root(), BTreeSet::new())
            .await
            .is_ok(),
        "a loaded foreign vault answers witness queries"
    );
    assert!(store.foreign_failure().is_none());
}

#[tokio::test]
async fn a_private_foreign_account_is_refused_with_its_reason() {
    let f = fixture(2);
    let reference = f.rpc.get_chain_tip_block_num();
    let foreign = ForeignAccounts::new(f.rpc.clone(), reference);
    let private = AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap();

    assert_eq!(
        foreign.inputs(private, reference).await.unwrap_err(),
        ForeignAccountUnavailable::Private {
            account_id: private
        }
    );
    assert_eq!(
        foreign.failure(),
        Some(ForeignAccountUnavailable::Private {
            account_id: private
        })
    );
}

#[tokio::test]
async fn a_foreign_account_is_never_read_at_another_block() {
    let f = fixture(4);
    let reference = f.rpc.get_chain_tip_block_num();
    let foreign = ForeignAccounts::new(f.rpc.clone(), reference);
    let earlier = BlockNumber::from(reference.as_u32() - 1);
    assert!(matches!(
        foreign.inputs(f.faucet.id(), earlier).await.unwrap_err(),
        ForeignAccountUnavailable::Unavailable { .. }
    ));
}

mod guarded_multisig {
    use super::*;
    use guardian_shared::SignatureScheme;
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::account::auth::AuthSecretKey;
    use miden_protocol::account::{Account, AccountType};
    use miden_protocol::crypto::SequentialCommit;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_protocol::transaction::{InputNotes, TransactionArgs, TransactionScript};
    use miden_protocol::{Felt, Word};
    use miden_standards::account::auth::{AuthGuardedMultisig, MultisigAuthArgs};
    use miden_standards::code_builder::CodeBuilder;
    use miden_testing::MockChainBuilder;
    use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
    use miden_tx::{TransactionExecutor, TransactionExecutorError};

    struct Proposal {
        account: Account,
        cosigners: Vec<SecretKey>,
        script: TransactionScript,
        auth_args: MultisigAuthArgs,
    }

    fn rotation_proposal(bound: BlockNumber) -> Proposal {
        let cosigners: Vec<SecretKey> = (0..2).map(|_| SecretKey::new()).collect();
        let commitments: Vec<Word> = cosigners
            .iter()
            .map(|key| key.public_key().to_commitment())
            .collect();
        let guardian = SecretKey::new().public_key().to_commitment();
        let config = MultisigGuardianConfig::new(2, commitments, guardian)
            .with_account_type(AccountType::Public)
            .with_signature_scheme(SignatureScheme::Falcon);
        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .expect("multisig account builds");

        let new_guardian = SecretKey::new().public_key().to_commitment();
        let scheme_id = SignatureScheme::Falcon.auth_scheme_id();
        let script = CodeBuilder::new()
            .with_dynamically_linked_package(AuthGuardedMultisig::code())
            .expect("library links")
            .compile_tx_script(format!(
                "@transaction_script\npub proc main\n    push.{new_guardian}\n    push.{scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
            ))
            .expect("tx script compiles");
        let auth_args = MultisigAuthArgs::new(bound, Word::from([Felt::new_unchecked(7); 4]));
        Proposal {
            account,
            cosigners,
            script,
            auth_args,
        }
    }

    fn tx_args(proposal: &Proposal) -> TransactionArgs {
        let commitment = proposal.auth_args.to_commitment();
        let mut args = TransactionArgs::new(Default::default())
            .with_tx_script(proposal.script.clone())
            .with_auth_args(commitment);
        args.extend_advice_map([(commitment, proposal.auth_args.to_elements())]);
        args
    }

    async fn unsigned_summary(
        store: &ExecutionDataStore,
        proposal: &Proposal,
    ) -> Result<miden_protocol::transaction::TransactionSummary, TransactionExecutorError> {
        let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
            TransactionExecutor::new(store);
        match executor
            .execute_transaction(
                proposal.account.id(),
                store.chain().reference_block(),
                InputNotes::default(),
                tx_args(proposal),
            )
            .await
        {
            Err(TransactionExecutorError::Unauthorized(summary)) => Ok(*summary),
            Err(error) => Err(error),
            Ok(_) => panic!("an unsigned multisig transaction must not execute"),
        }
    }

    #[tokio::test]
    async fn a_proposal_bound_to_an_old_block_reproduces_and_executes_at_the_tip() {
        let bound = BlockNumber::from(3);
        let proposal = rotation_proposal(bound);
        let mut chain = MockChainBuilder::with_accounts([proposal.account.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain.prove_until_block(bound).unwrap();

        let signed_at_bound = match chain
            .build_transaction(proposal.account.id())
            .authenticator(None)
            .tx_script(proposal.script.clone())
            .add_advice_map_entry(
                proposal.auth_args.to_commitment(),
                proposal.auth_args.to_elements(),
            )
            .auth_args(proposal.auth_args.to_commitment())
            .build()
            .unwrap()
            .execute()
            .await
            .unwrap_err()
        {
            TransactionExecutorError::Unauthorized(summary) => *summary,
            error => panic!("expected the unsigned abort: {error:?}"),
        };

        let rpc = Arc::new(MockRpcApi::new(chain));
        rpc.advance_blocks(60);
        let view = build_chain_view(rpc.as_ref(), &BTreeSet::from([bound]))
            .await
            .unwrap();
        let reference = view.reference_block();
        assert!(reference.as_u32() > bound.as_u32() + 50);
        let store = ExecutionDataStore::new(proposal.account.clone(), view, rpc.clone(), &[]);

        let reproduced = unsigned_summary(&store, &proposal).await.unwrap();
        assert_eq!(reproduced.block_number(), bound);
        assert_eq!(
            reproduced.to_commitment(),
            signed_at_bound.to_commitment(),
            "the summary reproduces at a later reference block"
        );

        let message = reproduced.to_commitment();
        let signing = SigningInputs::TransactionSummary(Box::new(reproduced));
        let mut signed_args = tx_args(&proposal);
        for key in &proposal.cosigners {
            let signature =
                BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(key.clone())])
                    .get_signature(key.public_key().to_commitment().into(), &signing)
                    .await
                    .expect("cosigner signs");
            signed_args.add_signature(key.public_key().into(), message, signature);
        }
        let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
            TransactionExecutor::new(&store);
        let executed = executor
            .execute_transaction(
                proposal.account.id(),
                reference,
                InputNotes::default(),
                signed_args,
            )
            .await
            .expect("the signed transaction executes at the tip");
        assert_eq!(executed.block_header().block_num(), reference);
        assert_eq!(
            executed.final_account().nonce(),
            proposal.account.nonce() + Felt::ONE
        );
    }

    const FOREIGN_COMPONENT: &str = "
        use miden::protocol::active_account

        @account_procedure
        pub proc get_id
            exec.active_account::get_id
            swapw dropw
        end
    ";

    /// A public account whose only procedure returns its own id, for a script to invoke.
    fn foreign_account() -> (Account, miden_protocol::account::AccountComponent) {
        use miden_protocol::account::{AccountBuilder, AccountComponent, AccountComponentMetadata};

        let component = AccountComponent::new(
            CodeBuilder::default()
                .compile_component_code("foreign_account", FOREIGN_COMPONENT)
                .expect("foreign component compiles"),
            Vec::new(),
            AccountComponentMetadata::mock("foreign_account"),
        )
        .expect("foreign component builds");
        let account = AccountBuilder::new([21; 32])
            .with_components(Auth::IncrNonce)
            .with_component(component.clone())
            .account_type(AccountType::Public)
            .build_existing()
            .expect("foreign account builds");
        (account, component)
    }

    /// A guardian-key rotation whose script first reads the foreign account's id through
    /// foreign procedure invocation and asserts it.
    fn rotation_reading_a_foreign_account(bound: BlockNumber) -> (Proposal, Account) {
        let mut proposal = rotation_proposal(bound);
        let (foreign, component) = foreign_account();
        let new_guardian = SecretKey::new().public_key().to_commitment();
        let scheme_id = SignatureScheme::Falcon.auth_scheme_id();
        proposal.script = CodeBuilder::new()
            .with_dynamically_linked_package(AuthGuardedMultisig::code())
            .expect("library links")
            .with_dynamically_linked_package(component.component_code())
            .expect("foreign component links")
            .compile_tx_script(format!(
                "use miden::core::sys
                use miden::protocol::tx
                use miden::protocol::account_id

                @transaction_script
                pub proc main
                    padw padw padw push.0.0.0
                    procref.::foreign_account::get_id
                    push.{prefix} push.{suffix}
                    exec.tx::execute_foreign_procedure
                    push.{prefix} push.{suffix}
                    exec.account_id::eq
                    assert.err=\"the foreign account returned another id\"
                    exec.sys::truncate_stack
                    push.{new_guardian}
                    push.{scheme_id}
                    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key
                    drop
                    dropw
                end",
                prefix = foreign.id().prefix().as_felt(),
                suffix = foreign.id().suffix(),
            ))
            .expect("tx script compiles");
        (proposal, foreign)
    }

    async fn signed_at_bound_with_foreign(
        proposal: &Proposal,
        foreign: &Account,
        bound: BlockNumber,
    ) -> (
        miden_testing::MockChain,
        miden_protocol::transaction::TransactionSummary,
    ) {
        let mut chain =
            MockChainBuilder::with_accounts([proposal.account.clone(), foreign.clone()])
                .unwrap()
                .build()
                .unwrap();
        chain.prove_until_block(bound).unwrap();
        let foreign_inputs = chain.get_foreign_account_inputs(foreign.clone()).unwrap();
        let summary = match chain
            .build_transaction(proposal.account.id())
            .authenticator(None)
            .foreign_accounts(vec![foreign_inputs])
            .tx_script(proposal.script.clone())
            .add_advice_map_entry(
                proposal.auth_args.to_commitment(),
                proposal.auth_args.to_elements(),
            )
            .auth_args(proposal.auth_args.to_commitment())
            .build()
            .unwrap()
            .execute()
            .await
            .unwrap_err()
        {
            TransactionExecutorError::Unauthorized(summary) => *summary,
            error => panic!("expected the unsigned abort: {error:?}"),
        };
        (chain, summary)
    }

    #[tokio::test]
    async fn a_script_invoking_a_public_foreign_account_reproduces_and_executes_at_the_tip() {
        let bound = BlockNumber::from(3);
        let (proposal, foreign) = rotation_reading_a_foreign_account(bound);
        let (chain, signed_at_bound) =
            signed_at_bound_with_foreign(&proposal, &foreign, bound).await;

        let rpc = Arc::new(MockRpcApi::new(chain));
        rpc.advance_blocks(60);
        let view = build_chain_view(rpc.as_ref(), &BTreeSet::from([bound]))
            .await
            .unwrap();
        let reference = view.reference_block();
        let store = ExecutionDataStore::new(proposal.account.clone(), view, rpc.clone(), &[]);

        let reproduced = unsigned_summary(&store, &proposal).await.unwrap();
        assert_eq!(
            reproduced.to_commitment(),
            signed_at_bound.to_commitment(),
            "the summary reproduces with the foreign account read at the reference block"
        );
        assert!(store.foreign_failure().is_none());

        let message = reproduced.to_commitment();
        let signing = SigningInputs::TransactionSummary(Box::new(reproduced));
        let mut signed_args = tx_args(&proposal);
        for key in &proposal.cosigners {
            let signature =
                BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(key.clone())])
                    .get_signature(key.public_key().to_commitment().into(), &signing)
                    .await
                    .expect("cosigner signs");
            signed_args.add_signature(key.public_key().into(), message, signature);
        }
        let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
            TransactionExecutor::new(&store);
        let executed = executor
            .execute_transaction(
                proposal.account.id(),
                reference,
                InputNotes::default(),
                signed_args,
            )
            .await
            .expect("the signed transaction executes with its foreign account");
        assert_eq!(executed.block_header().block_num(), reference);
    }

    #[tokio::test]
    async fn a_foreign_account_whose_state_the_node_no_longer_serves_is_unavailable() {
        let bound = BlockNumber::from(3);
        let (proposal, foreign) = rotation_reading_a_foreign_account(bound);
        let (chain, _) = signed_at_bound_with_foreign(&proposal, &foreign, bound).await;

        let rpc = Arc::new(MockRpcApi::new(chain));
        rpc.advance_blocks(60);
        let view = build_chain_view(rpc.as_ref(), &BTreeSet::from([bound]))
            .await
            .unwrap();
        let reference = view.reference_block();
        let store = ExecutionDataStore::new(proposal.account.clone(), view, rpc.clone(), &[]);
        rpc.prove_block();
        rpc.prune_account_state_at(reference);

        assert!(unsigned_summary(&store, &proposal).await.is_err());
        match store.foreign_failure() {
            Some(ForeignAccountUnavailable::Unavailable { account_id, reason }) => {
                assert_eq!(account_id, foreign.id());
                assert!(reason.contains(&reference.to_string()), "{reason}");
            }
            other => panic!("expected the foreign account to be unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_untracked_bound_block_cannot_be_authenticated() {
        let bound = BlockNumber::from(3);
        let proposal = rotation_proposal(bound);
        let mut chain = MockChainBuilder::with_accounts([proposal.account.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain.prove_until_block(bound).unwrap();
        let rpc = Arc::new(MockRpcApi::new(chain));
        rpc.advance_blocks(10);
        let view = build_chain_view(rpc.as_ref(), &BTreeSet::new())
            .await
            .unwrap();
        let store = ExecutionDataStore::new(proposal.account.clone(), view, rpc.clone(), &[]);
        assert!(
            unsigned_summary(&store, &proposal).await.is_err(),
            "the executor never asks for the bound block, so it must be tracked up front"
        );
    }
}

mod executor {
    use std::num::NonZeroU32;

    use super::*;
    use guardian_shared::{FromJson, ProposalSignature, SignatureScheme, ToJson};
    use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::account::{Account, AccountType};
    use miden_protocol::crypto::SequentialCommit;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_protocol::note::Note;
    use miden_protocol::transaction::{TransactionScript, TransactionSummary};
    use miden_protocol::utils::serde::{Deserializable, Serializable};
    use miden_protocol::{Felt, Word};
    use miden_standards::account::auth::{AuthGuardedMultisig, MultisigAuthArgs};
    use miden_standards::code_builder::CodeBuilder;
    use miden_tx::{LocalTransactionProver, TransactionExecutorError};

    use crate::delta_object::CosignerSignature;
    use crate::network::miden::execution::aborts::{APPROVAL_EXPIRED, Abort, VAULT_SHORTFALL};
    use crate::network::miden::execution::{MidenExecutor, StoredRequest};
    use crate::services::execute_proposal::{ExecutionInput, ProposalExecutor};
    use crate::services::execution_codec::TransactionRequestEnvelope;
    use crate::storage::ExecutionFailureCode;
    use crate::storage::execution::{ExpirationBound, RequestInvalidReason};

    struct Proposal {
        account: Account,
        cosigners: Vec<SecretKey>,
        request_bytes: Vec<u8>,
        summary: TransactionSummary,
        rpc: Arc<MockRpcApi>,
        script: TransactionScript,
        auth_commitment: Word,
        auth_elements: Vec<Felt>,
    }

    const BOUND: u32 = 3;

    /// A guarded 2-of-2 guardian-key rotation proposed at block 3, then left while 60 more
    /// blocks are produced. The rotation needs only the cosigners' signatures.
    async fn proposal(declare_bound_block: bool) -> Proposal {
        proposal_expiring_after(declare_bound_block, 1_000).await
    }

    async fn proposal_expiring_after(declare_bound_block: bool, approval_blocks: u32) -> Proposal {
        let cosigners: Vec<SecretKey> = (0..2).map(|_| SecretKey::new()).collect();
        let commitments: Vec<Word> = cosigners
            .iter()
            .map(|key| key.public_key().to_commitment())
            .collect();
        let config = MultisigGuardianConfig::new(
            2,
            commitments,
            SecretKey::new().public_key().to_commitment(),
        )
        .with_account_type(AccountType::Public)
        .with_signature_scheme(SignatureScheme::Falcon);
        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .unwrap();

        let new_guardian = SecretKey::new().public_key().to_commitment();
        let scheme_id = SignatureScheme::Falcon.auth_scheme_id();
        let script = CodeBuilder::new()
            .with_dynamically_linked_package(AuthGuardedMultisig::code())
            .unwrap()
            .compile_tx_script(format!(
                "@transaction_script\npub proc main\n    push.{new_guardian}\n    push.{scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
            ))
            .unwrap();
        let bound = BlockNumber::from(BOUND);
        let auth_args = MultisigAuthArgs::new(bound, Word::from([Felt::new_unchecked(9); 4]));
        let auth_args = match NonZeroU32::new(approval_blocks) {
            Some(blocks) => auth_args.with_approval_expiration_delta(blocks).unwrap(),
            None => auth_args,
        };
        let commitment = auth_args.to_commitment();

        let mut chain = MockChainBuilder::with_accounts([account.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain.prove_until_block(bound).unwrap();
        let summary = match chain
            .build_transaction(account.id())
            .authenticator(None)
            .tx_script(script.clone())
            .add_advice_map_entry(commitment, auth_args.to_elements())
            .auth_args(commitment)
            .build()
            .unwrap()
            .execute()
            .await
            .unwrap_err()
        {
            TransactionExecutorError::Unauthorized(summary) => *summary,
            error => panic!("expected the unsigned abort: {error:?}"),
        };

        let mut builder = TransactionRequestBuilder::new()
            .custom_script(script.clone())
            .auth_arg(commitment)
            .extend_advice_map([(commitment, auth_args.to_elements())]);
        if declare_bound_block {
            builder = builder.block_numbers([bound]);
        }
        let request_bytes = builder.build().unwrap().to_bytes();

        let rpc = Arc::new(MockRpcApi::new(chain));
        rpc.advance_blocks(60);
        Proposal {
            account,
            cosigners,
            request_bytes,
            summary,
            rpc,
            script,
            auth_commitment: commitment,
            auth_elements: auth_args.to_elements(),
        }
    }

    impl Proposal {
        fn signed_request(&self) -> TransactionRequestBuilder {
            TransactionRequestBuilder::new()
                .custom_script(self.script.clone())
                .block_numbers([BlockNumber::from(BOUND)])
        }

        fn with_request(
            &self,
            request: TransactionRequestBuilder,
            signers: &[usize],
        ) -> ExecutionInput {
            let mut input = self.input(signers);
            input.proposal_payload["transaction_request"] = serde_json::json!(
                TransactionRequestEnvelope::seal(&request.build().unwrap().to_bytes())
            );
            input
        }

        async fn refusal(&self, input: ExecutionInput) -> ExecutionFailureCode {
            match self.executor().prepare(input).await {
                Err(failure) => failure.code,
                Ok(_) => panic!("the request must be refused"),
            }
        }

        fn input(&self, signers: &[usize]) -> ExecutionInput {
            let message = self.summary.to_commitment();
            let cosigner_signatures = signers
                .iter()
                .map(|index| {
                    let key = &self.cosigners[*index];
                    CosignerSignature {
                        signer_id: format!(
                            "0x{}",
                            hex::encode(key.public_key().to_commitment().to_bytes())
                        ),
                        signature: ProposalSignature::Falcon {
                            signature: format!("0x{}", hex::encode(key.sign(message).to_bytes())),
                        },
                        timestamp: "2026-09-30T12:00:00Z".to_string(),
                    }
                })
                .collect();
            ExecutionInput {
                account_id: self.account.id().to_hex(),
                state_json: self.account.to_json(),
                proposal_payload: serde_json::json!({
                    "tx_summary": self.summary.to_json(),
                    "metadata": { "proposal_type": "switch_guardian" },
                    "transaction_request": TransactionRequestEnvelope::seal(
                        &self.request_bytes),
                }),
                cosigner_signatures,
            }
        }

        fn executor(&self) -> MidenExecutor {
            MidenExecutor::new(
                self.rpc.clone(),
                Arc::new(LocalTransactionProver::default()),
            )
        }
    }

    /// MAST deserialization is not canonical (two decodes of the same bytes re-encode
    /// differently), so the mirror is held to semantic equality under the upstream decoder.
    #[tokio::test]
    async fn the_stored_request_re_encodes_to_an_equal_upstream_request() {
        let proposal = proposal(true).await;
        let decoded = StoredRequest::decode(&proposal.request_bytes).unwrap();
        assert_eq!(
            TransactionRequest::read_from_bytes(&decoded.to_bytes()).unwrap(),
            TransactionRequest::read_from_bytes(&proposal.request_bytes).unwrap()
        );
        assert!(decoded.block_numbers().contains(&BlockNumber::from(BOUND)));
        decoded.check_against(&proposal.summary).unwrap();
    }

    #[tokio::test]
    async fn signature_selection_counts_only_distinct_valid_cosigners() {
        let proposal = proposal(true).await;
        let executor = proposal.executor();
        let both = executor
            .select_signatures(&proposal.input(&[0, 1]))
            .unwrap();
        assert_eq!((both.required, both.valid, both.ignored), (2, 2, 0));
        assert!(both.is_ready());

        let duplicated = executor
            .select_signatures(&proposal.input(&[0, 0]))
            .unwrap();
        assert_eq!((duplicated.valid, duplicated.ignored), (1, 1));
        assert!(!duplicated.is_ready());

        let mut forged = proposal.input(&[0, 1]);
        forged.cosigner_signatures[1].signature = forged.cosigner_signatures[0].signature.clone();
        let forged = executor.select_signatures(&forged).unwrap();
        assert_eq!(
            (forged.valid, forged.ignored),
            (1, 1),
            "a signature under another key is ignored"
        );

        let mut invalid_first = proposal.input(&[0, 0, 1]);
        invalid_first.cosigner_signatures[0].signature =
            invalid_first.cosigner_signatures[2].signature.clone();
        let invalid_first = executor.select_signatures(&invalid_first).unwrap();
        assert_eq!(
            (invalid_first.valid, invalid_first.ignored),
            (2, 1),
            "an invalid entry does not claim its signer ahead of the signer's valid one"
        );
        assert!(invalid_first.is_ready());

        let outsider = SecretKey::new();
        let mut with_outsider = proposal.input(&[0, 1]);
        with_outsider.cosigner_signatures.push(CosignerSignature {
            signer_id: format!(
                "0x{}",
                hex::encode(outsider.public_key().to_commitment().to_bytes())
            ),
            signature: ProposalSignature::Falcon {
                signature: format!(
                    "0x{}",
                    hex::encode(outsider.sign(proposal.summary.to_commitment()).to_bytes())
                ),
            },
            timestamp: "2026-09-30T12:00:00Z".to_string(),
        });
        let with_outsider = executor.select_signatures(&with_outsider).unwrap();
        assert_eq!((with_outsider.valid, with_outsider.ignored), (2, 1));
        assert!(
            with_outsider.is_ready(),
            "a bad entry beside enough valid ones does not block"
        );
    }

    #[tokio::test]
    async fn only_signatures_guardian_holds_count_towards_the_threshold() {
        let proposal = proposal(true).await;
        let held = proposal
            .executor()
            .select_signatures(&proposal.input(&[0]))
            .unwrap();
        assert_eq!((held.required, held.valid), (2, 1));
        assert!(
            !held.is_ready(),
            "a signature collected elsewhere does not exist for Guardian"
        );
    }

    #[tokio::test]
    async fn a_request_that_does_not_reproduce_the_signed_summary_is_a_binding_mismatch() {
        let proposal = proposal(true).await;
        let other_guardian = SecretKey::new().public_key().to_commitment();
        let scheme_id = SignatureScheme::Falcon.auth_scheme_id();
        let other_script = CodeBuilder::new()
            .with_dynamically_linked_package(AuthGuardedMultisig::code())
            .unwrap()
            .compile_tx_script(format!(
                "@transaction_script\npub proc main\n    push.{other_guardian}\n    push.{scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
            ))
            .unwrap();
        let request = TransactionRequestBuilder::new()
            .custom_script(other_script)
            .block_numbers([BlockNumber::from(BOUND)])
            .auth_arg(proposal.auth_commitment)
            .extend_advice_map([(proposal.auth_commitment, proposal.auth_elements.clone())]);
        assert_eq!(
            proposal
                .refusal(proposal.with_request(request, &[0, 1]))
                .await,
            ExecutionFailureCode::BindingMismatch
        );
    }

    #[tokio::test]
    async fn a_request_without_auth_args_or_their_preimage_is_refused() {
        let proposal = proposal(true).await;
        let without_auth_arg = proposal
            .signed_request()
            .extend_advice_map([(proposal.auth_commitment, proposal.auth_elements.clone())]);
        let without_preimage = proposal.signed_request().auth_arg(proposal.auth_commitment);
        for request in [without_auth_arg, without_preimage] {
            assert_eq!(
                proposal
                    .refusal(proposal.with_request(request, &[0, 1]))
                    .await,
                ExecutionFailureCode::RequestInvalid(RequestInvalidReason::AuthArgsMissing)
            );
        }
    }

    #[tokio::test]
    async fn a_proposal_without_an_approval_expiration_is_refused() {
        let proposal = proposal_expiring_after(true, 0).await;
        assert_eq!(
            proposal.refusal(proposal.input(&[0, 1])).await,
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::ApprovalExpirationMissing)
        );
    }

    #[tokio::test]
    async fn a_request_consuming_unpinned_notes_is_refused() {
        let proposal = proposal(true).await;
        let request = proposal
            .signed_request()
            .auth_arg(proposal.auth_commitment)
            .extend_advice_map([(proposal.auth_commitment, proposal.auth_elements.clone())])
            .input_notes([(
                Note::mock_noop(Word::from([Felt::new_unchecked(7); 4])),
                None,
            )]);
        assert_eq!(
            proposal
                .refusal(proposal.with_request(request, &[0, 1]))
                .await,
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::InputNotesNotPinned)
        );
    }

    #[tokio::test]
    async fn blocks_the_request_declares_beyond_the_signed_ones_are_not_fetched() {
        let proposal = proposal(true).await;
        let request = proposal
            .signed_request()
            .auth_arg(proposal.auth_commitment)
            .extend_advice_map([(proposal.auth_commitment, proposal.auth_elements.clone())])
            .block_numbers([BlockNumber::from(BOUND), BlockNumber::from(u32::MAX)]);
        assert!(
            proposal
                .executor()
                .prepare(proposal.with_request(request, &[0, 1]))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn signatures_below_the_enforced_threshold_are_insufficient_not_a_mismatch() {
        let proposal = proposal(true).await;
        let mut attempt = proposal
            .executor()
            .prepare(proposal.input(&[0]))
            .await
            .unwrap();
        let failure = attempt.execute(None).await.unwrap_err();
        assert_eq!(failure.code, ExecutionFailureCode::InsufficientSignatures);
        assert!(
            failure.message.contains("1 valid signatures"),
            "{}",
            failure.message
        );
    }

    #[tokio::test]
    async fn a_node_behind_the_bound_block_is_chain_behind() {
        let mut proposal = proposal(true).await;
        let mut chain = MockChainBuilder::with_accounts([proposal.account.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain.prove_until_block(BlockNumber::from(1)).unwrap();
        proposal.rpc = Arc::new(MockRpcApi::new(chain));
        assert_eq!(
            proposal.refusal(proposal.input(&[0, 1])).await,
            ExecutionFailureCode::ChainBehind
        );
    }

    #[tokio::test]
    async fn a_stored_proposal_reproduces_executes_and_proves_at_the_tip() {
        let proposal = proposal(true).await;
        let executor = proposal.executor();
        let mut attempt = executor.prepare(proposal.input(&[0, 1])).await.unwrap();
        assert!(attempt.reference_block() > BOUND + 50);
        assert!(
            !attempt.requires_guardian_ack(),
            "a guardian rotation needs no acknowledgment"
        );
        assert_eq!(
            TransactionSummary::from_json(attempt.summary_payload())
                .unwrap()
                .to_commitment(),
            proposal.summary.to_commitment()
        );

        let executed = attempt.execute(None).await.unwrap();
        assert!(executed.final_account_commitment.starts_with("0x"));
        let proven = attempt.prove().await.unwrap();
        assert_eq!(proven.reference_block, attempt.reference_block());
        assert!(proven.expiration_block > proven.reference_block);
        assert!(
            proven.expiration_block <= BOUND + 1_000,
            "the approval bound caps the expiration"
        );
    }

    #[tokio::test]
    async fn a_request_that_does_not_declare_its_bound_block_is_refused_before_any_chain_read() {
        let proposal = proposal(false).await;
        let Err(failure) = proposal.executor().prepare(proposal.input(&[0, 1])).await else {
            panic!("a request without its bound block must be refused");
        };
        assert_eq!(
            failure.code,
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::BoundBlockNotDeclared)
        );
    }

    #[tokio::test]
    async fn an_approval_expired_before_the_tip_is_refused_before_reproduction() {
        let proposal = proposal_expiring_after(true, 10).await;
        let Err(failure) = proposal.executor().prepare(proposal.input(&[0, 1])).await else {
            panic!("an expired approval must be refused");
        };
        assert_eq!(
            failure.code,
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval)
        );
    }

    #[tokio::test]
    async fn the_auth_procedure_abort_on_an_expired_approval_is_recognised() {
        let proposal = proposal_expiring_after(true, 10).await;
        let request = StoredRequest::decode(&proposal.request_bytes).unwrap();
        let mut chain = MockChainBuilder::with_accounts([proposal.account.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain
            .prove_until_block(BlockNumber::from(BOUND + 20))
            .unwrap();
        let inputs = request.execution_inputs(&proposal.account, []).unwrap();
        let error = chain
            .build_transaction(proposal.account.id())
            .authenticator(None)
            .tx_script(inputs.tx_args.tx_script().unwrap().clone())
            .extend_advice_inputs(inputs.tx_args.advice_inputs().clone())
            .auth_args(inputs.tx_args.auth_args())
            .build()
            .unwrap()
            .execute()
            .await
            .unwrap_err();
        assert_eq!(Abort::of(&error), Some(Abort::ApprovalExpired), "{error}");
    }

    #[test]
    fn the_pinned_abort_messages_match_upstream() {
        assert_eq!(
            APPROVAL_EXPIRED,
            miden_standards::errors::standards::ERR_MULTISIG_APPROVAL_EXPIRED.message()
        );
        assert_eq!(
            VAULT_SHORTFALL,
            miden_protocol::errors::tx_kernel::ERR_VAULT_FUNGIBLE_ASSET_AMOUNT_LESS_THAN_AMOUNT_TO_WITHDRAW
                .message()
        );
    }

    struct UnreachableProver;

    #[async_trait::async_trait]
    impl miden_client::transaction::TransactionProver for UnreachableProver {
        async fn prove(
            &self,
            _inputs: miden_protocol::transaction::TransactionInputs,
        ) -> Result<miden_protocol::transaction::ProvenTransaction, miden_tx::TransactionProverError>
        {
            Err(miden_tx::TransactionProverError::Other {
                error_msg: "failed to prove transaction".into(),
                source: Some(Box::new(std::io::Error::other(
                    "connection error: i/o timeout",
                ))),
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_prover_is_retried_until_the_transaction_expiration_is_reached() {
        let proposal = proposal_expiring_after(true, 80).await;
        let executor = MidenExecutor::new(proposal.rpc.clone(), Arc::new(UnreachableProver));
        let mut attempt = executor.prepare(proposal.input(&[0, 1])).await.unwrap();
        attempt.execute(None).await.unwrap();

        let chain = proposal.rpc.clone();
        let producer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                chain.advance_blocks(1);
            }
        });
        let failure = attempt.prove().await.unwrap_err();
        producer.abort();

        assert_eq!(
            failure.code,
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Transaction),
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("i/o timeout"),
            "{}",
            failure.message
        );
    }

    /// Fails with a transport error the first `failures` times, then proves locally.
    struct RecoveringProver {
        failures: std::sync::atomic::AtomicUsize,
        local: LocalTransactionProver,
    }

    #[async_trait::async_trait]
    impl miden_client::transaction::TransactionProver for RecoveringProver {
        async fn prove(
            &self,
            inputs: miden_protocol::transaction::TransactionInputs,
        ) -> Result<miden_protocol::transaction::ProvenTransaction, miden_tx::TransactionProverError>
        {
            let remaining = self.failures.load(std::sync::atomic::Ordering::SeqCst);
            if remaining > 0 {
                self.failures
                    .store(remaining - 1, std::sync::atomic::Ordering::SeqCst);
                return UnreachableProver.prove(inputs).await;
            }
            miden_client::transaction::TransactionProver::prove(&self.local, inputs).await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_prover_that_recovers_completes_the_same_attempt() {
        let proposal = proposal(true).await;
        let prover = Arc::new(RecoveringProver {
            failures: std::sync::atomic::AtomicUsize::new(2),
            local: LocalTransactionProver::default(),
        });
        let executor = MidenExecutor::new(proposal.rpc.clone(), prover.clone());
        let mut attempt = executor.prepare(proposal.input(&[0, 1])).await.unwrap();
        attempt.execute(None).await.unwrap();
        let proven = attempt.prove().await.unwrap();
        assert_eq!(
            prover.failures.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "both transient failures were retried"
        );
        assert!(proven.expiration_block > proven.reference_block);
    }

    /// Proves whatever it is given, and keeps a copy of the inputs.
    struct CapturingProver {
        captured: std::sync::Mutex<Option<miden_protocol::transaction::TransactionInputs>>,
        local: LocalTransactionProver,
    }

    #[async_trait::async_trait]
    impl miden_client::transaction::TransactionProver for CapturingProver {
        async fn prove(
            &self,
            inputs: miden_protocol::transaction::TransactionInputs,
        ) -> Result<miden_protocol::transaction::ProvenTransaction, miden_tx::TransactionProverError>
        {
            *self.captured.lock().unwrap() = Some(inputs.clone());
            miden_client::transaction::TransactionProver::prove(&self.local, inputs).await
        }
    }

    /// Answers every request with a valid proof of another transaction.
    struct ForeignProver {
        inputs: miden_protocol::transaction::TransactionInputs,
        local: LocalTransactionProver,
    }

    #[async_trait::async_trait]
    impl miden_client::transaction::TransactionProver for ForeignProver {
        async fn prove(
            &self,
            _inputs: miden_protocol::transaction::TransactionInputs,
        ) -> Result<miden_protocol::transaction::ProvenTransaction, miden_tx::TransactionProverError>
        {
            miden_client::transaction::TransactionProver::prove(&self.local, self.inputs.clone())
                .await
        }
    }

    #[tokio::test]
    async fn a_proof_of_another_transaction_is_refused_before_the_boundary() {
        let other = proposal_expiring_after(true, 80).await;
        let capturing = Arc::new(CapturingProver {
            captured: std::sync::Mutex::new(None),
            local: LocalTransactionProver::default(),
        });
        let mut elsewhere = MidenExecutor::new(other.rpc.clone(), capturing.clone())
            .prepare(other.input(&[0, 1]))
            .await
            .unwrap();
        elsewhere.execute(None).await.unwrap();
        elsewhere.prove().await.unwrap();
        let foreign = capturing.captured.lock().unwrap().take().unwrap();

        let proposal = proposal(true).await;
        let executor = MidenExecutor::new(
            proposal.rpc.clone(),
            Arc::new(ForeignProver {
                inputs: foreign,
                local: LocalTransactionProver::default(),
            }),
        );
        let mut attempt = executor.prepare(proposal.input(&[0, 1])).await.unwrap();
        attempt.execute(None).await.unwrap();
        let failure = attempt.prove().await.unwrap_err();
        assert_eq!(
            failure.code,
            ExecutionFailureCode::ProvingFailed,
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("returned transaction"),
            "{}",
            failure.message
        );
    }

    /// A 2-of-2 ECDSA account's rotation, approved by one raw and one EIP-712 cosigner signature,
    /// as a hardware wallet signs it.
    #[tokio::test]
    async fn an_eip712_cosigner_signature_counts_and_authorizes_the_execution() {
        use guardian_shared::EcdsaMessageFormat;
        use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
        use miden_standards::account::auth::Eip712TransactionSummary;

        let cosigners: Vec<SigningKey> = (0..2).map(|_| SigningKey::new()).collect();
        let config = MultisigGuardianConfig::new(
            2,
            cosigners
                .iter()
                .map(|key| key.public_key().to_commitment())
                .collect(),
            SigningKey::new().public_key().to_commitment(),
        )
        .with_account_type(AccountType::Public)
        .with_signature_scheme(SignatureScheme::Ecdsa);
        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .unwrap();
        let new_guardian = SigningKey::new().public_key().to_commitment();
        let scheme_id = SignatureScheme::Ecdsa.auth_scheme_id();
        let script = CodeBuilder::new()
            .with_dynamically_linked_package(AuthGuardedMultisig::code())
            .unwrap()
            .compile_tx_script(format!(
                "@transaction_script\npub proc main\n    push.{new_guardian}\n    push.{scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
            ))
            .unwrap();
        let bound = BlockNumber::from(BOUND);
        let auth_args = MultisigAuthArgs::new(bound, Word::from([Felt::new_unchecked(9); 4]))
            .with_approval_expiration_delta(NonZeroU32::new(1_000).unwrap())
            .unwrap();
        let commitment = auth_args.to_commitment();
        let mut chain = MockChainBuilder::with_accounts([account.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain.prove_until_block(bound).unwrap();
        let summary = match chain
            .build_transaction(account.id())
            .authenticator(None)
            .tx_script(script.clone())
            .add_advice_map_entry(commitment, auth_args.to_elements())
            .auth_args(commitment)
            .build()
            .unwrap()
            .execute()
            .await
            .unwrap_err()
        {
            TransactionExecutorError::Unauthorized(summary) => *summary,
            error => panic!("expected the unsigned abort: {error:?}"),
        };
        let request_bytes = TransactionRequestBuilder::new()
            .custom_script(script)
            .auth_arg(commitment)
            .extend_advice_map([(commitment, auth_args.to_elements())])
            .block_numbers([bound])
            .build()
            .unwrap()
            .to_bytes();
        let rpc = Arc::new(MockRpcApi::new(chain));
        rpc.advance_blocks(60);

        let signature = |key: &SigningKey, format: EcdsaMessageFormat| {
            let signed = match format {
                EcdsaMessageFormat::Raw => key.sign(summary.to_commitment()),
                EcdsaMessageFormat::Eip712 => key.sign_prehash(summary.eip712_hash().into_bytes()),
            };
            CosignerSignature {
                signer_id: format!(
                    "0x{}",
                    hex::encode(key.public_key().to_commitment().to_bytes())
                ),
                signature: ProposalSignature::Ecdsa {
                    signature: format!("0x{}", hex::encode(signed.to_bytes())),
                    public_key: Some(format!("0x{}", hex::encode(key.public_key().to_bytes()))),
                    message_format: format,
                },
                timestamp: "2026-09-30T12:00:00Z".to_string(),
            }
        };
        let input = ExecutionInput {
            account_id: account.id().to_hex(),
            state_json: account.to_json(),
            proposal_payload: serde_json::json!({
                "tx_summary": summary.to_json(),
                "metadata": { "proposal_type": "switch_guardian" },
                "transaction_request": TransactionRequestEnvelope::seal(
                    &request_bytes),
            }),
            cosigner_signatures: vec![
                signature(&cosigners[0], EcdsaMessageFormat::Raw),
                signature(&cosigners[1], EcdsaMessageFormat::Eip712),
            ],
        };
        let executor = MidenExecutor::new(rpc, Arc::new(LocalTransactionProver::default()));

        let selection = executor.select_signatures(&input).unwrap();
        assert_eq!((selection.valid, selection.ignored), (2, 0));
        assert!(selection.is_ready(), "the EIP-712 signature counts");
        let mut attempt = executor.prepare(input).await.unwrap();
        attempt
            .execute(None)
            .await
            .expect("the auth procedure accepts the EIP-712 advice");
    }

    #[tokio::test]
    async fn an_eip712_signature_over_another_summary_is_ignored() {
        let proposal = proposal(true).await;
        let mut input = proposal.input(&[0]);
        let impostor = miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey::new();
        input.cosigner_signatures.push(CosignerSignature {
            signer_id: input.cosigner_signatures[0].signer_id.clone(),
            signature: ProposalSignature::Ecdsa {
                signature: format!(
                    "0x{}",
                    hex::encode(impostor.sign_prehash([7; 32]).to_bytes())
                ),
                public_key: Some(format!(
                    "0x{}",
                    hex::encode(impostor.public_key().to_bytes())
                )),
                message_format: guardian_shared::EcdsaMessageFormat::Eip712,
            },
            timestamp: "2026-09-30T12:00:00Z".to_string(),
        });
        let selection = proposal.executor().select_signatures(&input).unwrap();
        assert_eq!((selection.valid, selection.ignored), (1, 1));
    }

    #[tokio::test]
    async fn a_node_that_serves_no_encryption_key_fails_sealing_before_any_send() {
        let proposal = proposal(true).await;
        let executor = proposal.executor();
        let mut attempt = executor.prepare(proposal.input(&[0, 1])).await.unwrap();
        attempt.execute(None).await.unwrap();
        attempt.prove().await.unwrap();
        let failure = attempt.seal().await.unwrap_err();
        assert_eq!(
            failure.code,
            ExecutionFailureCode::SealingFailed,
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("encryption key"),
            "{}",
            failure.message
        );
    }

    /// An unpayable fee aborts in `pay_fee`, which runs before the unauthorized abort, so an
    /// unfunded account cannot even be proposed for. The abort is the one Guardian reads as a fee
    /// shortfall when a fee grows between proposal and execution.
    #[tokio::test]
    async fn an_unpayable_fee_aborts_as_the_shortfall_guardian_reports_as_insufficient_fee() {
        let cosigners: Vec<SecretKey> = (0..2).map(|_| SecretKey::new()).collect();
        let config = MultisigGuardianConfig::new(
            2,
            cosigners
                .iter()
                .map(|key| key.public_key().to_commitment())
                .collect(),
            SecretKey::new().public_key().to_commitment(),
        )
        .with_account_type(AccountType::Public)
        .with_signature_scheme(SignatureScheme::Falcon);
        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .unwrap();
        let fee_faucet = miden_protocol::testing::account_id::ACCOUNT_ID_FEE_FAUCET
            .try_into()
            .unwrap();
        let mut chain = MockChainBuilder::with_accounts([account.clone()])
            .unwrap()
            .verification_base_fee(50)
            .build()
            .unwrap();
        chain.prove_until_block(BlockNumber::from(BOUND)).unwrap();
        let auth_args = MultisigAuthArgs::new(
            BlockNumber::from(BOUND),
            Word::from([Felt::new_unchecked(9); 4]),
        )
        .with_conversion_info(
            miden_standards::account::auth::FeeConversionInfo::one_to_one(fee_faucet),
        );
        let commitment = auth_args.to_commitment();
        let error = chain
            .build_transaction(account.id())
            .authenticator(None)
            .add_advice_map_entry(commitment, auth_args.to_elements())
            .auth_args(commitment)
            .build()
            .unwrap()
            .execute()
            .await
            .unwrap_err();
        assert_eq!(Abort::of(&error), Some(Abort::VaultShortfall), "{error}");
    }

    #[tokio::test]
    async fn a_tampered_request_is_refused_on_its_envelope() {
        let proposal = proposal(true).await;
        let mut input = proposal.input(&[0, 1]);
        input.proposal_payload["transaction_request"]["checksum"] =
            serde_json::json!(format!("0x{}", "00".repeat(32)));
        let Err(failure) = proposal.executor().prepare(input).await else {
            panic!("a tampered request must be refused");
        };
        assert_eq!(failure.code, ExecutionFailureCode::RequestCodec);
    }
}

mod threshold {
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::Word;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_standards::account::wallets::BasicWallet;

    use crate::network::miden::execution::threshold::{InvokedProcedure, effective_threshold};

    fn auth_root(name: &str) -> Word {
        use miden_standards::account::auth::AuthGuardedMultisig;
        let code = AuthGuardedMultisig::code();
        let export = code
            .exports()
            .find(|export| export.path.to_string().rsplit("::").next() == Some(name))
            .unwrap();
        code.get_procedure_root_by_path(&*export.path)
            .unwrap()
            .into()
    }

    fn account(overrides: Vec<(Word, u32)>) -> miden_protocol::account::Account {
        let signers = (0..3)
            .map(|_| SecretKey::new().public_key().to_commitment())
            .collect();
        let config =
            MultisigGuardianConfig::new(2, signers, SecretKey::new().public_key().to_commitment())
                .with_proc_threshold_overrides(overrides);
        MultisigGuardianBuilder::new(config)
            .build_existing()
            .unwrap()
    }

    #[test]
    fn a_proposal_type_is_held_to_its_procedure_override() {
        let account = account(vec![
            (BasicWallet::move_asset_to_note_root().into(), 3),
            (BasicWallet::receive_asset_root().into(), 1),
            (auth_root("set_procedure_threshold"), 3),
        ]);
        let of = |proposal_type| {
            effective_threshold(&account, InvokedProcedure::of_proposal_type(proposal_type))
        };
        assert_eq!(of(Some("p2id")), Some(3));
        assert_eq!(of(Some("consume_notes")), Some(1));
        assert_eq!(
            of(Some("add_signer")),
            Some(2),
            "no override: the account default"
        );
    }

    #[test]
    fn an_unmodelled_proposal_type_is_held_to_the_account_default() {
        let account = account(vec![(BasicWallet::receive_asset_root().into(), 1)]);
        for proposal_type in [Some("b2agg"), Some("custom"), None] {
            assert_eq!(
                effective_threshold(&account, InvokedProcedure::of_proposal_type(proposal_type)),
                Some(2)
            );
        }
    }

    #[test]
    fn every_modelled_auth_procedure_resolves_to_an_override() {
        let account = account(vec![
            (auth_root("update_signers_and_threshold"), 3),
            (auth_root("set_procedure_threshold"), 3),
            (auth_root("update_guardian_public_key"), 1),
        ]);
        let of = |proposal_type| {
            effective_threshold(
                &account,
                InvokedProcedure::of_proposal_type(Some(proposal_type)),
            )
        };
        for proposal_type in [
            "add_signer",
            "remove_signer",
            "change_threshold",
            "update_procedure_threshold",
        ] {
            assert_eq!(of(proposal_type), Some(3), "{proposal_type}");
        }
        assert_eq!(of("switch_guardian"), Some(1));
    }
}
