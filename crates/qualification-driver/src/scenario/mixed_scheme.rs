//! A mixed-scheme guarded multisig (issue #563): one Falcon and one ECDSA
//! approver, registered with the per-signer `Miden` auth and driven over gRPC
//! against the image under test.
//!
//! Neither multisig SDK can build such an account yet (the Rust SDK waits on
//! #549, the TypeScript one on the follow-up to #540), so it is built here
//! straight from the upstream `AuthGuardedMultisig` component, which carries
//! one scheme per approver. Its GUARDIAN is the server's ECDSA acknowledgement
//! identity, read at run time because the stack generates that key per run.
//!
//! Both passes of a deterministic run act on the same registered account, so
//! its keys and seed are fixed. Checks that need an account GUARDIAN has never
//! seen (the refused declarations, the scheme gate) build a fresh one each time.
//! The refused declarations store nothing; the gate's account, once the gated
//! GUARDIAN has refused it, is registered on the main one to show nothing but
//! the gate stood in its way, and is never read again.

use std::sync::Arc;

use guardian_client::{
    AuthConfig, EcdsaKeyStore, FalconKeyStore, GuardianClient, MidenAuth, MidenCosigner,
    MidenEcdsaAuth, MidenFalconRpoAuth, Signer, ToJson, auth_config::AuthType,
};
use guardian_shared::{EcdsaMessageFormat, ProposalSignature, SignatureScheme};
use miden_multisig_client::ProposalPayload;
use miden_protocol::account::auth::PublicKeyCommitment;
use miden_protocol::account::delta::AccountVaultDelta;
use miden_protocol::account::{
    Account, AccountBuilder, AccountCodePatch, AccountComponent, AccountDelta, AccountId,
    AccountStoragePatch,
};
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{Felt, Word, ZERO};
use miden_standards::account::auth::{
    Approver, ApproverSet, AuthGuardedMultisig, AuthGuardedMultisigConfig, GuardianConfig,
};
use miden_standards::account::wallets::BasicWallet;
use rand::SeedableRng;
use rand::rngs::StdRng;

use super::account::{operator_login, same_commitment};
use super::{ActionOutcome, Runner, StackServer};

/// Seeds the registered account's keys and its account seed. Fixed, so the
/// post-restart pass and an upgrade's target phase build the account the first
/// pass registered rather than a new one.
const REGISTERED_SEED: [u8; 32] = [0x63; 32];

/// The nonce the per-scheme approvals proposal is labelled with: the account's
/// first transaction.
const PROPOSAL_NONCE: u64 = 1;

/// One Falcon and one ECDSA approver, threshold 2, GUARDIAN under ECDSA unless
/// built otherwise.
struct MixedAccount {
    account: Account,
    id: AccountId,
    falcon: FalconSecretKey,
    ecdsa: EcdsaSecretKey,
    falcon_commitment: String,
    ecdsa_commitment: String,
}

impl MixedAccount {
    /// Builds the account from `seed`, its GUARDIAN slot holding
    /// `guardian_commitment` under `guardian_scheme`. The same seed always yields
    /// the same keys and, for the same GUARDIAN, the same account.
    fn build(seed: [u8; 32], guardian_commitment: Word, guardian_scheme: SignatureScheme) -> Self {
        let mut rng = StdRng::from_seed(seed);
        let falcon = FalconSecretKey::with_rng(&mut rng);
        let ecdsa = EcdsaSecretKey::with_rng(&mut rng);
        let falcon_word = falcon.public_key().to_commitment();
        let ecdsa_word = ecdsa.public_key().to_commitment();
        let account = guarded_multisig(
            seed,
            2,
            &[
                (falcon_word, SignatureScheme::Falcon),
                (ecdsa_word, SignatureScheme::Ecdsa),
            ],
            (guardian_commitment, guardian_scheme),
        );
        Self {
            id: account.id(),
            account,
            falcon,
            ecdsa,
            falcon_commitment: commitment_hex(falcon_word),
            ecdsa_commitment: commitment_hex(ecdsa_word),
        }
    }

    /// The account the scenario registers and keeps across passes.
    fn registered(guardian_commitment: Word) -> Self {
        Self::build(REGISTERED_SEED, guardian_commitment, SignatureScheme::Ecdsa)
    }

