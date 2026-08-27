//! Shared fixtures for the client test modules: fully offline
//! [`MultisigClient`]s (SQLite store in a temp dir, mock chain RPC, optional
//! mock note transport) and the account/note builders the recovery test
//! suites drive them with.

use std::path::Path;
use std::sync::Arc;

use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::note_transport::NoteTransportClient;
use miden_client::rpc::{Endpoint, NodeRpcClient};
use miden_client::testing::mock::MockRpcApi;
use miden_client::testing::note_transport::{MockNoteTransportApi, MockNoteTransportNode};
use miden_client_sqlite_store::SqliteStore;
use miden_protocol::Word;
use miden_protocol::account::Account;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::asset::FungibleAsset;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::{Note, NoteDetails, NoteType};
use miden_client::Serializable;
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
        Arc::new(GuardianKeyStore::generate()),
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
