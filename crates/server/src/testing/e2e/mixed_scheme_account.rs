//! Accounts whose cosigners mix Falcon and ECDSA keys (issue #563), checked
//! against real account states built from the upstream guarded-multisig
//! component and decoded by the real Miden network client:
//!
//! 1. `/configure` accepts the per-signer `Auth::Miden` declaration of a
//!    mixed account, refuses single-scheme or mis-scoped declarations and a
//!    GUARDIAN key stored under the wrong scheme, and acknowledges with the
//!    GUARDIAN's scheme. Each cosigner then authenticates under its own
//!    scheme, and only under it.
//! 2. A real `update_signers_and_threshold` transaction that adds a Falcon
//!    signer to an ECDSA account, proposed with the ECDSA signer's approval
//!    and pushed for the server's ack, executed on a mock chain and
//!    canonicalized, promotes the account's single-scheme auth to
//!    `Auth::Miden`. The new Falcon signer then authenticates, and its
//!    approval alone gets the next transaction acknowledged.

use std::sync::Arc;

use guardian_shared::auth_request_eip712::request_digest;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use guardian_shared::{EcdsaMessageFormat, ProposalSignature, SignatureScheme, ToJson};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::auth::{AuthSecretKey, Signature as AccountSignature};
use miden_protocol::account::{Account, AccountType};
use miden_protocol::assembly::Package;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, Signature as EcdsaSignature, SigningKey as EcdsaSecretKey,
};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::vm::AdviceInputs;
use miden_protocol::{Felt, Hasher, Word};
use miden_standards::StandardsLib;
use miden_standards::account::auth::MultisigAuthArgs;
use miden_standards::code_builder::CodeBuilder;
use miden_testing::MockChainBuilder;
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};

use super::MultisigAuthArgsExt;
use crate::delta_object::DeltaObject;
use crate::error::GuardianError;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials, MidenCosigner, RequestAuthFormat};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::services::{
    ConfigureAccountParams, ConfigureAccountResult, GetStateParams, PushDeltaParams,
    PushDeltaProposalParams, PushDeltaResult, configure_account, get_state,
    process_canonicalizations_now, push_delta, push_delta_proposal,
};
use crate::state::AppState;
use crate::testing::helpers::{IntegrationMockNetworkClient, create_test_app_state};

fn commitment_hex(word: Word) -> String {
    format!("0x{}", hex::encode(word.to_bytes()))
}

fn account_commitment_hex(account: &Account) -> String {
    commitment_hex(account.to_commitment())
}

fn word_from_hex(hex_str: &str) -> Word {
    let bytes = hex::decode(hex_str.trim_start_matches("0x")).expect("valid hex");
    Word::read_from_bytes(&bytes).expect("valid word bytes")
}

fn request_word(account_id_hex: &str, timestamp: i64) -> Word {
    AuthRequestMessage::from_account_id_hex(account_id_hex, timestamp, AuthRequestPayload::empty())
        .expect("valid account ID")
        .to_word()
}

/// Strictly increasing request timestamps, so successive requests of one
/// signer never trip replay protection.
fn next_timestamp() -> i64 {
    static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let now = chrono::Utc::now().timestamp_millis();
    let previous = LAST.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
    if now > previous {
        now
    } else {
        LAST.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }
}

fn falcon_credentials(key: &FalconSecretKey, account_id_hex: &str) -> Credentials {
    let timestamp = next_timestamp();
    let signature = key.sign(request_word(account_id_hex, timestamp));
    Credentials::signature(
        key.public_key().into_hex(),
        format!("0x{}", hex::encode(signature.to_bytes())),
        timestamp,
    )
}

fn ecdsa_credentials(key: &EcdsaSecretKey, account_id_hex: &str) -> Credentials {
    let timestamp = next_timestamp();
    let signature = key.sign(request_word(account_id_hex, timestamp));
    Credentials::signature(
        format!("0x{}", hex::encode(key.public_key().to_bytes())),
        format!("0x{}", hex::encode(signature.to_bytes())),
        timestamp,
    )
}

fn eip712_credentials(key: &EcdsaSecretKey, account_id_hex: &str) -> Credentials {
    let timestamp = next_timestamp();
    let signature = key.sign_prehash(request_digest(request_word(account_id_hex, timestamp)));
    Credentials::signature(
        format!("0x{}", hex::encode(key.public_key().to_bytes())),
        format!("0x{}", hex::encode(signature.to_bytes())),
        timestamp,
    )
    .with_auth_format(RequestAuthFormat::Eip712)
}