    /// A never-registered account, for checks that must store nothing.
    fn throwaway(guardian_commitment: Word) -> Self {
        Self::build(rand::random(), guardian_commitment, SignatureScheme::Ecdsa)
    }

    /// The per-signer declaration, cosigners in approver index order.
    fn per_signer_auth(&self) -> AuthConfig {
        miden_auth(
            &[
                (&self.falcon_commitment, "falcon"),
                (&self.ecdsa_commitment, "ecdsa"),
            ],
            "ecdsa",
        )
    }

    fn falcon_signer(&self) -> Arc<dyn Signer> {
        Arc::new(FalconKeyStore::new(self.falcon.clone()))
    }

    fn ecdsa_signer(&self) -> Arc<dyn Signer> {
        Arc::new(EcdsaKeyStore::new(self.ecdsa.clone()))
    }
}

/// A guarded multisig built straight from the upstream component (plus a
/// `BasicWallet`), whose approvers and GUARDIAN each carry their own scheme,
/// as `miden-standards` stores them. The same construction as the server's
/// own mixed-scheme integration tests.
fn guarded_multisig(
    seed: [u8; 32],
    threshold: u32,
    approvers: &[(Word, SignatureScheme)],
    guardian: (Word, SignatureScheme),
) -> Account {
    let approver = |(commitment, scheme): (Word, SignatureScheme)| {
        Approver::new(PublicKeyCommitment::from(commitment), scheme.auth_scheme())
    };
    let approver_set =
        ApproverSet::new(approvers.iter().copied().map(approver).collect(), threshold)
            .expect("two distinct approvers under a threshold of two form a valid set");
    let config =
        AuthGuardedMultisigConfig::new(approver_set, GuardianConfig::new(approver(guardian)))
            .expect("a guardian distinct from the approvers forms a valid config");
    let component: AccountComponent = AuthGuardedMultisig::new(config)
        .expect("the guarded multisig component builds from a valid config")
        .into();
    AccountBuilder::new(seed)
        .with_component(component)
        .with_component(BasicWallet)
        .build()
        .expect("a guarded multisig with a basic wallet builds")
}

fn miden_auth(cosigners: &[(&str, &str)], guardian_scheme: &str) -> AuthConfig {
    AuthConfig {
        auth_type: Some(AuthType::Miden(MidenAuth {
            cosigners: cosigners
                .iter()
                .map(|(commitment, scheme)| MidenCosigner {
                    commitment: commitment.to_string(),
                    scheme: scheme.to_string(),
                })
                .collect(),
            guardian_scheme: guardian_scheme.to_string(),
        })),
    }
}

fn commitment_hex(word: Word) -> String {
    format!("0x{}", hex::encode(word.to_bytes()))
}

async fn connect(endpoint: &str, signer: Arc<dyn Signer>) -> Result<GuardianClient, ActionOutcome> {
    match GuardianClient::connect(endpoint.to_string()).await {
        Ok(client) => Ok(client.with_signer(signer)),
        Err(error) => Err(ActionOutcome::failed_setup(format!(
            "cannot reach {endpoint}: {error}"
        ))),
    }
}

/// The server's ECDSA acknowledgement commitment, which the account's
/// GUARDIAN slot must hold for registration to succeed.
async fn guardian_ecdsa_commitment(endpoint: &str) -> Result<Word, ActionOutcome> {
    let mut client = GuardianClient::connect(endpoint.to_string())
        .await
        .map_err(|error| {
            ActionOutcome::failed_setup(format!("cannot reach {endpoint}: {error}"))
        })?;
    let (commitment, _) = client.get_pubkey(Some("ecdsa")).await.map_err(|error| {
        ActionOutcome::failed_product(format!(
            "GUARDIAN did not serve its ECDSA acknowledgement identity: {error}"
        ))
    })?;
    Word::try_from(commitment.as_str()).map_err(|error| {
        ActionOutcome::failed_product(format!(
            "GUARDIAN served a malformed ECDSA acknowledgement commitment `{commitment}`: {error}"
        ))
    })
}

/// Whether the account is already registered: readable by its Falcon cosigner.
/// `account_not_found` is the one answer that means no.
async fn is_registered(endpoint: &str, mixed: &MixedAccount) -> Result<bool, ActionOutcome> {
    let mut client = connect(endpoint, mixed.falcon_signer()).await?;
    match client.get_state(&mixed.id).await {
        Ok(_) => Ok(true),
        Err(error) if error.guardian_code().as_deref() == Some("account_not_found") => Ok(false),
        Err(error) => Err(ActionOutcome::failed_product(format!(
            "reading the mixed-scheme account to see whether it is registered failed: {error}"
        ))),
    }
}

