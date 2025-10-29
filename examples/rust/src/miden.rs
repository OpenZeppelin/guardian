use std::sync::Arc;
use miden_client::{Client, crypto::RpoRandomCoin, rpc::TonicRpcClient, transactions::{TransactionRequest, TransactionResult}};
use miden_client_sqlite_store::SqliteStore;
use miden_keystore::FilesystemKeyStore;
use miden_objects::{Felt, Word};
use miden_objects::account::{Account, AccountId};
use miden_objects::crypto::dsa::rpo_falcon512::Signature;
use private_state_manager_client::ClientResult;
use rand_chacha::ChaCha20Rng;
use tempfile::TempDir;

pub async fn create_client(
    keystore: &FilesystemKeyStore<ChaCha20Rng>,
) -> ClientResult<Client<TonicRpcClient, RpoRandomCoin, SqliteStore>> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir for store");
    let store_path = temp_dir.path().join("miden.db");

    let sqlite_store = SqliteStore::new(store_path.try_into()?)
        .await
        .map_err(|e| format!("Failed to create store: {}", e))?;
    let store = Arc::new(sqlite_store);

    let coin_seed: [u64; 4] = [42, 42, 42, 42];
    let rng = RpoRandomCoin::new(coin_seed.map(Felt::new));

    let miden_node_url = std::env::var("MIDEN_NODE_URL")
        .unwrap_or_else(|_| "https://rpc.testnet.miden.io".to_string());

    let (protocol, host, port) = parse_endpoint(&miden_node_url)?;
    let endpoint = miden_client::rpc::Endpoint::new(protocol, host, port);

    let rpc_client = Arc::new(
        TonicRpcClient::new(&endpoint, 10_000)
            .map_err(|e| format!("Failed to create RPC client: {}", e))?
    );

    let keystore_arc = Arc::new(keystore.clone());

    Client::new(
        rpc_client,
        rng,
        store,
        Some(keystore_arc),
        false,
        None,
        None,
        None,
    )
    .map_err(|e| format!("Failed to create client: {}", e).into())
}

fn parse_endpoint(url: &str) -> ClientResult<(String, String, Option<u16>)> {
    let url = url.trim_end_matches('/');

    if let Some(rest) = url.strip_prefix("https://") {
        let parts: Vec<&str> = rest.split(':').collect();
        let host = parts[0].to_string();
        let port = if parts.len() > 1 {
            Some(parts[1].parse().map_err(|_| "Invalid port")?)
        } else {
            Some(443)
        };
        Ok(("https".to_string(), host, port))
    } else if let Some(rest) = url.strip_prefix("http://") {
        let parts: Vec<&str> = rest.split(':').collect();
        let host = parts[0].to_string();
        let port = if parts.len() > 1 {
            Some(parts[1].parse().map_err(|_| "Invalid port")?)
        } else {
            Some(80)
        };
        Ok(("http".to_string(), host, port))
    } else {
        Err("URL must start with http:// or https://".into())
    }
}

pub async fn import_and_sync_account(
    client: &mut Client<TonicRpcClient, RpoRandomCoin, SqliteStore>,
    account: &Account,
    seed: [u8; 32],
) -> ClientResult<()> {
    client.insert_account(account, Some(seed), &Default::default())
        .map_err(|e| format!("Failed to import account: {}", e))?;

    client.sync_state()
        .await
        .map_err(|e| format!("Failed to sync state: {}", e))?;

    Ok(())
}

pub async fn execute_transaction(
    client: &mut Client<TonicRpcClient, RpoRandomCoin, SqliteStore>,
    account_id: AccountId,
    tx_request: TransactionRequest,
) -> ClientResult<TransactionResult> {
    client.new_transaction(account_id, tx_request)
        .await
        .map_err(|e| format!("Failed to execute transaction: {}", e).into())
}

pub async fn submit_transaction(
    client: &mut Client<TonicRpcClient, RpoRandomCoin, SqliteStore>,
    tx_result: TransactionResult,
) -> ClientResult<()> {
    println!("  Proving and submitting transaction (this may take 10-30 seconds)...");
    let start = std::time::Instant::now();

    client.submit_transaction(tx_result)
        .await
        .map_err(|e| format!("Failed to submit transaction: {}", e))?;

    println!("  ✓ Transaction proven and submitted in {:?}", start.elapsed());

    Ok(())
}

pub fn build_transaction_with_psm_signature(
    psm_signature: Signature,
) -> TransactionRequest {
    let mut tx_request = TransactionRequest::builder();

    tx_request = tx_request.extend_advice_map([(
        Word::default(),
        psm_signature.to_bytes().as_slice(),
    )]);

    tx_request.build()
}
