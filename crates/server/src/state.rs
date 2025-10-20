use crate::canonicalization::CanonicalizationMode;
use crate::network::NetworkClient;
use crate::storage::{MetadataStore, StorageRegistry};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub storage: StorageRegistry,
    pub metadata: Arc<dyn MetadataStore>,
    pub network_client: Arc<Mutex<dyn NetworkClient>>,
    pub canonicalization_mode: CanonicalizationMode,
}
