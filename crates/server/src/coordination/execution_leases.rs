use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::coordination::leader::{LeaderElector, Lease};
use crate::error::Result;
use crate::storage::execution_lease_name;

/// Hands out the account-scoped lease that fences one Guardian execution.
/// Each execution task acquires under its own holder id, so ownership can
/// transfer between tasks of one replica as well as across replicas.
pub trait ExecutionLeases: Send + Sync {
    fn elector(&self, account_id: &str, holder_id: &str) -> Arc<dyn LeaderElector>;

    /// Whether the leases live in the shared database, so their fences hold across replicas and
    /// the Postgres backend can validate them.
    fn is_shared(&self) -> bool;
}

/// Execution leases backed by `worker_leases` rows.
#[cfg(feature = "postgres")]
pub struct PgExecutionLeases {
    pool: diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>,
}

#[cfg(feature = "postgres")]
impl PgExecutionLeases {
    pub fn new(
        pool: diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>,
    ) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "postgres")]
impl ExecutionLeases for PgExecutionLeases {
    fn elector(&self, account_id: &str, holder_id: &str) -> Arc<dyn LeaderElector> {
        Arc::new(crate::coordination::postgres::PgLeaseElector::new(
            self.pool.clone(),
            execution_lease_name(account_id),
            holder_id,
        ))
    }

    fn is_shared(&self) -> bool {
        true
    }
}

#[derive(Clone)]
struct HeldLease {
    holder_id: String,
    fence_token: i64,
    expires_at: DateTime<Utc>,
}

/// In-process execution leases for single-process deployments. Unlike
/// [`crate::coordination::AlwaysLeader`] they expire and fence: a task that
/// stops renewing loses the lease, and the next holder receives a higher
/// fence token.
#[derive(Clone, Default)]
pub struct InMemoryExecutionLeases {
    leases: Arc<Mutex<HashMap<String, HeldLease>>>,
}

impl InMemoryExecutionLeases {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ExecutionLeases for InMemoryExecutionLeases {
    fn elector(&self, account_id: &str, holder_id: &str) -> Arc<dyn LeaderElector> {
        Arc::new(InMemoryLeaseElector {
            leases: self.leases.clone(),
            name: execution_lease_name(account_id),
            holder_id: holder_id.to_string(),
        })
    }

    fn is_shared(&self) -> bool {
        false
    }
}

struct InMemoryLeaseElector {
    leases: Arc<Mutex<HashMap<String, HeldLease>>>,
    name: String,
    holder_id: String,
}

impl InMemoryLeaseElector {
    fn ttl_from(now: DateTime<Utc>, ttl: Duration) -> DateTime<Utc> {
        now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::MAX)
    }

    fn is_current(&self, held: &HeldLease, lease: &Lease, now: DateTime<Utc>) -> bool {
        held.holder_id == lease.holder_id
            && held.fence_token == lease.fence_token
            && now < held.expires_at
    }
}

#[async_trait]
impl LeaderElector for InMemoryLeaseElector {
    async fn try_acquire(&self, ttl: Duration) -> Result<Option<Lease>> {
        let now = Utc::now();
        let mut leases = self
            .leases
            .lock()
            .expect("execution lease registry poisoned");
        let fence_token = match leases.get(&self.name) {
            Some(held) if held.holder_id == self.holder_id => held.fence_token,
            Some(held) if now < held.expires_at => return Ok(None),
            Some(held) => held.fence_token + 1,
            None => 0,
        };
        let expires_at = Self::ttl_from(now, ttl);
        leases.insert(
            self.name.clone(),
            HeldLease {
                holder_id: self.holder_id.clone(),
                fence_token,
                expires_at,
            },
        );
        Ok(Some(Lease {
            name: self.name.clone(),
            holder_id: self.holder_id.clone(),
            fence_token,
            expires_at,
        }))
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<bool> {
        let now = Utc::now();
        let mut leases = self
            .leases
            .lock()
            .expect("execution lease registry poisoned");
        match leases.get_mut(&lease.name) {
            Some(held) if self.is_current(held, lease, now) => {
                held.expires_at = Self::ttl_from(now, ttl);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn verify_held(&self, lease: &Lease) -> Result<bool> {
        let leases = self
            .leases
            .lock()
            .expect("execution lease registry poisoned");
        Ok(leases
            .get(&lease.name)
            .is_some_and(|held| self.is_current(held, lease, Utc::now())))
    }

    async fn release(&self, lease: Lease) -> Result<()> {
        let now = Utc::now();
        let mut leases = self
            .leases
            .lock()
            .expect("execution lease registry poisoned");
        if let Some(held) = leases.get_mut(&lease.name)
            && self.is_current(held, &lease, now)
        {
            held.expires_at = now;
        }
        Ok(())
    }

    fn supports_fencing(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_holder_per_account_while_the_lease_is_live() {
        let leases = InMemoryExecutionLeases::new();
        let a = leases.elector("account-1", "task-a");
        let b = leases.elector("account-1", "task-b");
        let lease_a = a
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .expect("first holder acquires");
        assert_eq!(lease_a.name, "execution:account-1");
        assert!(
            b.try_acquire(Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn different_accounts_hold_leases_concurrently() {
        let leases = InMemoryExecutionLeases::new();
        let one = leases.elector("account-1", "task-a");
        let two = leases.elector("account-2", "task-b");
        assert!(
            one.try_acquire(Duration::from_secs(60))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            two.try_acquire(Duration::from_secs(60))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_expired_lease_passes_to_a_new_holder_with_a_higher_fence() {
        let leases = InMemoryExecutionLeases::new();
        let a = leases.elector("account-1", "task-a");
        let b = leases.elector("account-1", "task-b");
        let lease_a = a.try_acquire(Duration::ZERO).await.unwrap().unwrap();
        let lease_b = b
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .expect("an expired lease can be taken over");
        assert!(lease_b.fence_token > lease_a.fence_token);
        assert!(!a.renew(&lease_a, Duration::from_secs(60)).await.unwrap());
        assert!(!a.verify_held(&lease_a).await.unwrap());
        assert!(b.verify_held(&lease_b).await.unwrap());
    }

    #[tokio::test]
    async fn release_hands_over_without_rewinding_the_fence() {
        let leases = InMemoryExecutionLeases::new();
        let a = leases.elector("account-1", "task-a");
        let b = leases.elector("account-1", "task-b");
        let lease_a = a
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        a.release(lease_a.clone()).await.unwrap();
        let lease_b = b
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert!(lease_b.fence_token > lease_a.fence_token);
    }
}
