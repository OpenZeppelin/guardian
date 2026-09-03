//! Shared fixtures for the crate's test modules: fully offline
//! [`MultisigClient`]s (SQLite store in a temp dir, mock chain RPC, optional
//! mock note transport), the account/note builders the recovery test
//! suites drive them with, and the fee-conversion fixtures the create and
//! rebuild paths are asserted against.

use std::path::Path;
use std::sync::Arc;

use miden_client::Serializable;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::note_transport::NoteTransportClient;
use miden_client::rpc::{Endpoint, NodeRpcClient};
use miden_client::testing::account_id::{
    ACCOUNT_ID_FEE_FAUCET, ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET_1,
};
use miden_client::testing::mock::MockRpcApi;
use miden_client::testing::note_transport::{MockNoteTransportApi, MockNoteTransportNode};
use miden_client::transaction::{ChainAnchor, TransactionRequest, TransactionRequestBuilder};
use miden_client_sqlite_store::SqliteStore;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::asset::FungibleAsset;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::{Note, NoteDetails, NoteType};
use miden_standards::account::auth::{FeeConversionInfo, commit_fee_conversion_info};
use miden_standards::note::P2idNote;
use miden_tx::utils::sync::RwLock;

use super::MultisigClient;
use crate::keystore::GuardianKeyStore;
use crate::prover::ProverConfig;
use crate::rpc::RpcConfig;

/// Core offline constructor: SQLite store in `dir`, `node` as the inner
/// Miden client's RPC, optional note transport, and an unreachable GUARDIAN
/// endpoint. Returns the store handle for tests that seed records directly.
///
/// The multisig client's direct node channel is left endpoint-built (and
/// unreachable) — use [`offline_client_with_node`] to inject `node` there
/// too.
pub(crate) async fn offline_client_parts(
    dir: &Path,
    node: Arc<dyn NodeRpcClient>,
    transport: Option<Arc<dyn NoteTransportClient>>,
) -> (MultisigClient, Arc<SqliteStore>) {
    offline_client_parts_with_keystore(dir, node, transport, Arc::new(GuardianKeyStore::generate()))
        .await
}

/// [`offline_client_parts`] with an injected keystore, for tests that need
/// the client's signer commitment known up front (e.g. to build a multisig
/// account whose cosigner set contains this client's key).
pub(crate) async fn offline_client_parts_with_keystore(
    dir: &Path,
    node: Arc<dyn NodeRpcClient>,
    transport: Option<Arc<dyn NoteTransportClient>>,
    keystore: Arc<GuardianKeyStore>,
) -> (MultisigClient, Arc<SqliteStore>) {
    let store = Arc::new(
        SqliteStore::new(dir.join("store.sqlite3"))
            .await
            .expect("sqlite store opens"),
    );
    let keystore_dir = dir.join("keys");
    std::fs::create_dir_all(&keystore_dir).expect("keystore dir");

    let mut builder = ClientBuilder::<FilesystemKeyStore>::new()
        .rpc(node)
        .store(store.clone())
        .filesystem_keystore(keystore_dir)
        .expect("keystore opens");
    if let Some(transport) = transport {
        builder = builder.note_transport(transport);
    }
    let miden_client = builder.build().await.expect("miden client builds");

    let client = MultisigClient::new(
        miden_client,
        keystore,
        "http://localhost:1".to_string(),
        dir.to_path_buf(),
        Endpoint::localhost(),
        None,
        ProverConfig::new(),
        RpcConfig::new(),
    );
    (client, store)
}

/// Fully offline MultisigClient: mock chain RPC for the inner client, SQLite
/// store in a temp dir, and (optionally) the upstream mock note transport.
/// The direct node channel stays endpoint-built and unreachable.
pub(crate) async fn offline_client(
    dir: &Path,
    transport: Option<Arc<dyn NoteTransportClient>>,
) -> MultisigClient {
    offline_client_parts(dir, Arc::new(MockRpcApi::default()), transport)
        .await
        .0
}

