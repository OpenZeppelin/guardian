use crate::canonicalization::CanonicalizationMode;
use crate::clock::Clock;
use crate::network::NetworkClient;
use crate::storage::{MetadataStore, StorageRegistry};
use miden_keystore::FilesystemKeyStore;
use rand_chacha::ChaCha20Rng;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub storage: StorageRegistry,
    pub metadata: Arc<dyn MetadataStore>,
    pub network_client: Arc<Mutex<dyn NetworkClient>>,
    pub keystore: Arc<FilesystemKeyStore<ChaCha20Rng>>,
    pub canonicalization_mode: CanonicalizationMode,
    pub clock: Arc<dyn Clock>,
}
