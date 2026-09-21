use std::sync::Arc;

use anyhow::anyhow;
use miden_client::Client;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::rpc::Endpoint;
use miden_client_sqlite_store::SqliteStore;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::asset::Asset;
use miden_protocol::crypto::rand::RandomCoin;

use crate::manifest::NetworkName;

pub type MidenClient = Client<FilesystemKeyStore>;

pub fn endpoint_for(network: NetworkName) -> Endpoint {
    match network {
        NetworkName::Devnet => Endpoint::devnet(),
        NetworkName::Testnet => Endpoint::testnet(),
    }
}

/// One store per network, reused across invocations.
///
/// Scenario accounts get a fresh store so no stale view can be mistaken for
/// chain state, but the treasury is the opposite case: it is a private account,
/// so the node serves only its commitment and the authoritative state lives
/// here. A fresh store per call loses it, and the next transaction then builds
/// on a stale nonce.
pub async fn connect(
    network: NetworkName,
    data_dir: &std::path::Path,
) -> anyhow::Result<MidenClient> {
    std::fs::create_dir_all(data_dir)?;
    let store = SqliteStore::new(data_dir.join(format!("treasury-{}.sqlite", network.as_str())))
        .await
        .map_err(|error| anyhow!("cannot open the local store: {error}"))?;

    let seed: [u32; 4] = rand::random();
    let rng = Box::new(RandomCoin::new(seed.into()));

    let builder = match network {
        NetworkName::Devnet => ClientBuilder::<FilesystemKeyStore>::for_devnet(),
        NetworkName::Testnet => ClientBuilder::<FilesystemKeyStore>::for_testnet(),
    };

    builder
        .store(Arc::new(store))
        .rng(rng)
        .filesystem_keystore(
            data_dir
                .join("keys")
                .to_str()
                .ok_or_else(|| anyhow!("the data directory path is not valid UTF-8"))?,
        )
        .map_err(|error| anyhow!("cannot open the local keystore: {error}"))?
        .build()
        .await
        .map_err(|error| anyhow!("cannot build a Miden client: {error}"))
}

/// Registers the treasury with the local client. Without this the client has no
/// per-account note tag for it, so syncing discovers nothing addressed to it
/// and the account reads as unfunded even when a note is waiting.
pub async fn track_account(
    client: &mut MidenClient,
    data_dir: &std::path::Path,
    account: &Account,
    secret_key: &miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey,
) -> anyhow::Result<()> {
    use miden_client::keystore::Keystore;
    use miden_protocol::account::auth::AuthSecretKey;

    let keystore = FilesystemKeyStore::new(data_dir.join("keys"))
        .map_err(|error| anyhow!("cannot open the keystore: {error}"))?;
    keystore
        .add_key(
            &AuthSecretKey::Falcon512Poseidon2(secret_key.clone()),
            account.id(),
        )
        .await
        .map_err(|error| anyhow!("cannot store the treasury key: {error}"))?;

    // Registering is a one-time step. Once the treasury has transacted, the
    // local record is the authoritative state for a private account, and
    // overwriting it with the pristine account would rewind the nonce.
    let already_tracked = client
        .get_account(account.id())
        .await
        .ok()
        .flatten()
        .is_some();
    if !already_tracked {
        client
            .add_account(account, false)
            .await
            .map_err(|error| anyhow!("cannot track the treasury account: {error}"))?;
    }
    Ok(())
}

pub struct ChainView {
    pub block_number: u32,
    pub account: Option<Account>,
    pub consumable_note_count: usize,
    pub transactions: Vec<(String, String)>,
}

/// Fungible balances held in an account's vault, keyed by faucet.
pub fn vault_balances(account: &Account) -> Vec<(AccountId, u64)> {
    account
        .vault()
        .assets()
        .filter_map(|asset| match asset {
            Asset::Fungible(fungible) => Some((fungible.faucet_id(), fungible.amount().as_u64())),
            Asset::NonFungible(_) => None,
        })
        .collect()
}

pub async fn observe(client: &mut MidenClient, account_id: AccountId) -> anyhow::Result<ChainView> {
    let summary = client
        .sync_state()
        .await
        .map_err(|error| anyhow!("syncing with the network failed: {error}"))?;

    // Absent until the account has transacted, which is the expected state for
    // a treasury whose funding note is still unconsumed.
    let account = client.get_account(account_id).await.ok().flatten();

    let consumable = client
        .get_consumable_notes(Some(account_id))
        .await
        .map(|notes| notes.len())
        .unwrap_or(0);

    let transactions = client
        .get_transactions(miden_client::store::TransactionFilter::All)
        .await
        .map(|records| {
            records
                .into_iter()
                .map(|record| (record.id.to_hex(), format!("{:?}", record.status)))
                .collect()
        })
        .unwrap_or_default();

    Ok(ChainView {
        block_number: summary.block_num.as_u32(),
        account,
        consumable_note_count: consumable,
        transactions,
    })
}