fn falcon_approval(key: &FalconSecretKey, message: Word) -> ProposalSignature {
    ProposalSignature::Falcon {
        signature: format!("0x{}", hex::encode(key.sign(message).to_bytes())),
    }
}

fn ecdsa_approval(key: &EcdsaSecretKey, message: Word) -> ProposalSignature {
    ProposalSignature::Ecdsa {
        signature: format!("0x{}", hex::encode(key.sign(message).to_bytes())),
        public_key: Some(format!("0x{}", hex::encode(key.public_key().to_bytes()))),
        message_format: EcdsaMessageFormat::Raw,
    }
}

/// Creates the proposal for `summary` with the proposer's own approval
/// attached, as the SDKs do before pushing for the acknowledgement.
async fn propose(
    state: &AppState,
    account_id_hex: &str,
    nonce: u64,
    summary: &miden_protocol::transaction::TransactionSummary,
    proposer: Word,
    approval: ProposalSignature,
    credentials: Credentials,
) -> Result<(), GuardianError> {
    push_delta_proposal(
        state,
        PushDeltaProposalParams {
            account_id: account_id_hex.to_string(),
            nonce,
            delta_payload: serde_json::json!({
                "tx_summary": summary.to_json(),
                "signatures": [{
                    "signer_id": commitment_hex(proposer),
                    "signature": approval,
                }],
                "metadata": { "proposal_type": "custom" },
            }),
            credentials,
        },
    )
    .await
    .map(|_| ())
}

/// Pushes `summary` on `base` for this server's acknowledgement.
async fn push(
    state: &AppState,
    base: &Account,
    summary: &miden_protocol::transaction::TransactionSummary,
    credentials: Credentials,
) -> Result<PushDeltaResult, GuardianError> {
    push_delta(
        state,
        PushDeltaParams {
            delta: DeltaObject {
                account_id: base.id().to_hex(),
                nonce: base.nonce().as_canonical_u64() + 1,
                prev_commitment: account_commitment_hex(base),
                new_commitment: None,
                delta_payload: summary.to_json(),
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: Default::default(),
                metadata: None,
            },
            credentials,
        },
    )
    .await
}

fn cosigner(commitment: Word, scheme: SignatureScheme) -> MidenCosigner {
    MidenCosigner {
        commitment: commitment_hex(commitment),
        scheme,
    }
}

/// A public guarded multisig whose signers and GUARDIAN carry the given
/// schemes, deployed (`build_existing`) so a mock chain accepts it.
fn guarded_account(
    threshold: u32,
    signers: &[(Word, SignatureScheme)],
    guardian_commitment: Word,
    guardian_scheme: SignatureScheme,
) -> Account {
    let config = MultisigGuardianConfig::new(
        threshold,
        signers.iter().map(|(commitment, _)| *commitment).collect(),
        guardian_commitment,
    )
    .with_signer_schemes(signers.iter().map(|(_, scheme)| *scheme).collect())
    .with_guardian_scheme(guardian_scheme)
    .with_account_type(AccountType::Public);
    MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("guarded multisig account builds")
}

/// Uses the real Miden network client, whose credential, GUARDIAN-binding
/// and signer-set checks decode the submitted state; none of them reaches
/// the node.
fn with_real_network_client(state: &mut AppState) {
    state.network_client = Arc::new(MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal));
}

/// The real Miden network client over a chain that holds `account`'s state:
/// an account past nonce 0 registers only from its on-chain state.
fn with_chain_holding(state: &mut AppState, account: &Account) {
    let mut chain = IntegrationMockNetworkClient::new(MidenNetworkClient::lazy_for_test(
        NetworkType::MidenLocal,
    ));
    chain.register_account(account.id().to_hex(), account_commitment_hex(account));
    state.network_client = Arc::new(chain);
}

async fn configure(
    state: &AppState,
    account: &Account,
    auth: Auth,
    credential: Credentials,
) -> Result<ConfigureAccountResult, GuardianError> {
    configure_account(
        state,
        ConfigureAccountParams {
            account_id: account.id().to_hex(),
            auth,
            network_config: NetworkConfig::miden_default(),
            initial_state: account.to_json(),
            credential,
        },
    )
    .await
}

