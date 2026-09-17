pub mod challenge_store;
pub mod leader;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod session_store;
pub mod stats_store;

pub use challenge_store::{
    ChallengePayload, ChallengeStore, InMemoryChallengeStore, StoredChallenge,
};
pub use leader::{AlwaysLeader, LeaderElector, Lease};
pub use session_store::{
    InMemorySessionStore, SessionKey, SessionStore, SessionSubject, StoredSession,
};
pub use stats_store::{
    InMemoryStatsStore, PublishOutcome, PublishedStats, RefreshRequestOutcome, StatsControl,
    StatsStore,
};

use std::sync::Arc;

/// Whether coordination is backed by the shared external store (replica-safe) or
/// is single-process in-memory. Carried on the handles so the startup log and
/// guards reflect the **actual** resolved backing, not an inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinationMode {
    Shared,
    SingleProcess,
}

impl CoordinationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CoordinationMode::Shared => "shared",
            CoordinationMode::SingleProcess => "single-process",
        }
    }
}

/// Lease name for the single-owner canonicalization worker.
pub const CANONICALIZATION_LEASE: &str = "canonicalization";

/// Lease name for the single-owner `/dashboard/stats` refresher (issue #371).
pub const DASHBOARD_STATS_LEASE: &str = "dashboard_stats";

/// Coordination store handles selected by the storage backend, threaded from the
/// storage builder (where the Postgres pool is available) into the realm-scoped
/// consumers.
#[derive(Clone)]
pub struct CoordinationHandles {
    pub mode: CoordinationMode,
    pub operator_sessions: Arc<dyn SessionStore>,
    pub operator_challenges: Arc<dyn ChallengeStore>,
    pub leader: Arc<dyn LeaderElector>,
    /// Single-owner lease for the `/dashboard/stats` refresher.
    pub stats_leader: Arc<dyn LeaderElector>,
    /// Shared publication store for the `/dashboard/stats` aggregate.
    pub stats_store: Arc<dyn StatsStore>,
    #[cfg(feature = "evm")]
    pub evm_sessions: Arc<dyn SessionStore>,
    #[cfg(feature = "evm")]
    pub evm_challenges: Arc<dyn ChallengeStore>,
}

impl CoordinationHandles {
    pub fn in_memory() -> Self {
        Self {
            mode: CoordinationMode::SingleProcess,
            operator_sessions: Arc::new(InMemorySessionStore::new()),
            operator_challenges: Arc::new(InMemoryChallengeStore::new()),
            leader: Arc::new(AlwaysLeader::new(CANONICALIZATION_LEASE, "single-process")),
            stats_leader: Arc::new(AlwaysLeader::new(DASHBOARD_STATS_LEASE, "single-process")),
            stats_store: Arc::new(InMemoryStatsStore::new()),
            #[cfg(feature = "evm")]
            evm_sessions: Arc::new(InMemorySessionStore::new()),
            #[cfg(feature = "evm")]
            evm_challenges: Arc::new(InMemoryChallengeStore::new()),
        }
    }

    /// `cipher` is the storage cipher when at-rest encryption is
    /// configured; the published `/dashboard/stats` snapshot is sealed
    /// with it so the at-rest boundary stays the one the encryption
    /// config describes.
    #[cfg(feature = "postgres")]
    pub(crate) fn postgres(
        pool: diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>,
        holder_id: String,
        cipher: Option<Arc<dyn crate::storage::encryption::cipher::StorageCipher>>,
    ) -> Self {
        use postgres::{PgChallengeStore, PgLeaseElector, PgSessionStore, PgStatsStore};
        Self {
            mode: CoordinationMode::Shared,
            operator_sessions: Arc::new(PgSessionStore::new(pool.clone(), Realm::Operator)),
            operator_challenges: Arc::new(PgChallengeStore::new(pool.clone(), Realm::Operator)),
            leader: Arc::new(PgLeaseElector::new(
                pool.clone(),
                CANONICALIZATION_LEASE,
                holder_id.clone(),
            )),
            stats_leader: Arc::new(PgLeaseElector::new(
                pool.clone(),
                DASHBOARD_STATS_LEASE,
                holder_id,
            )),
            stats_store: Arc::new(PgStatsStore::new(pool.clone(), cipher)),
            #[cfg(feature = "evm")]
            evm_sessions: Arc::new(PgSessionStore::new(pool.clone(), Realm::Evm)),
            #[cfg(feature = "evm")]
            evm_challenges: Arc::new(PgChallengeStore::new(pool, Realm::Evm)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Realm {
    Operator,
    Evm,
}

impl Realm {
    pub fn as_str(self) -> &'static str {
        match self {
            Realm::Operator => "operator",
            Realm::Evm => "evm",
        }
    }
}