/// Fully offline MultisigClient with `node` injected into both the inner
/// Miden client and the direct node channel, so node-backed primitives and
/// store syncs see the same mock chain.
pub(crate) async fn offline_client_with_node(
    dir: &Path,
    node: Arc<dyn NodeRpcClient>,
) -> MultisigClient {
    offline_client_with_node_parts(dir, node).await.0
}

/// [`offline_client_with_node`] variant that also returns the store handle,
/// for tests that seed records directly.
pub(crate) async fn offline_client_with_node_parts(
    dir: &Path,
    node: Arc<dyn NodeRpcClient>,
) -> (MultisigClient, Arc<SqliteStore>) {
    let (mut client, store) = offline_client_parts(dir, node.clone(), None).await;
    client.set_node_rpc_client(node);
    (client, store)
}

/// The upstream mock note transport and its backing node handle, for seeding
/// a transport backlog.
pub(crate) fn mock_transport() -> (
    Arc<RwLock<MockNoteTransportNode>>,
    Arc<dyn NoteTransportClient>,
) {
    let node = Arc::new(RwLock::new(MockNoteTransportNode::new()));
    let api: Arc<dyn NoteTransportClient> = Arc::new(MockNoteTransportApi::new(node.clone()));
    (node, api)
}

/// Adds `note` to the mock transport's backlog the way a sender would relay
/// it: header plus serialized details.
pub(crate) fn add_to_transport(node: &Arc<RwLock<MockNoteTransportNode>>, note: Note) {
    let header = *note.header();
    let details_bytes = NoteDetails::from(note).to_bytes();
    node.write().add_note(header, details_bytes);
}

/// A plain wallet account with a fresh (per-`seed`) seed; enough for the
/// store-side account/tag behavior under test (no multisig components
/// needed).
pub(crate) fn test_wallet(seed: u8) -> Account {
    use miden_client::account::component::BasicWallet;
    use miden_client::account::{AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountType};
    use miden_protocol::account::auth::AuthScheme;
    use miden_standards::account::auth::{Approver, AuthSingleSig};

    let key_pair = AuthSecretKey::new_falcon512_poseidon2();
    let auth_component = AuthSingleSig::new(Approver::new(
        key_pair.public_key().to_commitment(),
        AuthScheme::Falcon512Poseidon2,
    ));
    AccountBuilder::new([seed; 32])
        .account_type(AccountType::Private)
        .with_component(auth_component)
        .with_component(BasicWallet)
        .build_with_schema_commitment()
        .expect("test wallet builds")
}

/// A mock chain holding the given output notes in its first post-genesis
/// block, with the tip advanced a few blocks past it — the note-bearing
/// block sits strictly below the tip.
pub(crate) fn chain_with_notes(
    notes: Vec<miden_protocol::transaction::RawOutputNote>,
) -> Arc<MockRpcApi> {
    let mut builder = miden_client::testing::MockChain::builder();
    for note in notes {
        builder.add_output_note(note);
    }
    let api = Arc::new(MockRpcApi::new(builder.build().expect("mock chain builds")));
    api.advance_blocks(4);
    api
}

/// The fee faucet the fee-conversion fixtures build their chain at.
///
/// Deliberately not [`ACCOUNT_ID_FEE_FAUCET`], which is what `MockChainBuilder` uses when no
/// faucet is set: a fixture at the default cannot tell a faucet read from the chain apart from
/// one hardcoded to the default.
pub(crate) fn chain_fee_faucet() -> AccountId {
    ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET_1
        .try_into()
        .expect("valid fee faucet id")
}

/// A second fee faucet, distinct from both [`chain_fee_faucet`] and the builder default.
///
/// A fixture where the anchor's faucet and the tip's faucet agree cannot tell "read the anchor"
/// apart from "read the tip"; giving the two chains different faucets is what separates them.
pub(crate) fn alternate_fee_faucet() -> AccountId {
    ACCOUNT_ID_FEE_FAUCET
        .try_into()
        .expect("valid fee faucet id")
}

