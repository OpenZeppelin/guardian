use std::path::Path;
use std::sync::Arc;

use miden_client::crypto::RpoRandomCoin;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::{Client, Deserializable, ExecutionOptions, Serializable, Word};
use miden_client_sqlite_store::SqliteStore;
use miden_objects::{MAX_TX_EXECUTION_CYCLES, MIN_TX_EXECUTION_CYCLES};

pub fn commitment_from_hex(hex_commitment: &str) -> Result<Word, String> {
    let trimmed = hex_commitment.strip_prefix("0x").unwrap_or(hex_commitment);
    let bytes = hex::decode(trimmed)
        .map_err(|err| format!("Failed to decode commitment hex '{hex_commitment}': {err}"))?;

    Word::read_from_bytes(&bytes)
        .map_err(|err| format!("Failed to deserialize commitment word '{hex_commitment}': {err}"))
}

pub async fn create_miden_client(
    data_dir: &Path,
    endpoint: &Endpoint,
) -> Result<Client<()>, String> {
    let store_path = data_dir.join("miden-client.sqlite");
    let store = SqliteStore::new(store_path)
        .await
        .map_err(|err| format!("Failed to open SQLite store: {err}"))?;
    let store = Arc::new(store);

    let rng = Box::new(RpoRandomCoin::new(Word::default()));
    let exec_options = ExecutionOptions::new(
        Some(MAX_TX_EXECUTION_CYCLES),
        MIN_TX_EXECUTION_CYCLES,
        false,
        true,
    )
    .map_err(|err| format!("Failed to build execution options: {err}"))?;

    let grpc_client = GrpcClient::new(endpoint, 10_000);
    let rpc_client: Arc<dyn NodeRpcClient> = Arc::new(grpc_client);

    Client::new(
        rpc_client,
        rng,
        store,
        None,
        exec_options,
        Some(20),
        Some(256),
        None,
    )
    .await
    .map_err(|err| format!("Failed to create Miden client: {err}"))
}

pub fn format_word_as_hex(word: &Word) -> String {
    format!("0x{}", hex::encode(word.to_bytes()))
}
