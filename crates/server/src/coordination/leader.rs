use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::time::Duration;

use crate::error::Result;

/// A held leadership lease. `fence_token` strictly increases on every
/// (re)acquisition so a superseded holder can be detected at the write boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub name: String,
    pub holder_id: String,
    pub fence_token: i64,
    pub expires_at: DateTime<Utc>,
}

/// Coordinates single-owner background work across replicas. `renew` runs on its
/// own timer concurrent with the protected work; a `false` return means the lease
/// was lost. State-mutating writes are fenced by the storage backend itself: the
/// holder attaches its lease identity ([`crate::storage::LeaseFence`]) to each
/// canonicalization write and a fencing backend validates and locks the lease row
/// in the same transaction, so `verify_held` is a diagnostic read, not the write
/// guard. `release` expires the lease in place for prompt handover on a planned
/// stop; the server has no graceful shutdown hook yet, so nothing calls it in
/// production paths and a stopped holder hands over only after TTL expiry
/// (documented in the horizontal-scaling runbook).
#[async_trait]
pub trait LeaderElector: Send + Sync {
    async fn try_acquire(&self, ttl: Duration) -> Result<Option<Lease>>;
    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<bool>;
    async fn verify_held(&self, lease: &Lease) -> Result<bool>;
    async fn release(&self, lease: Lease) -> Result<()>;

    /// Whether leases from this elector are backed by a shared-store row
    /// that fenced storage backends can validate and lock inside the
    /// same transaction as a canonicalization write. Single-process
    /// electors return `false`: their leases have no row, so fenced
    /// writes skip the lease predicate. Required (no default) so a
    /// backend cannot silently lose its fencing in a merge.
    fn supports_fencing(&self) -> bool;
}

/// Single-process elector: the only replica is always the leader. Used on the
/// filesystem backend, where no shared coordination store exists.
pub struct AlwaysLeader {
    name: String,
    holder_id: String,
}

impl AlwaysLeader {
    pub fn new(name: impl Into<String>, holder_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            holder_id: holder_id.into(),
        }
    }

    fn lease(&self) -> Lease {
        Lease {
            name: self.name.clone(),
            holder_id: self.holder_id.clone(),
            fence_token: 0,
            expires_at: DateTime::<Utc>::MAX_UTC,
        }
    }
}

#[async_trait]
impl LeaderElector for AlwaysLeader {
    async fn try_acquire(&self, _ttl: Duration) -> Result<Option<Lease>> {
        Ok(Some(self.lease()))
    }

    async fn renew(&self, _lease: &Lease, _ttl: Duration) -> Result<bool> {
        Ok(true)
    }

    async fn verify_held(&self, _lease: &Lease) -> Result<bool> {
        Ok(true)
    }

    async fn release(&self, _lease: Lease) -> Result<()> {
        Ok(())
    }

    fn supports_fencing(&self) -> bool {
        false
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn always_leader_acquires_renews_and_verifies() {
        let elector = AlwaysLeader::new("canonicalization", "single-process");
        let lease = elector
            .try_acquire(Duration::from_secs(30))
            .await
            .unwrap()
            .expect("always leader acquires");
        assert_eq!(lease.holder_id, "single-process");
        assert!(
            elector
                .renew(&lease, Duration::from_secs(30))
                .await
                .unwrap()
        );
        assert!(elector.verify_held(&lease).await.unwrap());
    }
}