/// Expects `configure` to be refused with `code`, naming `what` was declared.
async fn expect_configure_refused(
    endpoint: &str,
    mixed: &MixedAccount,
    signer: Arc<dyn Signer>,
    auth: AuthConfig,
    code: &str,
    what: &str,
) -> Result<(), ActionOutcome> {
    let mut client = connect(endpoint, signer).await?;
    match client
        .configure(&mixed.id, auth, mixed.account.to_json())
        .await
    {
        Ok(_) => Err(ActionOutcome::failed_product(format!(
            "GUARDIAN registered the mixed-scheme account under {what}"
        ))),
        Err(error) => match error.guardian_code() {
            Some(actual) if actual == code => Ok(()),
            Some(actual) => Err(ActionOutcome::failed_product(format!(
                "{what} was refused with `{actual}` rather than `{code}`: {error}"
            ))),
            None => Err(ActionOutcome::failed_product(format!(
                "{what} failed without a GUARDIAN error code: {error}"
            ))),
        },
    }
}

/// Declarations that misstate the account's schemes are refused, and none of
/// them stores the account.
///
/// Each one names something the account's state contradicts. The Falcon
/// declaration names this server's Falcon acknowledgement key, which the
/// GUARDIAN slot does not hold; the ECDSA one names the right key but not the
/// Falcon approver's scheme; the swapped one declares the ECDSA approver as
/// Falcon; declaring the GUARDIAN under Falcon names the other acknowledgement
/// key again. None of those reaches the GUARDIAN scheme slot, because a key
/// mismatch is found first, so a separate account holds this server's ECDSA key
/// in its GUARDIAN slot under the Falcon scheme: the right key, the wrong
/// scheme, which only the slot check refuses.
pub async fn assert_declarations_refused(runner: &Runner) -> ActionOutcome {
    let endpoint = runner.endpoints.grpc.clone();
    let guardian = match guardian_ecdsa_commitment(&endpoint).await {
        Ok(guardian) => guardian,
        Err(outcome) => return outcome,
    };
    let mixed = MixedAccount::throwaway(guardian);
    let both = vec![
        mixed.falcon_commitment.clone(),
        mixed.ecdsa_commitment.clone(),
    ];

    let cases = [
        (
            mixed.falcon_signer(),
            AuthConfig {
                auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                    cosigner_commitments: both.clone(),
                })),
            },
            "authorization_failed",
            "a single-scheme Falcon declaration",
        ),
        (
            mixed.ecdsa_signer(),
            AuthConfig {
                auth_type: Some(AuthType::MidenEcdsa(MidenEcdsaAuth {
                    cosigner_commitments: both,
                })),
            },
            "invalid_input",
            "a single-scheme ECDSA declaration",
        ),
        (
            mixed.falcon_signer(),
            miden_auth(
                &[
                    (&mixed.falcon_commitment, "falcon"),
                    (&mixed.ecdsa_commitment, "falcon"),
                ],
                "ecdsa",
            ),
            "invalid_input",
            "a per-signer declaration with the ECDSA approver declared as Falcon",
        ),
        (
            mixed.falcon_signer(),
            miden_auth(
                &[
                    (&mixed.falcon_commitment, "falcon"),
                    (&mixed.ecdsa_commitment, "ecdsa"),
                ],
                "falcon",
            ),
            "authorization_failed",
            "a per-signer declaration with the GUARDIAN declared as Falcon",
        ),
    ];
    for (signer, auth, code, what) in cases {
        if let Err(outcome) =
            expect_configure_refused(&endpoint, &mixed, signer, auth, code, what).await
        {
            return outcome;
        }
    }

    let slot_mismatch = MixedAccount::build(rand::random(), guardian, SignatureScheme::Falcon);
    if let Err(outcome) = expect_configure_refused(
        &endpoint,
        &slot_mismatch,
        slot_mismatch.falcon_signer(),
        slot_mismatch.per_signer_auth(),
        "authorization_failed",
        "a per-signer declaration of an account whose GUARDIAN slot stores the ECDSA \
         acknowledgement key under the Falcon scheme",
    )
    .await
    {
        return outcome;
    }

    for refused in [&mixed, &slot_mismatch] {
        match is_registered(&endpoint, refused).await {
            Ok(false) => {}
            Ok(true) => {
                return ActionOutcome::failed_product(
                    "every declaration was refused, yet GUARDIAN serves the account".to_string(),
                );
            }
            Err(outcome) => return outcome,
        }
    }
    ActionOutcome::Passed
}

