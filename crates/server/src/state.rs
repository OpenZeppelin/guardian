use crate::ack::Acknowledger;
use crate::canonicalization::CanonicalizationConfig;
use crate::clock::Clock;
use crate::metadata::MetadataStore;
use crate::network::NetworkClient;
use crate::storage::StorageRegistry;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub storage: StorageRegistry,
    pub metadata: Arc<dyn MetadataStore>,
    pub network_client: Arc<Mutex<dyn NetworkClient>>,
    pub ack: Acknowledger,
    pub canonicalization: Option<CanonicalizationConfig>,
    pub clock: Arc<dyn Clock>,
}
