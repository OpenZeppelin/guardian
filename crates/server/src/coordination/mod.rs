pub mod block_watch_store;
pub mod challenge_store;
pub mod execution_leases;
pub mod leader;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod read_records;
pub mod session_store;
pub mod stats_store;

pub use block_watch_store::{
    BlockWatchStore, Cursor, CursorWrite, FencedWrite, InMemoryBlockWatchStore, NewObservation,
    Observation, WalkRequests,
};
pub use challenge_store::{
    ChallengePayload, ChallengeStore, InMemoryChallengeStore, StoredChallenge,
};
#[cfg(feature = "postgres")]
pub use execution_leases::PgExecutionLeases;
pub use execution_leases::{
    ExecutionLeases, InMemoryExecutionLeases, Renewal, lease_deadline, release_quietly,
};
pub use leader::{AlwaysLeader, LeaderElector, Lease};
pub use read_records::{FileReadRecords, InMemoryReadRecords, ReadRecordOutcome, ReadRecordStore};
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

/// Single-owner lease of the chain-driven release sweep (issue #434).
pub const RELEASE_SWEEP_LEASE: &str = "release_sweep";

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
    /// Single-owner lease for the chain-driven release sweep (issue #434).
    pub release_sweep_leader: Arc<dyn LeaderElector>,
    /// Account-scoped leases that fence Guardian executions (issue #254).
    pub execution_leases: Arc<dyn ExecutionLeases>,
    /// Shared publication store for the `/dashboard/stats` aggregate.
    pub stats_store: Arc<dyn StatsStore>,
    /// Block watch cursor and observations (issue #585), written by the
    /// release sweep lease holder.
    pub block_watch_store: Arc<dyn BlockWatchStore>,
    /// Miden account sessions.
    pub miden_sessions: Arc<dyn SessionStore>,
    /// Record of accepted session reads (`unique` read replay policy).
    pub read_records: Arc<dyn ReadRecordStore>,
    #[cfg(feature = "evm")]
    pub evm_sessions: Arc<dyn SessionStore>,
    #[cfg(feature = "evm")]
    pub evm_challenges: Arc<dyn ChallengeStore>,
}

impl CoordinationHandles {
    /// Single-process handles whose block watch cursor survives restarts
    /// in `<block_watch_dir>/cursor.json` (the filesystem backend).
    pub fn in_memory_with_block_watch_dir(
        block_watch_dir: std::path::PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            block_watch_store: Arc::new(InMemoryBlockWatchStore::persistent(block_watch_dir)?),
            ..Self::in_memory()
        })
    }

    pub fn in_memory() -> Self {
        Self {
            mode: CoordinationMode::SingleProcess,
            operator_sessions: Arc::new(InMemorySessionStore::new()),
            operator_challenges: Arc::new(InMemoryChallengeStore::new()),
            leader: Arc::new(AlwaysLeader::new(CANONICALIZATION_LEASE, "single-process")),
            stats_leader: Arc::new(AlwaysLeader::new(DASHBOARD_STATS_LEASE, "single-process")),
            release_sweep_leader: Arc::new(AlwaysLeader::new(
                RELEASE_SWEEP_LEASE,
                "single-process",
            )),
            execution_leases: Arc::new(InMemoryExecutionLeases::new()),
            stats_store: Arc::new(InMemoryStatsStore::new()),
            block_watch_store: Arc::new(InMemoryBlockWatchStore::new()),
            miden_sessions: Arc::new(InMemorySessionStore::new()),
            read_records: Arc::new(InMemoryReadRecords::new()),
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
        use postgres::{
            PgBlockWatchStore, PgChallengeStore, PgLeaseElector, PgReadRecords, PgSessionStore,
            PgStatsStore,
        };
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
                holder_id.clone(),
            )),
            release_sweep_leader: Arc::new(PgLeaseElector::new(
                pool.clone(),
                RELEASE_SWEEP_LEASE,
                holder_id,
            )),
            execution_leases: Arc::new(PgExecutionLeases::new(pool.clone())),
            stats_store: Arc::new(PgStatsStore::new(pool.clone(), cipher)),
            read_records: Arc::new(PgReadRecords::new(pool.clone())),
            block_watch_store: Arc::new(PgBlockWatchStore::new(pool.clone())),
            miden_sessions: Arc::new(PgSessionStore::new(pool.clone(), Realm::Miden)),
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
    Miden,
}

impl Realm {
    pub fn as_str(self) -> &'static str {
        match self {
            Realm::Operator => "operator",
            Realm::Evm => "evm",
            Realm::Miden => "miden",
        }
    }
}
