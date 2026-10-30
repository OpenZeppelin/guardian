//! Approver sets that mix Falcon and ECDSA keys (issue #549), driven against
//! the in-process mock GUARDIAN gRPC server and a mock chain.
//!
//! The account stores a scheme per approver and one for the GUARDIAN. Every
//! signer update writes the whole approver set back with a scheme for each,
//! and a GUARDIAN switch writes the GUARDIAN's scheme, so the SDK has to carry
//! the stored schemes through creation, registration, proposal building and
//! every cosigner's rebuild. Before #549 it wrote the acting signer's scheme
//! everywhere, which on a mixed account rewrote the other approvers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use guardian_client::auth_config::AuthType;
use guardian_client::testing::mocks::{MockGuardianHandle, MockGuardianService, start_mock_server};
use guardian_client::{ConfigureRequest, GetDeltaProposalResponse, GetDeltaProposalsResponse};
use guardian_shared::{FromJson, SignatureScheme};
use miden_client::testing::mock::MockRpcApi;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::{Account, StorageMapKey};
use miden_protocol::transaction::TransactionSummary;
use miden_standards::account::auth::AuthGuardedMultisig;

use super::MultisigClient;
use super::test_support::{
    chain_with_notes, offline_client_parts_with_keystore, pending_proto_delta, registered_state,
    signer_specs,
};
use crate::account::MultisigAccount;
use crate::error::MultisigError;
use crate::execution::build_final_transaction_request;
use crate::keystore::{EcdsaGuardianKeyStore, GuardianKeyStore, KeyManager};
use crate::payload::ProposalPayload;
use crate::proposal::{Proposal, TransactionType};
use crate::signers::{SignerSchemeChange, SignerSpec};
use crate::transaction::word_to_hex;
use crate::transaction::{
    build_update_signers_transaction_request, execute_for_summary_at_tip, generate_salt,
};

const F: SignatureScheme = SignatureScheme::Falcon;
const E: SignatureScheme = SignatureScheme::Ecdsa;

const GUARDIAN_COMMITMENT: [u32; 4] = [9, 9, 9, 9];

fn falcon_key() -> Arc<dyn KeyManager> {
    Arc::new(GuardianKeyStore::generate())
}

fn ecdsa_key() -> Arc<dyn KeyManager> {
    Arc::new(EcdsaGuardianKeyStore::generate())
}

/// A guarded multisig whose approvers carry the given schemes.
fn account_with(
    approvers: &[SignerSpec],
    threshold: u32,
    guardian_scheme: SignatureScheme,
    seed: u8,
) -> Account {
    let config = MultisigGuardianConfig::new(
        threshold,
        SignerSpec::commitments(approvers),
        Word::from(GUARDIAN_COMMITMENT),
    )
    .with_signer_schemes(approvers.iter().map(|approver| approver.scheme).collect())
    .with_guardian_scheme(guardian_scheme);
    MultisigGuardianBuilder::new(config)
        .with_seed([seed; 32])
        .build()
        .expect("mixed multisig account builds")
}

/// A mock GUARDIAN serving `account`'s registered state.
async fn guardian_for(account: &Account) -> (String, MockGuardianHandle) {
    let (endpoint, handle, _configured) = recording_guardian().await;
    handle.set_persistent_get_state(registered_state(account));
    (endpoint, handle)
}

/// A mock GUARDIAN, with the configure requests it receives.
async fn recording_guardian() -> (
    String,
    MockGuardianHandle,
    Arc<Mutex<Vec<ConfigureRequest>>>,
) {
    let service = MockGuardianService::default();
    let handle = service.handle();
    let configured = service.configure_requests_handle();
    let endpoint = start_mock_server(service).await.unwrap();
    (endpoint, handle, configured)
}

