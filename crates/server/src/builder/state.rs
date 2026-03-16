use crate::ack::AckRegistry;
use crate::builder::clock::Clock;
use crate::canonicalization::CanonicalizationConfig;
use crate::metadata::MetadataStore;
use crate::network::NetworkClient;
use crate::storage::StorageBackend;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn StorageBackend>,
    pub metadata: Arc<dyn MetadataStore>,
    pub network_client: Arc<Mutex<dyn NetworkClient>>,
    pub ack: AckRegistry,
    pub canonicalization: Option<CanonicalizationConfig>,
    pub clock: Arc<dyn Clock>,
}