/// Whether this pass must find what the first pass stored rather than store
/// it: after a plain restart. Not on the first pass, and not on an upgrade's
/// target phase, where the pass before the restart ran the release being
/// upgraded from, whose seed phase does not run this scenario.
fn expects_stored(runner: &Runner) -> bool {
    runner.post_restart && !runner.upgrade_target
}

/// Registers the account with the per-signer declaration, signed by its
/// Falcon cosigner, and checks GUARDIAN acknowledges it with its ECDSA key.
///
/// After a plain restart it registers nothing and requires the account the
/// first pass stored: registering it again would recreate exactly what a lost
/// row is missing, and every later assertion would pass against the copy.
pub async fn register(runner: &Runner) -> ActionOutcome {
    let endpoint = runner.endpoints.grpc.clone();
    let guardian = match guardian_ecdsa_commitment(&endpoint).await {
        Ok(guardian) => guardian,
        Err(outcome) => return outcome,
    };
    let mixed = MixedAccount::registered(guardian);
    match is_registered(&endpoint, &mixed).await {
        Ok(true) => return ActionOutcome::Passed,
        Ok(false) if expects_stored(runner) => {
            return ActionOutcome::failed_product(
                "the mixed-scheme account registered before the restart is gone: GUARDIAN \
                 answers `account_not_found` to its Falcon cosigner"
                    .to_string(),
            );
        }
        Ok(false) => {}
        Err(outcome) => return outcome,
    }

    let mut client = match connect(&endpoint, mixed.falcon_signer()).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };
    match client
        .configure(&mixed.id, mixed.per_signer_auth(), mixed.account.to_json())
        .await
    {
        Ok(response) if same_commitment(&response.ack_commitment, &commitment_hex(guardian)) => {
            ActionOutcome::Passed
        }
        Ok(response) => ActionOutcome::failed_product(format!(
            "the mixed-scheme account registered, but GUARDIAN acknowledged it with `{}` rather \
             than its ECDSA identity `{}`, which the account's GUARDIAN slot holds",
            response.ack_commitment,
            commitment_hex(guardian)
        )),
        Err(error) => ActionOutcome::failed_product(match error.guardian_code() {
            Some(code) => format!(
                "the per-signer declaration of the mixed-scheme account was refused with \
                 `{code}`: {error}"
            ),
            None => {
                format!("the per-signer declaration of the mixed-scheme account failed: {error}")
            }
        }),
    }
}

/// The registered account, built and confirmed present, for the actions that
/// read it.
async fn registered_account(endpoint: &str) -> Result<MixedAccount, ActionOutcome> {
    let mixed = MixedAccount::registered(guardian_ecdsa_commitment(endpoint).await?);
    if is_registered(endpoint, &mixed).await? {
        Ok(mixed)
    } else {
        Err(ActionOutcome::failed_product(
            "the mixed-scheme account is not registered; `mixed-scheme-register` must run first"
                .to_string(),
        ))
    }
}

