pub mod challenge_store;
pub mod leader;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod session_store;

pub use challenge_store::{
    ChallengePayload, ChallengeStore, InMemoryChallengeStore, StoredChallenge,
};
pub use leader::{AlwaysLeader, LeaderElector, Lease};
pub use session_store::{
    InMemorySessionStore, SessionKey, SessionStore, SessionSubject, StoredSession,
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

/// Coordination store handles selected by the storage backend, threaded from the
/// storage builder (where the Postgres pool is available) into the realm-scoped
/// consumers.
#[derive(Clone)]
pub struct CoordinationHandles {
    pub mode: CoordinationMode,
    pub operator_sessions: Arc<dyn SessionStore>,
    pub operator_challenges: Arc<dyn ChallengeStore>,
    pub leader: Arc<dyn LeaderElector>,
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
            #[cfg(feature = "evm")]
            evm_sessions: Arc::new(InMemorySessionStore::new()),
            #[cfg(feature = "evm")]
            evm_challenges: Arc::new(InMemoryChallengeStore::new()),
        }
    }

    #[cfg(feature = "postgres")]
    pub fn postgres(
        pool: diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>,
        holder_id: String,
    ) -> Self {
        use postgres::{PgChallengeStore, PgLeaseElector, PgSessionStore};
        Self {
            mode: CoordinationMode::Shared,
            operator_sessions: Arc::new(PgSessionStore::new(pool.clone(), Realm::Operator)),
            operator_challenges: Arc::new(PgChallengeStore::new(pool.clone(), Realm::Operator)),
            leader: Arc::new(PgLeaseElector::new(
                pool.clone(),
                CANONICALIZATION_LEASE,
                holder_id,
            )),
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