/// A synced client for `keystore` holding `account`, pointed at `endpoint`.
/// Every party of a test shares `chain`, as cosigners share the network: a
/// proposal binds a block of it, which a rebuild has to find.
async fn party(
    dir: &std::path::Path,
    chain: &Arc<MockRpcApi>,
    keystore: Arc<dyn KeyManager>,
    account: &Account,
    endpoint: &str,
) -> MultisigClient {
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, chain.clone(), None, keystore).await;
    client.set_node_rpc_client(chain.clone());
    client.set_guardian_endpoint(endpoint, false).await.unwrap();
    client.add_or_update_account(account, false).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();
    client
}

/// Proposes `tx_type` and returns the payload the proposer pushed. The mock's
/// canned push response names another commitment, so the proposer's own call
/// fails after the push; what reached GUARDIAN is what cosigners verify.
async fn pushed_payload(
    proposer: &mut MultisigClient,
    handle: &MockGuardianHandle,
    tx_type: TransactionType,
) -> String {
    let outcome = proposer.propose_transaction(tx_type).await;
    assert!(
        matches!(&outcome, Err(MultisigError::GuardianServer(message)) if message.contains("mock_commitment")),
        "the proposal is built and pushed, then fails only on the mock's commitment: {outcome:?}"
    );
    handle
        .pushed_proposals()
        .last()
        .expect("the proposal reached GUARDIAN")
        .delta_payload
        .clone()
}

/// Serves `payload` as the account's one pending proposal.
fn serve_proposal(
    handle: &MockGuardianHandle,
    account: &Account,
    payload: &str,
    proposer: &dyn KeyManager,
) {
    let delta = pending_proto_delta(account, 1, payload.to_string(), &proposer.commitment_hex());
    handle.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![delta.clone()],
    });
    handle.set_persistent_get_delta_proposal(GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(delta),
    });
}

/// The single listed proposal, after the client verified it.
async fn listed(client: &mut MultisigClient) -> Proposal {
    let mut proposals = client.list_proposals().await.expect("the listing succeeds");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    proposals.remove(0)
}

fn signer_schemes_of(payload: &str) -> Vec<SignatureScheme> {
    let payload: ProposalPayload = serde_json::from_str(payload).unwrap();
    payload
        .metadata
        .unwrap()
        .signer_schemes
        .iter()
        .map(|scheme| SignatureScheme::from(scheme).unwrap())
        .collect()
}

fn summary_of(payload: &str) -> TransactionSummary {
    let payload: ProposalPayload = serde_json::from_str(payload).unwrap();
    TransactionSummary::from_json(&payload.tx_summary).unwrap()
}

/// The approver schemes the account stores after the transaction, by signer
/// index. The test accounts are new, so a summary's delta carries their whole
/// storage.
fn schemes_after(summary: &TransactionSummary) -> BTreeMap<u32, SignatureScheme> {
    let slot = AuthGuardedMultisig::approver_scheme_ids_slot();
    let Some((_, patch)) = summary
        .account_delta()
        .storage()
        .maps()
        .find(|(name, _)| *name == slot)
    else {
        return BTreeMap::new();
    };
    patch
        .entries()
        .expect("the scheme map is updated, not removed")
        .as_map()
        .iter()
        .map(|(key, value)| {
            let index = (0..16u32)
                .find(|index| *key == StorageMapKey::from_index(*index))
                .expect("an approver index key");
            let scheme = SignatureScheme::from_auth_scheme_id(value[0].as_canonical_u64())
                .expect("a supported scheme id");
            (index, scheme)
        })
        .collect()
}