/// Each cosigner authenticates under its own scheme and reads the state it
/// registered; keys that are not cosigners, under either scheme, are refused.
pub async fn assert_cosigners_authenticate(runner: &Runner) -> ActionOutcome {
    let endpoint = runner.endpoints.grpc.clone();
    let mixed = match registered_account(&endpoint).await {
        Ok(mixed) => mixed,
        Err(outcome) => return outcome,
    };
    let expected = commitment_hex(mixed.account.to_commitment());

    for (scheme, signer) in [
        ("Falcon", mixed.falcon_signer()),
        ("ECDSA", mixed.ecdsa_signer()),
    ] {
        let mut client = match connect(&endpoint, signer).await {
            Ok(client) => client,
            Err(outcome) => return outcome,
        };
        match client.get_state(&mixed.id).await {
            Ok(response) => match response.state {
                Some(state) if same_commitment(&state.commitment, &expected) => {}
                Some(state) => {
                    return ActionOutcome::failed_product(format!(
                        "the {scheme} cosigner read commitment {}, not the registered {expected}",
                        state.commitment
                    ));
                }
                None => {
                    return ActionOutcome::failed_product(format!(
                        "GUARDIAN authenticated the {scheme} cosigner but returned no state"
                    ));
                }
            },
            Err(error) => {
                return ActionOutcome::failed_product(format!(
                    "the {scheme} cosigner was not authenticated under its own scheme: {error}"
                ));
            }
        }
    }

    let strangers: [(&str, Arc<dyn Signer>); 2] = [
        ("Falcon", Arc::new(FalconKeyStore::generate())),
        ("ECDSA", Arc::new(EcdsaKeyStore::generate())),
    ];
    for (scheme, signer) in strangers {
        let mut client = match connect(&endpoint, signer).await {
            Ok(client) => client,
            Err(outcome) => return outcome,
        };
        match client.get_state(&mixed.id).await {
            Ok(_) => {
                return ActionOutcome::failed_product(format!(
                    "GUARDIAN served the mixed-scheme account to a {scheme} key that is not one \
                     of its cosigners"
                ));
            }
            Err(error) if error.guardian_code().as_deref() == Some("authentication_failed") => {}
            Err(error) => {
                return ActionOutcome::failed_product(format!(
                    "a {scheme} key that is not a cosigner was refused, but not as \
                     `authentication_failed`: {error}"
                ));
            }
        }
    }
    ActionOutcome::Passed
}

/// The operator dashboard reports the account's auth scheme as `mixed`.
pub async fn assert_reported_mixed(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let mixed = match registered_account(&runner.endpoints.grpc).await {
        Ok(mixed) => mixed,
        Err(outcome) => return outcome,
    };
    let base = runner.endpoints.http.trim_end_matches('/').to_string();
    if let Err(outcome) = operator_login(runner, &base, fixtures).await {
        return outcome;
    }

    let response = match runner
        .http
        .get(format!("{base}/dashboard/accounts/{}", mixed.id))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "reading the account from the operator dashboard failed: {error}"
            ));
        }
    };
    if !response.status().is_success() {
        return ActionOutcome::failed_product(format!(
            "the operator dashboard refused the mixed-scheme account's detail with {}",
            response.status()
        ));
    }
    let detail: serde_json::Value = match response.json().await {
        Ok(detail) => detail,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "the operator dashboard returned an unreadable account detail: {error}"
            ));
        }
    };
    match detail["auth_scheme"].as_str() {
        Some("mixed") => ActionOutcome::Passed,
        Some(other) => ActionOutcome::failed_product(format!(
            "the operator dashboard reports the mixed-scheme account's auth scheme as `{other}`"
        )),
        None => ActionOutcome::failed_product(format!(
            "the operator dashboard's account detail has no `auth_scheme`: {detail}"
        )),
    }
}

