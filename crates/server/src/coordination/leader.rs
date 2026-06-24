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
/// was lost. `verify_held` is the mandatory fence check the holder runs
/// immediately before any state-mutating write.
#[async_trait]
pub trait LeaderElector: Send + Sync {
    async fn try_acquire(&self, ttl: Duration) -> Result<Option<Lease>>;
    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<bool>;
    async fn verify_held(&self, lease: &Lease) -> Result<bool>;
    async fn release(&self, lease: Lease) -> Result<()>;
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