/// A GUARDIAN registration names each approver's scheme and the GUARDIAN's,
/// read from the account rather than from the registering key.
#[tokio::test]
async fn create_and_register_a_mixed_account_under_the_per_signer_auth() {
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let (endpoint, handle, configured) = recording_guardian().await;
    handle.set_persistent_get_pubkey(word_to_hex(&Word::from(GUARDIAN_COMMITMENT)));

    let dir = tempfile::tempdir().unwrap();
    let api = chain_with_notes(vec![]);
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api, None, falcon.clone()).await;
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let approvers = vec![
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = client
        .create_account_with_signers(2, approvers.clone(), E, Vec::new())
        .await
        .expect("the mixed account is created")
        .clone();
    assert_eq!(
        handle.get_pubkey_schemes(),
        vec![Some("ecdsa".to_string())],
        "the GUARDIAN key is fetched for the GUARDIAN scheme, not the creator's"
    );
    assert_eq!(account.cosigners().unwrap(), approvers);
    assert_eq!(account.guardian_scheme().unwrap(), E);

    client.push_account().await.expect("the account registers");
    let configured = configured.lock().unwrap();
    let Some(AuthType::Miden(auth)) = configured[0]
        .auth
        .as_ref()
        .and_then(|auth| auth.auth_type.clone())
    else {
        panic!("a mixed account registers under the per-signer auth: {configured:?}");
    };
    let registered: Vec<(String, String)> = auth
        .cosigners
        .iter()
        .map(|cosigner| (cosigner.commitment.clone(), cosigner.scheme.clone()))
        .collect();
    assert_eq!(
        registered,
        vec![
            (falcon.commitment_hex(), "falcon".to_string()),
            (wallet.commitment_hex(), "ecdsa".to_string()),
        ]
    );
    assert_eq!(auth.guardian_scheme, "ecdsa");
}

/// Accounts whose approvers and GUARDIAN share a scheme keep the single-scheme
/// auth, so they still register on a GUARDIAN without per-signer schemes.
#[tokio::test]
async fn a_single_scheme_account_keeps_its_single_scheme_auth() {
    let chain = chain_with_notes(vec![]);
    let wallet = ecdsa_key();
    let account = account_with(&[SignerSpec::new(wallet.commitment(), E)], 1, E, 31);
    let (endpoint, handle, configured) = recording_guardian().await;
    handle.set_persistent_get_state(registered_state(&account));
    let dir = tempfile::tempdir().unwrap();
    let mut client = party(dir.path(), &chain, wallet.clone(), &account, &endpoint).await;

    client.push_account().await.expect("the account registers");

    let configured = configured.lock().unwrap();
    assert!(
        matches!(
            configured[0].auth.as_ref().and_then(|auth| auth.auth_type.as_ref()),
            Some(AuthType::MidenEcdsa(auth)) if auth.cosigner_commitments == vec![wallet.commitment_hex()]
        ),
        "{configured:?}"
    );
}

/// An account whose scheme entries cannot be read is not registered under a
/// guessed scheme: nothing reaches GUARDIAN.
#[tokio::test]
async fn an_account_without_scheme_entries_is_not_registered() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let account = account_with(&[SignerSpec::new(falcon.commitment(), F)], 1, F, 33);
    let (id, vault, storage, code, nonce, seed) = account.clone().into_parts();
    let slots = storage
        .into_slots()
        .into_iter()
        .filter(|slot| slot.name() != AuthGuardedMultisig::approver_scheme_ids_slot())
        .collect();
    let storage = miden_protocol::account::AccountStorage::new(slots).unwrap();
    let account = Account::new_unchecked(id, vault, storage, code, nonce, seed);
    let (endpoint, handle, configured) = recording_guardian().await;
    handle.set_persistent_get_state(registered_state(&account));
    let dir = tempfile::tempdir().unwrap();
    let mut client = party(dir.path(), &chain, falcon, &account, &endpoint).await;

    let err = client.push_account().await.unwrap_err();

    assert!(matches!(err, MultisigError::AccountStorage(_)), "{err}");
    assert!(configured.lock().unwrap().is_empty());
}

/// Issue #549: a Falcon proposer adds a signer to an account with an ECDSA
/// approver. The update keeps the ECDSA approver's scheme, names every target
/// scheme in the proposal, and the ECDSA cosigner reproduces it.
#[tokio::test]
async fn adding_a_signer_keeps_every_approver_scheme_and_verifies_for_the_other_scheme() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 2, F, 35);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let added = Word::from([3u32, 3, 3, 3]);
    let payload =
        pushed_payload(&mut proposer, &handle, TransactionType::add_cosigner(added)).await;

    assert_eq!(signer_schemes_of(&payload), vec![F, E, F]);
    assert_eq!(
        schemes_after(&summary_of(&payload)),
        BTreeMap::from([(0, F), (1, E), (2, F)]),
        "the ECDSA approver keeps its scheme and the added one takes the proposer's"
    );

    serve_proposal(&handle, &account, &payload, falcon.as_ref());
    let dir = tempfile::tempdir().unwrap();
    let mut cosigner = party(dir.path(), &chain, wallet, &account, &endpoint).await;
    let proposal = listed(&mut cosigner).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);
}