/// A transaction summary that only bumps the nonce. GUARDIAN stores a proposal
/// against the account's state without executing it, so this is enough to
/// carry approvals; nothing here reaches a chain.
fn nonce_only_summary(account_id: AccountId) -> Result<TransactionSummary, ActionOutcome> {
    let delta = AccountDelta::new(
        account_id,
        AccountStoragePatch::default(),
        AccountVaultDelta::default(),
        AccountCodePatch::default(),
        Felt::new_unchecked(PROPOSAL_NONCE),
    )
    .map_err(|error| ActionOutcome::failed_setup(format!("building the account delta: {error}")))?;
    let input_notes = InputNotes::new(Vec::new()).map_err(|error| {
        ActionOutcome::failed_setup(format!("building the input notes: {error}"))
    })?;
    let output_notes = RawOutputNotes::new(Vec::new()).map_err(|error| {
        ActionOutcome::failed_setup(format!("building the output notes: {error}"))
    })?;
    Ok(TransactionSummary::new(
        delta,
        input_notes,
        output_notes,
        BlockNumber::from(0),
        Word::from([ZERO; 4]),
        0,
        TransactionSummaryUserParams::new([ZERO; TransactionSummaryUserParams::NUM_ELEMENTS]),
    ))
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

/// The approvals the proposal at `PROPOSAL_NONCE` records, as `(signer, scheme)`
/// pairs sorted for comparison, or `None` when no such proposal is pending.
async fn recorded_approvals(
    client: &mut GuardianClient,
    id: &AccountId,
) -> Result<Option<Vec<(String, String)>>, ActionOutcome> {
    let proposals = client.get_delta_proposals(id).await.map_err(|error| {
        ActionOutcome::failed_product(format!(
            "listing the mixed-scheme account's proposals failed: {error}"
        ))
    })?;
    let Some(proposal) = proposals
        .proposals
        .iter()
        .find(|proposal| proposal.nonce == PROPOSAL_NONCE)
    else {
        return Ok(None);
    };
    match proposal
        .status
        .as_ref()
        .and_then(|status| status.status.as_ref())
    {
        Some(guardian_client::delta_status::Status::Pending(pending)) => {
            let mut recorded: Vec<(String, String)> = pending
                .cosigner_sigs
                .iter()
                .map(|sig| {
                    (
                        sig.signer_id.to_lowercase(),
                        sig.signature
                            .as_ref()
                            .map(|signature| signature.scheme.to_lowercase())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            recorded.sort();
            Ok(Some(recorded))
        }
        other => Err(ActionOutcome::failed_product(format!(
            "the proposal at nonce {PROPOSAL_NONCE} is not pending: {other:?}"
        ))),
    }
}

/// One proposal collects an approval from each cosigner, each under its own
/// scheme, and refuses one from a key that is not a cosigner.
///
/// The proposal and its approvals are written once, on the pass that registers
/// the account; writing them is idempotent there. After a plain restart nothing
/// is written before both approvals are read back: pushing and signing again
/// would recreate whatever the restart lost.
pub async fn assert_per_scheme_approvals(runner: &Runner) -> ActionOutcome {
    let endpoint = runner.endpoints.grpc.clone();
    let mixed = match registered_account(&endpoint).await {
        Ok(mixed) => mixed,
        Err(outcome) => return outcome,
    };
    let summary = match nonce_only_summary(mixed.id) {
        Ok(summary) => summary,
        Err(outcome) => return outcome,
    };
    let message = summary.to_commitment();
    let proposal_id = commitment_hex(message);
    let mut expected = vec![
        (mixed.falcon_commitment.to_lowercase(), "falcon".to_string()),
        (mixed.ecdsa_commitment.to_lowercase(), "ecdsa".to_string()),
    ];
    expected.sort();

    let mut falcon_client = match connect(&endpoint, mixed.falcon_signer()).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };
    let mut ecdsa_client = match connect(&endpoint, mixed.ecdsa_signer()).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };

    let before = match recorded_approvals(&mut falcon_client, &mixed.id).await {
        Ok(before) => before,
        Err(outcome) => return outcome,
    };
    if expects_stored(runner) {
        match before {
            Some(recorded) if recorded == expected => {}
            Some(recorded) => {
                return ActionOutcome::failed_product(format!(
                    "after the restart the proposal records approvals {recorded:?}, not the \
                     per-scheme pair {expected:?} stored before it"
                ));
            }
            None => {
                return ActionOutcome::failed_product(format!(
                    "the proposal at nonce {PROPOSAL_NONCE}, approved by both cosigners before \
                     the restart, is gone"
                ));
            }
        }
    } else {
        if before.is_none() {
            let payload =
                ProposalPayload::new(&summary).with_custom_metadata("qualification".to_string());
            if let Err(error) = falcon_client
                .push_delta_proposal(&mixed.id, PROPOSAL_NONCE, &payload)
                .await
            {
                return ActionOutcome::failed_product(format!(
                    "the Falcon cosigner could not create a proposal on the mixed-scheme \
                     account: {error}"
                ));
            }
        }
        let approvals = [
            (
                "Falcon",
                &mut falcon_client,
                falcon_approval(&mixed.falcon, message),
            ),
            (
                "ECDSA",
                &mut ecdsa_client,
                ecdsa_approval(&mixed.ecdsa, message),
            ),
        ];
        for (scheme, client, approval) in approvals {
            match client
                .sign_delta_proposal(&mixed.id, proposal_id.clone(), approval)
                .await
            {
                Ok(_) => {}
                Err(error)
                    if error.guardian_code().as_deref() == Some("proposal_already_signed") => {}
                Err(error) => {
                    return ActionOutcome::failed_product(format!(
                        "the {scheme} cosigner's approval under its own scheme was refused: \
                         {error}"
                    ));
                }
            }
        }
    }

    // Relayed by a cosigner, so the request authenticates and the approval's
    // own signer is what GUARDIAN judges.
    let stranger = FalconSecretKey::new();
    match falcon_client
        .sign_delta_proposal(
            &mixed.id,
            proposal_id.clone(),
            falcon_approval(&stranger, message),
        )
        .await
    {
        Ok(_) => {
            return ActionOutcome::failed_product(
                "GUARDIAN accepted an approval from a key that is not a cosigner".to_string(),
            );
        }
        Err(error) if error.guardian_code().as_deref() == Some("invalid_proposal_signature") => {}
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "an approval from a key that is not a cosigner was refused, but not as \
                 `invalid_proposal_signature`: {error}"
            ));
        }
    }

    match recorded_approvals(&mut ecdsa_client, &mixed.id).await {
        Ok(Some(recorded)) if recorded == expected => ActionOutcome::Passed,
        Ok(Some(recorded)) => ActionOutcome::failed_product(format!(
            "the proposal records approvals {recorded:?}, not one per cosigner under its own \
             scheme {expected:?}"
        )),
        Ok(None) => ActionOutcome::failed_product(format!(
            "the approved proposal at nonce {PROPOSAL_NONCE} is not listed"
        )),
        Err(outcome) => outcome,
    }
}

