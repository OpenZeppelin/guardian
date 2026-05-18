use crate::ack::AckRegistry;
use crate::audit::SharedAuditor;
use crate::builder::clock::Clock;
use crate::canonicalization::CanonicalizationConfig;
use crate::dashboard::DashboardState;
#[cfg(feature = "evm")]
use crate::evm::EvmAppState;
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
    pub dashboard: Arc<DashboardState>,
    /// Always-on audit writer (feature 006-operator-authz). On
    /// Postgres builds this is `PostgresAuditor`; on filesystem-only
    /// builds it is `LogAuditor` and a one-shot startup warning is
    /// emitted at construction time (FR-020 / FR-021).
    pub auditor: SharedAuditor,
    #[cfg(feature = "evm")]
    pub evm: Arc<EvmAppState>,
}