/// The scheme of an added signer can differ from the proposer's.
#[tokio::test]
async fn an_added_signer_takes_the_scheme_it_is_given() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let account = account_with(&[SignerSpec::new(falcon.commitment(), F)], 1, F, 37);
    let (endpoint, handle) = guardian_for(&account).await;
    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;

    let added = Word::from([4u32, 4, 4, 4]);
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::add_cosigner_with_scheme(added, E),
    )
    .await;

    assert_eq!(signer_schemes_of(&payload), vec![F, E]);
    assert_eq!(
        schemes_after(&summary_of(&payload)),
        BTreeMap::from([(0, F), (1, E)])
    );

    serve_proposal(&handle, &account, &payload, falcon.as_ref());
    let proposal = listed(&mut proposer).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);
}

/// A single-scheme update names no schemes, as before per-signer schemes.
#[tokio::test]
async fn a_single_scheme_update_names_no_schemes() {
    let chain = chain_with_notes(vec![]);
    let wallet = ecdsa_key();
    let account = account_with(&[SignerSpec::new(wallet.commitment(), E)], 1, E, 39);
    let (endpoint, handle) = guardian_for(&account).await;
    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, wallet.clone(), &account, &endpoint).await;

    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::add_cosigner(Word::from([5u32, 5, 5, 5])),
    )
    .await;

    let wire: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(wire["metadata"].get("signer_schemes").is_none(), "{wire}");
    assert_eq!(
        schemes_after(&summary_of(&payload)),
        BTreeMap::from([(0, E), (1, E)])
    );

    serve_proposal(&handle, &account, &payload, wallet.as_ref());
    let proposal = listed(&mut proposer).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);
}

/// Removing the only ECDSA approver leaves Falcon approvers, so the proposal
/// names no schemes; the ECDSA approver being removed still reproduces it,
/// under the scheme the kept approvers store rather than its own.
#[tokio::test]
async fn removing_the_other_scheme_verifies_for_every_approver() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let third = falcon_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
        SignerSpec::new(third.commitment(), F),
    ];
    let account = account_with(&approvers, 2, F, 41);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::remove_cosigner(wallet.commitment()),
    )
    .await;
    assert!(signer_schemes_of(&payload).is_empty());
    assert_eq!(
        schemes_after(&summary_of(&payload)),
        BTreeMap::from([(0, F), (1, F)])
    );

    serve_proposal(&handle, &account, &payload, falcon.as_ref());
    let dir = tempfile::tempdir().unwrap();
    let mut removed = party(dir.path(), &chain, wallet, &account, &endpoint).await;
    let proposal = listed(&mut removed).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);
}

/// The ECDSA approver removes itself: the target is all Falcon, which the
/// proposer's scheme does not describe, so the proposal names it.
#[tokio::test]
async fn an_approver_removing_itself_names_the_remaining_scheme() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 1, F, 43);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, wallet.clone(), &account, &endpoint).await;
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::remove_cosigner(wallet.commitment()),
    )
    .await;
    assert_eq!(signer_schemes_of(&payload), vec![F]);
    assert_eq!(
        schemes_after(&summary_of(&payload)),
        BTreeMap::from([(0, F)])
    );

    serve_proposal(&handle, &account, &payload, wallet.as_ref());
    let dir = tempfile::tempdir().unwrap();
    let mut kept = party(dir.path(), &chain, falcon, &account, &endpoint).await;
    let proposal = listed(&mut kept).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);
}