/// An ECDSA-only GUARDIAN refuses the mixed-scheme account for its Falcon
/// approver, even when the registration is signed by its ECDSA one, and the
/// same account registers on the main GUARDIAN, so the gate is the only reason
/// it was refused.
pub async fn assert_mixed_scheme_gate(runner: &Runner) -> ActionOutcome {
    let endpoint = match StackServer::SchemeGated.grpc_endpoint() {
        Ok(endpoint) => endpoint,
        Err(outcome) => return outcome,
    };
    // The gated server's own ECDSA identity, so the account binds the right
    // key and only the gate has a reason to refuse it.
    let guardian = match guardian_ecdsa_commitment(&endpoint).await {
        Ok(guardian) => guardian,
        Err(outcome) => return outcome,
    };
    let mixed = MixedAccount::throwaway(guardian);
    let mut client = match connect(&endpoint, mixed.ecdsa_signer()).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };
    match client
        .configure(&mixed.id, mixed.per_signer_auth(), mixed.account.to_json())
        .await
    {
        Ok(_) => {
            return ActionOutcome::failed_product(
                "the ECDSA-only GUARDIAN registered an account with a Falcon approver".to_string(),
            );
        }
        Err(error) => match error.guardian_code() {
            Some(code) if code == "signature_scheme_not_allowed" => {
                match error
                    .guardian_meta()
                    .and_then(|meta| meta["scheme"].as_str().map(str::to_string))
                {
                    Some(scheme) if scheme == "falcon" => {}
                    other => {
                        return ActionOutcome::failed_product(format!(
                            "the gate refused the mixed-scheme account but named scheme \
                             {other:?} rather than its Falcon approver: {error}"
                        ));
                    }
                }
            }
            Some(code) => {
                return ActionOutcome::failed_product(format!(
                    "the mixed-scheme registration was refused with `{code}` rather than \
                     `signature_scheme_not_allowed`, so the gate is not what turned it away: \
                     {error}"
                ));
            }
            None => {
                return ActionOutcome::failed_product(format!(
                    "the mixed-scheme registration failed without a GUARDIAN error code: {error}"
                ));
            }
        },
    }

    // The stack's servers share one acknowledgement identity; without that the
    // ungated half below would fail for a reason that is not the gate.
    let main = runner.endpoints.grpc.clone();
    match guardian_ecdsa_commitment(&main).await {
        Ok(main_guardian) if main_guardian == guardian => {}
        Ok(_) => {
            return ActionOutcome::failed_setup(
                "the main and scheme-gated GUARDIANs hold different ECDSA acknowledgement \
                 identities, so the refused account cannot be checked against the ungated one"
                    .to_string(),
            );
        }
        Err(outcome) => return outcome,
    }
    let mut main_client = match connect(&main, mixed.ecdsa_signer()).await {
        Ok(client) => client,
        Err(outcome) => return outcome,
    };
    match main_client
        .configure(&mixed.id, mixed.per_signer_auth(), mixed.account.to_json())
        .await
    {
        Ok(_) => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "the account the gate refused also fails to register on the ungated GUARDIAN, so \
             the gate may not be what refused it: {error}"
        )),
    }
}
