use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::OptionalExtension;
use diesel::sql_types::{BigInt, Double, Integer, Text, Timestamptz};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::coordination::leader::{LeaderElector, Lease};
use crate::error::{GuardianError, Result};

#[derive(diesel::QueryableByName)]
struct AcquireRow {
    #[diesel(sql_type = BigInt)]
    fence_token: i64,
    #[diesel(sql_type = Timestamptz)]
    expires_at: DateTime<Utc>,
}

#[derive(diesel::QueryableByName)]
struct HeldRow {
    #[diesel(sql_type = Integer)]
    #[allow(dead_code)]
    held: i32,
}

/// Postgres lease elector backed by one `worker_leases` row. All timing uses the
/// database clock so replicas agree. `fence_token` only advances when ownership
/// changes (a steal), so a holder can detect supersession at its write boundary.
pub struct PgLeaseElector {
    pool: Pool<AsyncPgConnection>,
    lease_name: String,
    holder_id: String,
}

impl PgLeaseElector {
    pub fn new(
        pool: Pool<AsyncPgConnection>,
        lease_name: impl Into<String>,
        holder_id: impl Into<String>,
    ) -> Self {
        Self {
            pool,
            lease_name: lease_name.into(),
            holder_id: holder_id.into(),
        }
    }
}

#[async_trait]
impl LeaderElector for PgLeaseElector {
    async fn try_acquire(&self, ttl: Duration) -> Result<Option<Lease>> {
        let mut conn = super::checkout(&self.pool, "lease").await?;
        let row = diesel::sql_query(
            "INSERT INTO worker_leases \
             (lease_name, holder_id, acquired_at, renewed_at, expires_at, fence_token) \
             VALUES ($1, $2, now(), now(), now() + make_interval(secs => $3), 0) \
             ON CONFLICT (lease_name) DO UPDATE SET \
                 holder_id = EXCLUDED.holder_id, \
                 acquired_at = CASE WHEN worker_leases.holder_id = EXCLUDED.holder_id \
                     THEN worker_leases.acquired_at ELSE now() END, \
                 renewed_at = now(), \
                 expires_at = now() + make_interval(secs => $3), \
                 fence_token = CASE WHEN worker_leases.holder_id = EXCLUDED.holder_id \
                     THEN worker_leases.fence_token ELSE worker_leases.fence_token + 1 END \
             WHERE worker_leases.expires_at < now() \
                OR worker_leases.holder_id = EXCLUDED.holder_id \
             RETURNING fence_token, expires_at",
        )
        .bind::<Text, _>(&self.lease_name)
        .bind::<Text, _>(&self.holder_id)
        .bind::<Double, _>(ttl.as_secs_f64())
        .get_result::<AcquireRow>(&mut conn)
        .await
        .optional()
        .map_err(|error| GuardianError::StorageError(format!("lease acquire: {error}")))?;

        Ok(row.map(|row| Lease {
            name: self.lease_name.clone(),
            holder_id: self.holder_id.clone(),
            fence_token: row.fence_token,
            expires_at: row.expires_at,
        }))
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<bool> {
        let mut conn = super::checkout(&self.pool, "lease").await?;
        let affected = diesel::sql_query(
            "UPDATE worker_leases SET renewed_at = now(), expires_at = now() + make_interval(secs => $1) \
             WHERE lease_name = $2 AND holder_id = $3 AND fence_token = $4 AND now() < expires_at",
        )
        .bind::<Double, _>(ttl.as_secs_f64())
        .bind::<Text, _>(&lease.name)
        .bind::<Text, _>(&lease.holder_id)
        .bind::<BigInt, _>(lease.fence_token)
        .execute(&mut conn)
        .await
        .map_err(|error| GuardianError::StorageError(format!("lease renew: {error}")))?;
        Ok(affected == 1)
    }

    async fn verify_held(&self, lease: &Lease) -> Result<bool> {
        let mut conn = super::checkout(&self.pool, "lease").await?;
        let row = diesel::sql_query(
            "SELECT 1 AS held FROM worker_leases \
             WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 AND now() < expires_at",
        )
        .bind::<Text, _>(&lease.name)
        .bind::<Text, _>(&lease.holder_id)
        .bind::<BigInt, _>(lease.fence_token)
        .get_result::<HeldRow>(&mut conn)
        .await
        .optional()
        .map_err(|error| GuardianError::StorageError(format!("lease verify: {error}")))?;
        Ok(row.is_some())
    }

    async fn release(&self, lease: Lease) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "lease").await?;
        // Expire the lease in place instead of deleting the row, so `fence_token`
        // survives and keeps advancing monotonically on the next steal. A DELETE
        // would let a fresh acquire re-INSERT `fence_token = 0`, after which a
        // stale `Lease { fence_token: 0 }` from a long-gone holder could pass
        // `verify_held` again.
        diesel::sql_query(
            "UPDATE worker_leases SET expires_at = now() \
             WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3",
        )
        .bind::<Text, _>(&lease.name)
        .bind::<Text, _>(&lease.holder_id)
        .bind::<BigInt, _>(lease.fence_token)
        .execute(&mut conn)
        .await
        .map_err(|error| GuardianError::StorageError(format!("lease release: {error}")))?;
        Ok(())
    }

    fn supports_fencing(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::pg::test_database_url;

    #[tokio::test]
    async fn try_acquire_fails_closed_when_unreachable() {
        let pool = build_postgres_pool_lazy("postgresql://127.0.0.1:1/__guardian_lease_fault__", 1)
            .expect("lazy pool builds even with an unreachable address");
        let elector = PgLeaseElector::new(pool, "canonicalization", "replica-a");
        assert!(
            elector.try_acquire(Duration::from_secs(30)).await.is_err(),
            "lease acquire must surface an error (not a false None) when unreachable",
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn single_owner_failover_fences_the_old_holder() {
        let url = test_database_url().await;
        let name = format!("canon-test-{}", Utc::now().timestamp_micros());
        let short_ttl = Duration::from_secs(1);
        let ttl = Duration::from_secs(60);
        let a = PgLeaseElector::new(
            build_postgres_pool_lazy(&url, 2).unwrap(),
            &name,
            "replica-a",
        );
        let b = PgLeaseElector::new(
            build_postgres_pool_lazy(&url, 2).unwrap(),
            &name,
            "replica-b",
        );

        let lease_a = a
            .try_acquire(short_ttl)
            .await
            .expect("acquire A")
            .expect("A becomes the single owner");
        // While A holds an unexpired lease, B cannot acquire.
        assert!(
            b.try_acquire(ttl).await.expect("B attempt").is_none(),
            "only one replica may hold the lease",
        );

        // A crashes (stops renewing); after the TTL elapses B steals the expired
        // lease, which advances the fence token (change of holder).
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let lease_b = b
            .try_acquire(ttl)
            .await
            .expect("acquire B")
            .expect("B takes over the expired lease");
        assert!(
            lease_b.fence_token > lease_a.fence_token,
            "a steal must advance the fence token",
        );

        // The superseded holder A can neither renew nor pass its fence check;
        // only the current holder B is verified.
        assert!(!a.renew(&lease_a, ttl).await.expect("A stale renew"));
        assert!(!a.verify_held(&lease_a).await.expect("A stale verify"));
        assert!(b.verify_held(&lease_b).await.expect("B verify"));

        b.release(lease_b).await.expect("cleanup");
    }
}