/// A threshold change rewrites every approver with its stored scheme.
#[tokio::test]
async fn changing_the_threshold_keeps_every_approver_scheme() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 2, F, 45);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, wallet.clone(), &account, &endpoint).await;
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::update_signers(1, SignerSpec::commitments(&approvers)),
    )
    .await;
    assert_eq!(signer_schemes_of(&payload), vec![F, E]);
    assert_eq!(
        schemes_after(&summary_of(&payload)),
        BTreeMap::from([(0, F), (1, E)]),
        "no approver's scheme changes"
    );

    serve_proposal(&handle, &account, &payload, wallet.as_ref());
    let dir = tempfile::tempdir().unwrap();
    let mut cosigner = party(dir.path(), &chain, falcon, &account, &endpoint).await;
    let proposal = listed(&mut cosigner).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);
}

/// A proposal from a client before #549: every approver under the proposer's
/// scheme, no schemes named. On a mixed account it rewrites the ECDSA
/// approver, so the listing marks it failed and signing and executing refuse
/// it before anything reaches GUARDIAN.
#[tokio::test]
async fn a_proposal_stamping_one_scheme_on_a_mixed_account_is_refused() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 1, F, 47);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let added = Word::from([6u32, 6, 6, 6]);
    let targets = [SignerSpec::commitments(&approvers), vec![added]].concat();
    let salt = Word::from([1u32, 2, 3, 4]);
    let auth_args = proposer.multisig_auth_args(salt, None, None).await.unwrap();
    let (request, _) = build_update_signers_transaction_request(
        1,
        &signer_specs(&targets, F),
        &auth_args,
        std::iter::empty(),
        None,
    )
    .unwrap();
    let summary = execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), request)
        .await
        .unwrap();
    assert_eq!(
        schemes_after(&summary),
        BTreeMap::from([(0, F), (1, F), (2, F)]),
        "the stamped proposal does rewrite the ECDSA approver"
    );
    let payload = ProposalPayload::new(&summary)
        .with_add_signer_metadata(
            1,
            targets.iter().map(word_to_hex).collect(),
            word_to_hex(&salt),
        )
        .with_required_signatures(1)
        .to_json()
        .to_string();
    serve_proposal(&handle, &account, &payload, falcon.as_ref());

    let dir = tempfile::tempdir().unwrap();
    let mut cosigner = party(dir.path(), &chain, wallet, &account, &endpoint).await;
    let proposal = listed(&mut cosigner).await;
    assert!(
        proposal.verification.is_failed() && !proposal.verification.is_retryable(),
        "{:?}",
        proposal.verification
    );

    let calls_before = handle.calls().len();
    let err = cosigner.sign_proposal(&proposal.id).await.unwrap_err();
    let MultisigError::SignerSchemeMismatch { changes } = &err else {
        panic!("signing refuses the rewrite: {err}");
    };
    assert_eq!(
        changes,
        &vec![SignerSchemeChange {
            commitment: approvers[1].commitment,
            stored: E,
            proposed: F,
        }]
    );
    let err = proposer.execute_proposal(&proposal.id).await.unwrap_err();
    assert!(
        matches!(err, MultisigError::SignerSchemeMismatch { .. }),
        "executing refuses the rewrite: {err}"
    );
    let reached: Vec<String> = handle.calls()[calls_before..].to_vec();
    assert!(
        !reached.iter().any(|call| call == "push_delta"),
        "no delta reaches GUARDIAN for an acknowledgement: {reached:?}"
    );
}