/// A mock chain whose block headers name `fee_faucet_id` in their fee parameters, with the tip
/// advanced a couple of blocks past genesis.
pub(crate) fn chain_at_fee_faucet(fee_faucet_id: AccountId) -> Arc<MockRpcApi> {
    let chain = miden_client::testing::MockChain::builder()
        .fee_faucet_id(fee_faucet_id)
        .build()
        .expect("mock chain builds");
    let api = Arc::new(MockRpcApi::new(chain));
    api.advance_blocks(2);
    api
}

/// A [`MultisigClient`] synced to a chain reporting `fee_faucet_id`, so
/// `resolve_fee_conversion_info` reads that faucet.
pub(crate) async fn client_at_fee_faucet(dir: &Path, fee_faucet_id: AccountId) -> MultisigClient {
    let mut client = offline_client_with_node(dir, chain_at_fee_faucet(fee_faucet_id)).await;
    client
        .miden_client
        .sync_state()
        .await
        .expect("the mock chain syncs");
    client
}

/// A [`ChainAnchor`] captured at the tip of a chain reporting `fee_faucet_id`.
///
/// A rebuild reads its faucet from the anchor that travels with the proposal, so an anchor
/// captured on a different chain than the executing client's is what makes the two sources
/// distinguishable.
pub(crate) async fn chain_anchor_at_fee_faucet(
    dir: &Path,
    fee_faucet_id: AccountId,
) -> ChainAnchor {
    let client = client_at_fee_faucet(dir, fee_faucet_id).await;
    let request = TransactionRequestBuilder::new()
        .build()
        .expect("an empty request builds");

    client
        .miden_client
        .chain_anchor_for_request(&request)
        .await
        .expect("the mock chain yields an anchor")
}

/// A guarded-multisig account, the account shape every create path builds requests for.
pub(crate) fn guarded_multisig_account() -> Account {
    let signer_commitment = SecretKey::new().public_key().to_commitment();

    MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
        1,
        vec![signer_commitment],
        Word::from([9u32, 8, 7, 6]),
    ))
    .build()
    .expect("guarded multisig account builds")
}

/// Asserts `request` commits `hash(one_to_one(fee_faucet_id) || salt)` as its auth arg and
/// carries the commitment's preimage in its advice map.
///
/// Both halves matter: the auth arg alone is what the auth procedure checks against, and without
/// the preimage `load_conversion_info` finds nothing to open it with.
pub(crate) fn assert_commits_fee_faucet(
    request: &TransactionRequest,
    fee_faucet_id: AccountId,
    salt: Word,
) {
    let (expected_auth_arg, expected_preimage) =
        commit_fee_conversion_info(FeeConversionInfo::one_to_one(fee_faucet_id), salt);

    assert_eq!(
        *request.auth_arg(),
        Some(expected_auth_arg),
        "the auth arg must commit the conversion info for {fee_faucet_id}"
    );
    assert_ne!(
        *request.auth_arg(),
        Some(salt),
        "the salt must not be passed through bare"
    );
    assert_eq!(
        request
            .advice_map()
            .get(&expected_auth_arg)
            .map(|values| values.to_vec()),
        Some(expected_preimage),
        "the commitment preimage must be reachable from the advice map"
    );
}

/// A distinct (per `seed`) P2ID note addressed at `target` from an unrelated
/// sender wallet.
pub(crate) fn p2id_note_for(target: &Account, seed: u32, note_type: NoteType) -> Note {
    let mut rng = RandomCoin::new(Word::from(&[seed, 0, 0, 0]));
    P2idNote::builder()
        .sender(test_wallet(200).id())
        .target(target.id())
        .asset(FungibleAsset::mock(1))
        .note_type(note_type)
        .generate_serial_number(&mut rng)
        .build()
        .expect("p2id note builds")
        .into()
}