async fn authenticated_signer(
    state: &AppState,
    account_id_hex: &str,
    credentials: Credentials,
) -> Result<String, GuardianError> {
    get_state(
        state,
        GetStateParams {
            account_id: account_id_hex.to_string(),
            credentials,
        },
    )
    .await
    .map(|result| result.state.auth_scheme)
}

#[tokio::test]
async fn mixed_scheme_account_registers_and_each_cosigner_authenticates_under_its_scheme() {
    let mut state = create_test_app_state().await;
    with_real_network_client(&mut state);

    let falcon = FalconSecretKey::new();
    let ecdsa = EcdsaSecretKey::new();
    let falcon_commitment = falcon.public_key().to_commitment();
    let ecdsa_commitment = ecdsa.public_key().to_commitment();
    let guardian = word_from_hex(&state.ack.commitment(&SignatureScheme::Ecdsa));
    let account = guarded_account(
        2,
        &[
            (falcon_commitment, SignatureScheme::Falcon),
            (ecdsa_commitment, SignatureScheme::Ecdsa),
        ],
        guardian,
        SignatureScheme::Ecdsa,
    );
    let account_id_hex = account.id().to_hex();
    let declared = Auth::Miden {
        cosigners: vec![
            cosigner(falcon_commitment, SignatureScheme::Falcon),
            cosigner(ecdsa_commitment, SignatureScheme::Ecdsa),
        ],
        guardian_scheme: SignatureScheme::Ecdsa,
    };

    // Single-scheme declarations of the mixed state are refused. A Falcon
    // declaration names this server's Falcon ACK key, which the GUARDIAN
    // slot does not hold; an ECDSA one names the right key but not the
    // Falcon cosigner's scheme.
    let both = vec![
        commitment_hex(falcon_commitment),
        commitment_hex(ecdsa_commitment),
    ];
    let falcon_declaration = Auth::MidenFalconRpo {
        cosigner_commitments: both.clone(),
    };
    match configure(
        &state,
        &account,
        falcon_declaration,
        falcon_credentials(&falcon, &account_id_hex),
    )
    .await
    {
        Err(GuardianError::AuthorizationFailed(message)) => {
            assert!(message.contains("guardian::pub_key"), "{message}")
        }
        other => panic!("expected AuthorizationFailed, got {other:?}"),
    }
    let ecdsa_declaration = Auth::MidenEcdsa {
        cosigner_commitments: both,
    };
    match configure(
        &state,
        &account,
        ecdsa_declaration,
        ecdsa_credentials(&ecdsa, &account_id_hex),
    )
    .await
    {
        Err(GuardianError::InvalidInput(message)) => {
            assert!(
                message.contains("cosigner schemes do not match"),
                "{message}"
            );
            assert!(
                message.contains(&format!("{}:falcon", commitment_hex(falcon_commitment))),
                "{message}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // A cosigner declared under the scheme it does not use is refused.
    let swapped = Auth::Miden {
        cosigners: vec![
            cosigner(falcon_commitment, SignatureScheme::Falcon),
            cosigner(ecdsa_commitment, SignatureScheme::Falcon),
        ],
        guardian_scheme: SignatureScheme::Ecdsa,
    };
    match configure(
        &state,
        &account,
        swapped,
        falcon_credentials(&falcon, &account_id_hex),
    )
    .await
    {
        Err(GuardianError::InvalidInput(message)) => {
            assert!(
                message.contains("cosigner schemes do not match"),
                "{message}"
            )
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // The GUARDIAN declared under the other scheme names the other ACK key.
    let wrong_guardian = Auth::Miden {
        cosigners: vec![
            cosigner(falcon_commitment, SignatureScheme::Falcon),
            cosigner(ecdsa_commitment, SignatureScheme::Ecdsa),
        ],
        guardian_scheme: SignatureScheme::Falcon,
    };
    match configure(
        &state,
        &account,
        wrong_guardian,
        falcon_credentials(&falcon, &account_id_hex),
    )
    .await
    {
        Err(GuardianError::AuthorizationFailed(message)) => {
            assert!(message.contains("guardian::pub_key"), "{message}")
        }
        other => panic!("expected AuthorizationFailed, got {other:?}"),
    }

    // The per-signer declaration registers, signed by the Falcon cosigner,
    // and is acknowledged with the GUARDIAN's ECDSA key.
    with_chain_holding(&mut state, &account);
    let configured = configure(
        &state,
        &account,
        declared.clone(),
        falcon_credentials(&falcon, &account_id_hex),
    )
    .await
    .expect("the mixed account registers");
    assert_eq!(
        configured.ack_commitment,
        state.ack.commitment(&SignatureScheme::Ecdsa)
    );
    assert_eq!(
        state
            .metadata
            .get(&account_id_hex)
            .await
            .unwrap()
            .expect("metadata stored")
            .auth,
        declared
    );

    // Each cosigner authenticates under its own scheme.
    for credentials in [
        falcon_credentials(&falcon, &account_id_hex),
        ecdsa_credentials(&ecdsa, &account_id_hex),
        eip712_credentials(&ecdsa, &account_id_hex),
    ] {
        assert_eq!(
            authenticated_signer(&state, &account_id_hex, credentials)
                .await
                .expect("a registered cosigner authenticates"),
            "mixed"
        );
    }

    // Keys that are not cosigners are refused, whatever their scheme.
    for credentials in [
        falcon_credentials(&FalconSecretKey::new(), &account_id_hex),
        ecdsa_credentials(&EcdsaSecretKey::new(), &account_id_hex),
    ] {
        assert!(matches!(
            authenticated_signer(&state, &account_id_hex, credentials).await,
            Err(GuardianError::AuthenticationFailed(_))
        ));
    }

    // Both cosigners find the account by their key commitment.
    for commitment in [falcon_commitment, ecdsa_commitment] {
        assert_eq!(
            state
                .metadata
                .find_by_cosigner_commitment(&commitment_hex(commitment))
                .await
                .unwrap(),
            vec![account_id_hex.clone()]
        );
    }
}

/// The brick scenario from the #540 review: this server's ECDSA ACK key in
/// the GUARDIAN slot, stored under the Falcon scheme, would have every
/// acknowledgement verified as Falcon on chain. It is refused at
/// `/configure`, for the single-scheme declaration as for the per-signer one.
#[tokio::test]
async fn a_guardian_key_stored_under_another_scheme_is_refused() {
    let mut state = create_test_app_state().await;
    with_real_network_client(&mut state);

    let ecdsa = EcdsaSecretKey::new();
    let ecdsa_commitment = ecdsa.public_key().to_commitment();
    let account = guarded_account(
        1,
        &[(ecdsa_commitment, SignatureScheme::Ecdsa)],
        word_from_hex(&state.ack.commitment(&SignatureScheme::Ecdsa)),
        SignatureScheme::Falcon,
    );
    let account_id_hex = account.id().to_hex();

    let single_scheme = Auth::MidenEcdsa {
        cosigner_commitments: vec![commitment_hex(ecdsa_commitment)],
    };
    match configure(
        &state,
        &account,
        single_scheme,
        ecdsa_credentials(&ecdsa, &account_id_hex),
    )
    .await
    {
        Err(GuardianError::AuthorizationFailed(message)) => {
            assert!(message.contains("guardian::scheme"), "{message}");
            assert!(message.contains("expected ecdsa, got falcon"), "{message}");
        }
        other => panic!("expected AuthorizationFailed, got {other:?}"),
    }
    assert!(state.metadata.get(&account_id_hex).await.unwrap().is_none());
}

#[tokio::test]
async fn adding_a_falcon_signer_on_chain_promotes_an_ecdsa_account() {
    let mut state = create_test_app_state().await;
    with_real_network_client(&mut state);

    let ecdsa = EcdsaSecretKey::new();
    let ecdsa_commitment = ecdsa.public_key().to_commitment();
    let falcon = FalconSecretKey::new();
    let falcon_commitment = falcon.public_key().to_commitment();
    let guardian_public_key_hex = state.ack.pubkey(&SignatureScheme::Ecdsa);
    let account = guarded_account(
        1,
        &[(ecdsa_commitment, SignatureScheme::Ecdsa)],
        word_from_hex(&state.ack.commitment(&SignatureScheme::Ecdsa)),
        SignatureScheme::Ecdsa,
    );
    let account_id_hex = account.id().to_hex();
    with_chain_holding(&mut state, &account);

    configure(
        &state,
        &account,
        Auth::MidenEcdsa {
            cosigner_commitments: vec![commitment_hex(ecdsa_commitment)],
        },
        ecdsa_credentials(&ecdsa, &account_id_hex),
    )
    .await
    .expect("the ECDSA account registers");

    // `update_signers_and_threshold` advice: [CONFIG, PUB_KEY_N, SCHEME_N,
    // ..., PUB_KEY_0, SCHEME_0], the existing ECDSA signer at index 0 and
    // the new Falcon signer at index 1, threshold 1.
    let new_signers = [
        (ecdsa_commitment, SignatureScheme::Ecdsa),
        (falcon_commitment, SignatureScheme::Falcon),
    ];
    let mut config = vec![
        Felt::new_unchecked(1),
        Felt::new_unchecked(new_signers.len() as u64),
        Felt::new_unchecked(0),
        Felt::new_unchecked(0),
    ];
    for (commitment, scheme) in new_signers.iter().rev() {
        config.extend_from_slice(commitment.as_elements());
        config.extend_from_slice(&[
            Felt::new_unchecked(scheme.auth_scheme_id()),
            Felt::new_unchecked(0),
            Felt::new_unchecked(0),
            Felt::new_unchecked(0),
        ]);
    }
    let config_hash = Hasher::hash_elements(&config);
    let advice_inputs = AdviceInputs::default().with_map([(config_hash, config)]);
    let standards: Package = StandardsLib::default().into();
    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_package(&standards)
        .expect("library links")
        .compile_tx_script(
            "use miden::standards::auth::multisig\n@transaction_script\npub proc main\n    call.multisig::update_signers_and_threshold\nend",
        )
        .expect("tx script compiles");

    let mock_chain = MockChainBuilder::with_accounts([account.clone()])
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");
    let auth_args = MultisigAuthArgs::new(
        mock_chain.latest_block_header().block_num(),
        Word::from([Felt::new_unchecked(5); 4]),
    );

    let summary = match mock_chain
        .build_transaction(account.id())
        .authenticator(None)
        .tx_script(tx_script.clone())
        .tx_script_args(config_hash)
        .extend_advice_inputs(advice_inputs.clone())
        .multisig_auth_args(&auth_args)
        .build()
        .expect("tx builds")
        .execute()
        .await
        .unwrap_err()
    {
        TransactionExecutorError::Unauthorized(summary) => summary,
        error => panic!("expected abort with tx effects: {error:?}"),
    };
    let message = summary.as_ref().to_commitment();

    // The ECDSA signer proposes the summary with its approval and pushes it
    // for this server's acknowledgement, as the SDKs do, over the
    // IntegrationMock chain for the canonicalizer. The push gate counts the
    // approval under the ECDSA scheme the state stores for its signer.
    let chain = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));
    state.network_client = chain.clone();
    propose(
        &state,
        &account_id_hex,
        account.nonce().as_canonical_u64() + 1,
        &summary,
        ecdsa_commitment,
        ecdsa_approval(&ecdsa, message),
        ecdsa_credentials(&ecdsa, &account_id_hex),
    )
    .await
    .expect("the ECDSA signer proposes the signer change");
    let pushed = push(
        &state,
        &account,
        &summary,
        ecdsa_credentials(&ecdsa, &account_id_hex),
    )
    .await
    .expect("push_delta acknowledges the approved signer change");
    assert_eq!(pushed.delta.ack_scheme, "ecdsa");

    let signer_signature =
        BasicAuthenticator::new(&[AuthSecretKey::EcdsaK256Keccak(ecdsa.clone())])
            .get_signature(
                ecdsa.public_key().to_commitment().into(),
                &SigningInputs::TransactionSummary(summary),
            )
            .await
            .expect("the ECDSA signer signs");
    let guardian_public_key = EcdsaPublicKey::read_from_bytes(
        &hex::decode(guardian_public_key_hex.trim_start_matches("0x")).unwrap(),
    )
    .expect("ACK public key");
    let guardian_signature = AccountSignature::EcdsaK256Keccak(
        EcdsaSignature::read_from_bytes(&hex::decode(&pushed.delta.ack_sig).unwrap())
            .expect("ACK signature"),
    );
    let executed = mock_chain
        .build_transaction(account.id())
        .authenticator(None)
        .tx_script(tx_script)
        .tx_script_args(config_hash)
        .extend_advice_inputs(advice_inputs)
        .add_signature(
            ecdsa.public_key().to_commitment().into(),
            message,
            signer_signature,
        )
        .add_signature(
            guardian_public_key.to_commitment().into(),
            message,
            guardian_signature,
        )
        .multisig_auth_args(&auth_args)
        .build()
        .expect("tx builds")
        .execute()
        .await
        .expect("the signed signer change executes");
    let mut executed_account = account.clone();
    executed_account
        .apply_patch(executed.account_patch())
        .expect("executed patch applies");
    chain.set_on_chain_commitment(&account_id_hex, &account_commitment_hex(&executed_account));

    // Until the change is canonical the Falcon key is not a cosigner.
    assert!(matches!(
        authenticated_signer(
            &state,
            &account_id_hex,
            falcon_credentials(&falcon, &account_id_hex)
        )
        .await,
        Err(GuardianError::AuthenticationFailed(_))
    ));

    let pass = process_canonicalizations_now(&state)
        .await
        .expect("canonicalization pass runs");
    assert_eq!(pass.failed_accounts, 0);
    let stored = state.storage.pull_state(&account_id_hex).await.unwrap();
    assert_eq!(stored.commitment, account_commitment_hex(&executed_account));

    // The promoted auth keeps the GUARDIAN's ECDSA scheme and registers the
    // new signer under the Falcon scheme the chain stores for it.
    assert_eq!(
        state
            .metadata
            .get(&account_id_hex)
            .await
            .unwrap()
            .expect("metadata stored")
            .auth,
        Auth::Miden {
            cosigners: vec![
                cosigner(ecdsa_commitment, SignatureScheme::Ecdsa),
                cosigner(falcon_commitment, SignatureScheme::Falcon),
            ],
            guardian_scheme: SignatureScheme::Ecdsa,
        }
    );
    assert_eq!(
        authenticated_signer(
            &state,
            &account_id_hex,
            falcon_credentials(&falcon, &account_id_hex)
        )
        .await
        .expect("the new Falcon signer authenticates"),
        "mixed"
    );
    assert!(
        authenticated_signer(
            &state,
            &account_id_hex,
            ecdsa_credentials(&ecdsa, &account_id_hex)
        )
        .await
        .is_ok(),
        "the ECDSA signer keeps authenticating"
    );

    // A transaction on the now mixed account, approved by the new Falcon
    // signer alone (threshold 1): the push gate counts the approval under
    // the Falcon scheme the state stores for it, and the GUARDIAN's ECDSA
    // key acknowledges.
    let mixed_chain = MockChainBuilder::with_accounts([executed_account.clone()])
        .expect("mock chain accepts the mixed account")
        .build()
        .expect("mock chain builds");
    let mixed_auth_args = MultisigAuthArgs::new(
        mixed_chain.latest_block_header().block_num(),
        Word::from([Felt::new_unchecked(6); 4]),
    );
    let second = match mixed_chain
        .build_transaction(executed_account.id())
        .authenticator(None)
        .multisig_auth_args(&mixed_auth_args)
        .build()
        .expect("tx builds")
        .execute()
        .await
        .unwrap_err()
    {
        TransactionExecutorError::Unauthorized(summary) => summary,
        error => panic!("expected abort with tx effects: {error:?}"),
    };
    let second_message = second.as_ref().to_commitment();
    assert!(matches!(
        push(
            &state,
            &executed_account,
            &second,
            falcon_credentials(&falcon, &account_id_hex),
        )
        .await,
        Err(GuardianError::InsufficientSignatures {
            required: 1,
            got: 0
        })
    ));
    propose(
        &state,
        &account_id_hex,
        executed_account.nonce().as_canonical_u64() + 1,
        &second,
        falcon_commitment,
        falcon_approval(&falcon, second_message),
        falcon_credentials(&falcon, &account_id_hex),
    )
    .await
    .expect("the Falcon signer proposes on the mixed account");
    let acknowledged = push(
        &state,
        &executed_account,
        &second,
        falcon_credentials(&falcon, &account_id_hex),
    )
    .await
    .expect("the gate counts the Falcon approval on the mixed account");
    assert_eq!(acknowledged.delta.ack_scheme, "ecdsa");
    assert_eq!(acknowledged.delta.ack_pubkey, guardian_public_key_hex);
}