/// A proposal that names a scheme other than the stored one for a kept
/// approver is refused the same way.
#[tokio::test]
async fn a_proposal_naming_another_scheme_for_a_kept_approver_is_refused() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 2, F, 49);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let salt = Word::from([2u32, 3, 4, 5]);
    let auth_args = proposer.multisig_auth_args(salt, None, None).await.unwrap();
    let rewritten = signer_specs(&SignerSpec::commitments(&approvers), F);
    let (request, _) = build_update_signers_transaction_request(
        1,
        &rewritten,
        &auth_args,
        std::iter::empty(),
        None,
    )
    .unwrap();
    let summary = execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), request)
        .await
        .unwrap();
    let payload = ProposalPayload::new(&summary)
        .with_threshold_metadata(
            1,
            SignerSpec::commitments(&approvers)
                .iter()
                .map(word_to_hex)
                .collect(),
            word_to_hex(&salt),
        )
        .with_signer_schemes(&[F, F])
        .with_required_signatures(2)
        .to_json()
        .to_string();
    serve_proposal(&handle, &account, &payload, falcon.as_ref());

    let proposal = listed(&mut proposer).await;
    assert!(
        proposal
            .verification
            .message()
            .is_some_and(|message| message.contains("stored 'ecdsa', proposed 'falcon'")),
        "{:?}",
        proposal.verification
    );
}

/// A GUARDIAN switch keeps the account's GUARDIAN scheme whatever the
/// proposer's: the new endpoint is asked for its key of that scheme, the
/// switch writes it, and a cosigner of the other scheme reproduces it.
#[tokio::test]
async fn a_switch_keeps_the_stored_guardian_scheme() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 1, E, 51);
    let (endpoint, handle) = guardian_for(&account).await;

    let target = MockGuardianService::default();
    let target_handle = target.handle();
    let target_endpoint = start_mock_server(target).await.unwrap();
    let new_guardian = Word::from([7u32, 7, 7, 7]);
    target_handle.set_persistent_get_pubkey(word_to_hex(&new_guardian));

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::switch_guardian(target_endpoint.clone(), new_guardian),
    )
    .await;
    assert_eq!(
        target_handle.get_pubkey_schemes(),
        vec![Some("ecdsa".to_string())],
        "the Falcon proposer asks the new GUARDIAN for its ECDSA key"
    );
    let guardian_scheme_slot = AuthGuardedMultisig::guardian_scheme_id_slot();
    let guardian_scheme_after: Vec<u64> = summary_of(&payload)
        .account_delta()
        .storage()
        .maps()
        .find(|(name, _)| *name == guardian_scheme_slot)
        .and_then(|(_, patch)| patch.entries().cloned())
        .expect("a new account's delta carries the GUARDIAN scheme map")
        .as_map()
        .values()
        .map(|value| value[0].as_canonical_u64())
        .collect();
    assert_eq!(
        guardian_scheme_after,
        vec![E.auth_scheme_id()],
        "the switch leaves the GUARDIAN scheme ECDSA"
    );

    serve_proposal(&handle, &account, &payload, falcon.as_ref());
    let dir = tempfile::tempdir().unwrap();
    let mut cosigner = party(dir.path(), &chain, wallet, &account, &endpoint).await;
    let proposal = listed(&mut cosigner).await;
    assert!(proposal.is_verified(), "{:?}", proposal.verification);

    let tx_type = TransactionType::switch_guardian(target_endpoint, new_guardian);
    let offline = proposer
        .create_proposal_offline(tx_type.clone())
        .await
        .expect("the offline switch is created");
    assert_eq!(offline.metadata.proposal_type, "switch_guardian");

    // Finalization checks the endpoint again before executing. Driven with
    // the switch request directly: past the check the mock chain refuses the
    // unsigned transaction, which is immaterial here.
    let auth_args = proposer
        .multisig_auth_args(generate_salt(), None, None)
        .await
        .unwrap();
    let request = build_final_transaction_request(
        &proposer.miden_client,
        &tx_type,
        &MultisigAccount::new(account.clone()),
        &auth_args,
        Vec::new(),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let _ = proposer
        .finalize_transaction(account.id(), request, &tx_type)
        .await;

    let queried = target_handle.get_pubkey_schemes();
    assert_eq!(
        queried.len(),
        3,
        "online creation, offline creation and finalization each check the endpoint: {queried:?}"
    );
    assert!(
        queried
            .iter()
            .all(|scheme| scheme.as_deref() == Some("ecdsa")),
        "every check asks for the ECDSA key: {queried:?}"
    );
}

