use crate::metadata::MetadataStore;
use crate::storage::StorageBackend;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn StorageBackend>,
    pub metadata: Arc<Mutex<dyn MetadataStore>>,
}