/// A name the SDK does not know fails only the proposal that carries it: the
/// listing still succeeds and reports that proposal as failed.
#[tokio::test]
async fn an_unknown_scheme_name_fails_only_its_proposal() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = account_with(&approvers, 2, F, 55);
    let (endpoint, handle) = guardian_for(&account).await;

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::update_signers(1, SignerSpec::commitments(&approvers)),
    )
    .await;
    let mut wire: serde_json::Value = serde_json::from_str(&payload).unwrap();
    wire["metadata"]["signer_schemes"] = serde_json::json!(["Rsa", "ecdsa"]);
    serve_proposal(&handle, &account, &wire.to_string(), falcon.as_ref());

    let proposal = listed(&mut proposer).await;
    assert!(
        proposal
            .verification
            .message()
            .is_some_and(|message| message.contains("signer_schemes") && message.contains("Rsa")),
        "{:?}",
        proposal.verification
    );
}

/// The creator's own key verifies only under its own scheme, so listing it
/// under the other would lock the creator out of the account it creates.
#[tokio::test]
async fn creating_an_account_with_the_own_key_under_another_scheme_is_refused() {
    let falcon = falcon_key();
    let (endpoint, handle, _configured) = recording_guardian().await;
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) = offline_client_parts_with_keystore(
        dir.path(),
        chain_with_notes(vec![]),
        None,
        falcon.clone(),
    )
    .await;
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let err = client
        .create_account_with_signers(
            1,
            vec![SignerSpec::new(falcon.commitment(), E)],
            F,
            Vec::new(),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(&err, MultisigError::InvalidConfig(message) if message.contains("falcon key")),
        "{err}"
    );
    assert!(
        handle.get_pubkey_schemes().is_empty(),
        "nothing reaches GUARDIAN"
    );
    assert!(client.account().is_none());
}

/// A signer update executes from an exported document on a mixed account.
/// The ECDSA cosigner reproduces and signs the document, the executor relays
/// it, takes GUARDIAN's acknowledgement and lands the update on chain with
/// every approver under its own scheme. Before #549 the document path built
/// no signer update at all: it passed no target signers and failed after
/// GUARDIAN had acknowledged the delta.
#[tokio::test]
async fn a_signer_update_executes_from_a_document_on_a_mixed_account() {
    use guardian_client::{DeltaObject as ProtoDeltaObject, PushDeltaResponse};
    use miden_client::rpc::encryption::TransactionEncryptionKey;
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::SigningKey;

    use crate::export::{ExportedProposal, ExportedSignature};

    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let guardian = GuardianKeyStore::generate();
    let approvers = [
        SignerSpec::new(falcon.commitment(), F),
        SignerSpec::new(wallet.commitment(), E),
    ];
    let account = MultisigGuardianBuilder::new(
        MultisigGuardianConfig::new(
            2,
            SignerSpec::commitments(&approvers),
            guardian.commitment(),
        )
        .with_signer_schemes(vec![F, E])
        .with_guardian_scheme(F),
    )
    .with_seed([57; 32])
    .build()
    .expect("mixed multisig account builds");

    let (endpoint, handle) = guardian_for(&account).await;
    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon.clone(), &account, &endpoint).await;
    let added = Word::from([8u32, 8, 8, 8]);
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::add_cosigner_with_scheme(added, E),
    )
    .await;
    let pending = pending_proto_delta(&account, 1, payload.clone(), &falcon.commitment_hex());
    let proposal = Proposal::from(&pending).unwrap();
    let mut document = ExportedProposal::from_proposal(&proposal, account.id())
        .unwrap()
        .with_signatures(
            proposal
                .signatures
                .iter()
                .map(|signature| ExportedSignature {
                    signer_commitment: signature.signer_commitment.clone(),
                    signature: signature.signature_hex.clone(),
                    scheme: signature.scheme,
                    public_key_hex: signature.public_key_hex.clone(),
                    message_format: signature.message_format,
                })
                .collect(),
        );

    let commitment = proposal.tx_summary.to_commitment();
    let executing = MockGuardianService::default().with_push_delta(Ok(PushDeltaResponse {
        success: true,
        message: String::new(),
        delta: Some(ProtoDeltaObject {
            ack_scheme: Some("falcon".to_string()),
            ..pending.clone()
        }),
        ack_sig: Some(guardian.sign_word_hex(commitment)),
    }));
    let executing_handle = executing.handle();
    let executing_endpoint = start_mock_server(executing).await.unwrap();
    executing_handle.set_persistent_get_state(registered_state(&account));
    executing_handle.set_persistent_get_pubkey(word_to_hex(&guardian.commitment()));
    executing_handle.set_persistent_get_delta_proposal(GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(pending),
    });

    let dir = tempfile::tempdir().unwrap();
    let mut executor = party(
        dir.path(),
        &chain,
        wallet.clone(),
        &account,
        &executing_endpoint,
    )
    .await;
    executor
        .sign_imported_proposal(&mut document)
        .await
        .expect("the ECDSA cosigner reproduces and signs the document");
    assert_eq!(document.signatures.len(), 2);

    executor
        .miden_client
        .seed_transaction_encryption_key(TransactionEncryptionKey::new_unattested(
            b"issue-549".to_vec(),
            SigningKey::new().public_key(),
            Word::default(),
        ))
        .await
        .unwrap();
    executor
        .execute_imported_proposal(&document)
        .await
        .expect("the document executes");
    assert!(
        executing_handle
            .calls()
            .iter()
            .any(|call| call == "push_delta"),
        "the delta was acknowledged"
    );

    chain.prove_block();
    executor.miden_client.sync_state().await.unwrap();
    executor
        .verify_state_commitment()
        .await
        .expect("the executor's state is the chain's");
    assert_eq!(
        executor.account().unwrap().cosigners().unwrap(),
        vec![approvers[0], approvers[1], SignerSpec::new(added, E)]
    );
}

/// An account that stores this client's key under another scheme than the
/// key's own cannot use its approvals, so the client neither proposes nor
/// signs, and nothing reaches GUARDIAN.
#[tokio::test]
async fn a_key_stored_under_another_scheme_neither_proposes_nor_signs() {
    let chain = chain_with_notes(vec![]);
    let falcon = falcon_key();
    let wallet = ecdsa_key();
    let account = account_with(
        &[
            SignerSpec::new(falcon.commitment(), F),
            SignerSpec::new(wallet.commitment(), F),
        ],
        1,
        F,
        59,
    );
    let (endpoint, handle) = guardian_for(&account).await;
    let dir = tempfile::tempdir().unwrap();
    let mut client = party(dir.path(), &chain, wallet.clone(), &account, &endpoint).await;

    let expected = |err: &MultisigError| {
        matches!(
            err,
            MultisigError::KeySchemeMismatch {
                key: E,
                stored: Some(F),
                ..
            }
        )
    };
    let err = client
        .propose_transaction(TransactionType::update_signers(
            1,
            vec![falcon.commitment(), wallet.commitment()],
        ))
        .await
        .unwrap_err();
    assert!(expected(&err), "{err}");
    let err = client.sign_proposal("0x00").await.unwrap_err();
    assert!(expected(&err), "{err}");
    let err = client
        .create_proposal_offline(TransactionType::switch_guardian(
            "http://127.0.0.1:1",
            Word::from([7u32, 7, 7, 7]),
        ))
        .await
        .unwrap_err();
    assert!(expected(&err), "{err}");
    assert!(handle.pushed_proposals().is_empty());

    let dir = tempfile::tempdir().unwrap();
    let mut proposer = party(dir.path(), &chain, falcon, &account, &endpoint).await;
    let payload = pushed_payload(
        &mut proposer,
        &handle,
        TransactionType::add_cosigner(Word::from([3u32, 3, 3, 3])),
    )
    .await;
    assert!(!payload.is_empty(), "a correctly stored key still proposes");
}
