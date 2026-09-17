//! Postgres [`StatsStore`]: one `dashboard_stats_snapshots` row holds
//! the current published aggregate; `dashboard_stats_control` is the
//! singleton refresh-control row. Publication runs in one transaction
//! that locks the control row, validates the caller's `worker_leases`
//! row (the same predicate canonicalization writes use), and refuses
//! to overwrite a snapshot published under a newer fence token.
//!
//! When storage encryption is configured the payload is sealed with the
//! same cipher as `states.state_json` (AAD bound to the publication
//! version), so the snapshot — a copy of every account's vault totals —
//! never widens the at-rest boundary. The snapshot is derived data,
//! fully reconstructible from storage, so a stored row the cipher
//! cannot open — plaintext published before the key was configured, an
//! envelope under a retired key id, a payload restored under different
//! key material, or a corrupt one — is treated as absent and superseded
//! by the next publication instead of pinning the fleet to it.

use std::time::Duration;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::OptionalExtension;
use diesel::sql_types::{BigInt, Integer, Jsonb, Nullable, Text, Timestamptz};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};

use crate::coordination::stats_store::{
    PublishOutcome, PublishedStats, RefreshRequestOutcome, StatsControl, StatsStore, decide_request,
};
use crate::error::{GuardianError, Result};
use crate::storage::LeaseFence;
use crate::storage::encryption::cipher::{CipherError, StorageCipher};
use crate::storage::encryption::envelope::RecordAad;

pub struct PgStatsStore {
    pool: Pool<AsyncPgConnection>,
    cipher: Option<Arc<dyn StorageCipher>>,
}

impl PgStatsStore {
    pub(crate) fn new(
        pool: Pool<AsyncPgConnection>,
        cipher: Option<Arc<dyn StorageCipher>>,
    ) -> Self {
        Self { pool, cipher }
    }

    fn seal(&self, version: i64, payload: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.cipher {
            None => Ok(payload.clone()),
            Some(cipher) => cipher
                .encrypt(&RecordAad::DashboardStats { version }, payload)
                .map_err(|e| storage_err("encrypt snapshot payload", e)),
        }
    }

    /// `Ok(None)` when encryption is on and the stored payload cannot be
    /// opened, whatever the reason. The snapshot is derived data: a row
    /// the cipher refuses is never trusted or served, and treating it as
    /// absent lets the next walk (from an empty baseline) replace it.
    /// Returning an error here would instead block the baseline sync
    /// that precedes every walk, so no replica could ever publish over
    /// the bad row.
    fn open(&self, version: i64, stored: serde_json::Value) -> Result<Option<serde_json::Value>> {
        match &self.cipher {
            None => Ok(Some(stored)),
            Some(cipher) => match cipher.decrypt(&RecordAad::DashboardStats { version }, &stored) {
                Ok(plain) => Ok(Some(plain)),
                Err(CipherError::NotAnEnvelope) => {
                    tracing::warn!(
                        version,
                        "dashboard stats: stored snapshot is plaintext while storage encryption is on; ignoring it until the next publication"
                    );
                    Ok(None)
                }
                Err(error) => {
                    tracing::warn!(
                        version,
                        %error,
                        "dashboard stats: stored snapshot does not decrypt; ignoring it until the next publication"
                    );
                    Ok(None)
                }
            },
        }
    }
}

#[derive(diesel::QueryableByName)]
struct VersionRow {
    #[diesel(sql_type = BigInt)]
    version: i64,
}

#[derive(diesel::QueryableByName)]
struct SnapshotRow {
    #[diesel(sql_type = BigInt)]
    version: i64,
    #[diesel(sql_type = Timestamptz)]
    as_of: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    published_at: DateTime<Utc>,
    #[diesel(sql_type = Jsonb)]
    payload: serde_json::Value,
}

#[derive(diesel::QueryableByName)]
struct ControlRow {
    #[diesel(sql_type = Timestamptz)]
    now: DateTime<Utc>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    last_published_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    refresh_requested_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Text>)]
    refresh_requested_by: Option<String>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    refresh_started_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Text>)]
    refresh_started_by: Option<String>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    last_operator_request_at: Option<DateTime<Utc>>,
}

impl From<ControlRow> for StatsControl {
    fn from(row: ControlRow) -> Self {
        Self {
            now: row.now,
            last_published_at: row.last_published_at,
            refresh_requested_at: row.refresh_requested_at,
            refresh_requested_by: row.refresh_requested_by,
            refresh_started_at: row.refresh_started_at,
            refresh_started_by: row.refresh_started_by,
            last_operator_request_at: row.last_operator_request_at,
        }
    }
}

#[derive(diesel::QueryableByName)]
struct HeldRow {
    #[diesel(sql_type = Integer)]
    #[allow(dead_code)]
    held: i32,
}

#[derive(diesel::QueryableByName)]
struct PublishedRow {
    #[diesel(sql_type = BigInt)]
    version: i64,
    #[diesel(sql_type = Timestamptz)]
    published_at: DateTime<Utc>,
}

#[derive(diesel::QueryableByName)]
struct StartedRow {
    #[diesel(sql_type = Timestamptz)]
    #[allow(dead_code)]
    refresh_started_at: DateTime<Utc>,
}

const CONTROL_SELECT: &str = "SELECT now() AS now, last_published_at, refresh_requested_at, \
     refresh_requested_by, refresh_started_at, refresh_started_by, last_operator_request_at \
     FROM dashboard_stats_control WHERE id = TRUE";

fn storage_err(context: &str, error: impl std::fmt::Display) -> GuardianError {
    GuardianError::StorageError(format!("dashboard stats store: {context}: {error}"))
}

/// Error type of the publish transaction: a database error (rolled back
/// and reported as a storage error) or a sealing failure carried through
/// unchanged.
#[derive(Debug)]
enum PublishTxError {
    Db(diesel::result::Error),
    Seal(GuardianError),
}

impl From<diesel::result::Error> for PublishTxError {
    fn from(e: diesel::result::Error) -> Self {
        Self::Db(e)
    }
}

/// The lease predicate canonicalization writes use, evaluated inside the
/// caller's transaction so publication and fencing are one atomic step.
async fn lease_is_current(
    conn: &mut AsyncPgConnection,
    fence: &LeaseFence,
) -> std::result::Result<bool, diesel::result::Error> {
    let row = diesel::sql_query(
        "SELECT 1 AS held FROM worker_leases \
         WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 \
           AND clock_timestamp() < expires_at",
    )
    .bind::<Text, _>(&fence.lease_name)
    .bind::<Text, _>(&fence.holder_id)
    .bind::<BigInt, _>(fence.fence_token)
    .get_result::<HeldRow>(conn)
    .await
    .optional()?;
    Ok(row.is_some())
}

#[async_trait]
impl StatsStore for PgStatsStore {
    async fn current_version(&self) -> Result<Option<i64>> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        let row = diesel::sql_query(
            "SELECT version FROM dashboard_stats_snapshots ORDER BY version DESC LIMIT 1",
        )
        .get_result::<VersionRow>(&mut conn)
        .await
        .optional()
        .map_err(|e| storage_err("read version", e))?;
        Ok(row.map(|r| r.version))
    }

    async fn load_current(&self) -> Result<Option<PublishedStats>> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        let row = diesel::sql_query(
            "SELECT version, as_of, published_at, payload FROM dashboard_stats_snapshots \
             ORDER BY version DESC LIMIT 1",
        )
        .get_result::<SnapshotRow>(&mut conn)
        .await
        .optional()
        .map_err(|e| storage_err("load snapshot", e))?;
        let Some(r) = row else {
            return Ok(None);
        };
        Ok(self
            .open(r.version, r.payload)?
            .map(|payload| PublishedStats {
                version: r.version,
                as_of: r.as_of,
                published_at: r.published_at,
                payload,
            }))
    }

    async fn read_control(&self, _now: DateTime<Utc>) -> Result<StatsControl> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        let row = diesel::sql_query(CONTROL_SELECT)
            .get_result::<ControlRow>(&mut conn)
            .await
            .map_err(|e| storage_err("read control row", e))?;
        Ok(row.into())
    }

    async fn mark_refresh_started(&self, fence: &LeaseFence, _now: DateTime<Utc>) -> Result<bool> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        let row = diesel::sql_query(
            "UPDATE dashboard_stats_control SET refresh_started_at = now(), refresh_started_by = $1 \
             WHERE id = TRUE AND EXISTS ( \
                 SELECT 1 FROM worker_leases \
                 WHERE lease_name = $2 AND holder_id = $1 AND fence_token = $3 \
                   AND clock_timestamp() < expires_at) \
             RETURNING refresh_started_at",
        )
        .bind::<Text, _>(&fence.holder_id)
        .bind::<Text, _>(&fence.lease_name)
        .bind::<BigInt, _>(fence.fence_token)
        .get_result::<StartedRow>(&mut conn)
        .await
        .optional()
        .map_err(|e| storage_err("mark refresh started", e))?;
        Ok(row.is_some())
    }

    async fn clear_refresh_started(&self, fence: &LeaseFence) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        diesel::sql_query(
            "UPDATE dashboard_stats_control \
             SET refresh_started_at = NULL, refresh_started_by = NULL \
             WHERE id = TRUE AND refresh_started_by = $1",
        )
        .bind::<Text, _>(&fence.holder_id)
        .execute(&mut conn)
        .await
        .map_err(|e| storage_err("clear refresh started", e))?;
        Ok(())
    }

    async fn publish(
        &self,
        fence: &LeaseFence,
        as_of: DateTime<Utc>,
        _now: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> Result<PublishOutcome> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        let fence = fence.clone();
        let sealed_for = |version: i64| self.seal(version, &payload);
        conn.transaction::<_, PublishTxError, _>(|conn| {
            async move {
                // Serialize publishers on the control row.
                diesel::sql_query(
                    "SELECT id FROM dashboard_stats_control WHERE id = TRUE FOR UPDATE",
                )
                .execute(conn)
                .await?;
                if !lease_is_current(conn, &fence).await? {
                    return Ok(PublishOutcome::StaleLease);
                }
                let newer = diesel::sql_query(
                    "SELECT 1 AS held FROM dashboard_stats_snapshots WHERE fence_token > $1 LIMIT 1",
                )
                .bind::<BigInt, _>(fence.fence_token)
                .get_result::<HeldRow>(conn)
                .await
                .optional()?;
                if newer.is_some() {
                    return Ok(PublishOutcome::StaleLease);
                }
                // The version is assigned under the control-row lock and
                // bound into the envelope's AAD before the row is written.
                let next = diesel::sql_query(
                    "SELECT COALESCE(MAX(version), 0) + 1 AS version FROM dashboard_stats_snapshots",
                )
                .get_result::<VersionRow>(conn)
                .await?
                .version;
                let sealed = sealed_for(next).map_err(PublishTxError::Seal)?;
                let published = diesel::sql_query(
                    "INSERT INTO dashboard_stats_snapshots \
                         (version, fence_token, holder_id, as_of, published_at, payload) \
                     VALUES ($1, $2, $3, $4, now(), $5) \
                     RETURNING version, published_at",
                )
                .bind::<BigInt, _>(next)
                .bind::<BigInt, _>(fence.fence_token)
                .bind::<Text, _>(&fence.holder_id)
                .bind::<Timestamptz, _>(as_of)
                .bind::<Jsonb, _>(&sealed)
                .get_result::<PublishedRow>(conn)
                .await?;
                diesel::sql_query("DELETE FROM dashboard_stats_snapshots WHERE version < $1")
                    .bind::<BigInt, _>(published.version)
                    .execute(conn)
                    .await?;
                // Consume an operator request the published walk already
                // covered (requested before the walk started); all
                // expressions see the pre-update row values.
                diesel::sql_query(
                    "UPDATE dashboard_stats_control SET \
                         last_published_at = now(), \
                         refresh_requested_at = CASE \
                             WHEN refresh_requested_at IS NOT NULL AND refresh_started_at IS NOT NULL \
                                  AND refresh_requested_at <= refresh_started_at THEN NULL \
                             ELSE refresh_requested_at END, \
                         refresh_requested_by = CASE \
                             WHEN refresh_requested_at IS NOT NULL AND refresh_started_at IS NOT NULL \
                                  AND refresh_requested_at <= refresh_started_at THEN NULL \
                             ELSE refresh_requested_by END, \
                         refresh_started_at = NULL, \
                         refresh_started_by = NULL \
                     WHERE id = TRUE",
                )
                .execute(conn)
                .await?;
                Ok(PublishOutcome::Published {
                    version: published.version,
                    published_at: published.published_at,
                })
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| match e {
            PublishTxError::Db(e) => storage_err("publish", e),
            PublishTxError::Seal(e) => e,
        })
    }

    async fn request_refresh(
        &self,
        requested_by: &str,
        _now: DateTime<Utc>,
        cooldown: Duration,
        stale_after: Duration,
    ) -> Result<RefreshRequestOutcome> {
        let mut conn = super::checkout(&self.pool, "dashboard stats").await?;
        let requested_by = requested_by.to_string();
        conn.transaction::<_, diesel::result::Error, _>(|conn| {
            async move {
                let control: StatsControl =
                    diesel::sql_query(format!("{CONTROL_SELECT} FOR UPDATE"))
                        .get_result::<ControlRow>(conn)
                        .await?
                        .into();
                if let Some(refused) = decide_request(&control, cooldown, stale_after) {
                    return Ok(refused);
                }
                let requested_at = control.now;
                diesel::sql_query(
                    "UPDATE dashboard_stats_control SET \
                         refresh_requested_at = $1, refresh_requested_by = $2, \
                         last_operator_request_at = $1 \
                     WHERE id = TRUE",
                )
                .bind::<Timestamptz, _>(requested_at)
                .bind::<Text, _>(&requested_by)
                .execute(conn)
                .await?;
                Ok(RefreshRequestOutcome::Queued { requested_at })
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| storage_err("request refresh", e))
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use crate::coordination::LeaderElector;
    use crate::coordination::postgres::PgLeaseElector;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::pg::test_database_url;

    fn fence_of(lease: &crate::coordination::Lease) -> LeaseFence {
        LeaseFence {
            lease_name: lease.name.clone(),
            holder_id: lease.holder_id.clone(),
            fence_token: lease.fence_token,
        }
    }

    async fn reset(pool: &Pool<AsyncPgConnection>) {
        let mut conn = pool.get().await.unwrap();
        diesel::sql_query("DELETE FROM dashboard_stats_snapshots")
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::sql_query(
            "UPDATE dashboard_stats_control SET last_published_at = NULL, \
             refresh_requested_at = NULL, refresh_requested_by = NULL, \
             refresh_started_at = NULL, refresh_started_by = NULL, \
             last_operator_request_at = NULL WHERE id = TRUE",
        )
        .execute(&mut conn)
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn publish_is_fenced_by_the_lease_and_by_newer_fence_tokens() {
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 4).unwrap();
        reset(&pool).await;
        let store = PgStatsStore::new(pool.clone(), None);
        let lease_name = format!("stats-test-{}", Utc::now().timestamp_micros());
        let a = PgLeaseElector::new(pool.clone(), &lease_name, "replica-a");
        let b = PgLeaseElector::new(pool.clone(), &lease_name, "replica-b");
        let now = Utc::now();

        // Without holding the lease nothing can be published.
        let bogus = LeaseFence {
            lease_name: lease_name.clone(),
            holder_id: "nobody".into(),
            fence_token: 0,
        };
        assert_eq!(
            store
                .publish(&bogus, now, now, serde_json::json!({}))
                .await
                .unwrap(),
            PublishOutcome::StaleLease
        );
        assert_eq!(store.current_version().await.unwrap(), None);

        // A holds the lease and publishes version 1.
        let lease_a = a
            .try_acquire(Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap();
        let fa = fence_of(&lease_a);
        assert!(store.mark_refresh_started(&fa, now).await.unwrap());
        let first = store
            .publish(&fa, now, now, serde_json::json!({"who": "a"}))
            .await
            .unwrap();
        assert!(matches!(
            first,
            PublishOutcome::Published { version: 1, .. }
        ));
        let control = store.read_control(now).await.unwrap();
        assert!(control.last_published_at.is_some());
        assert_eq!(control.refresh_started_at, None);

        // A's lease expires; B steals it (fence token advances) and publishes.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let lease_b = b
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert!(lease_b.fence_token > lease_a.fence_token);
        let fb = fence_of(&lease_b);
        let second = store
            .publish(&fb, now, now, serde_json::json!({"who": "b"}))
            .await
            .unwrap();
        assert!(matches!(
            second,
            PublishOutcome::Published { version: 2, .. }
        ));

        // The superseded holder A finishes its walk late: refused on both
        // the lease predicate and the newer-token predicate, and the
        // published payload is still B's.
        assert_eq!(
            store
                .publish(&fa, now, now, serde_json::json!({"who": "a-late"}))
                .await
                .unwrap(),
            PublishOutcome::StaleLease
        );
        let current = store.load_current().await.unwrap().unwrap();
        assert_eq!(current.version, 2);
        assert_eq!(current.payload, serde_json::json!({"who": "b"}));
        // A stale holder cannot mark a walk in progress either.
        assert!(!store.mark_refresh_started(&fa, now).await.unwrap());

        b.release(lease_b).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn payload_is_sealed_with_the_storage_cipher_and_plaintext_rows_are_ignored() {
        use crate::storage::encryption::cipher::Aes256GcmCipher;
        use crate::storage::encryption::key_provider::{InMemoryKeyProvider, StorageKeyProvider};
        use base64::Engine as _;

        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 4).unwrap();
        reset(&pool).await;
        let key = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        let provider: Arc<dyn StorageKeyProvider> =
            Arc::new(InMemoryKeyProvider::from_dev_key(&key, "k1").unwrap());
        let cipher: Arc<dyn StorageCipher> = Arc::new(Aes256GcmCipher::new(provider));
        let store = PgStatsStore::new(pool.clone(), Some(cipher));
        let lease_name = format!("stats-enc-{}", Utc::now().timestamp_micros());
        let a = PgLeaseElector::new(pool.clone(), &lease_name, "replica-a");
        let lease = a
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let now = Utc::now();
        let secret = serde_json::json!({ "records": [{ "account_id": "0xacc", "vault": { "0xfaucet": 1000 } }] });

        store
            .publish(&fence_of(&lease), now, now, secret.clone())
            .await
            .unwrap();

        // At rest the row is an envelope, not the payload.
        #[derive(diesel::QueryableByName)]
        struct RawRow {
            #[diesel(sql_type = Jsonb)]
            payload: serde_json::Value,
        }
        let mut conn = pool.get().await.unwrap();
        let raw = diesel::sql_query("SELECT payload FROM dashboard_stats_snapshots")
            .get_result::<RawRow>(&mut conn)
            .await
            .unwrap();
        assert!(
            raw.payload.get("ct").is_some() && raw.payload.get("kid").is_some(),
            "{:?}",
            raw.payload
        );
        assert!(
            !raw.payload.to_string().contains("0xfaucet"),
            "vault data leaked in the clear"
        );

        // Through the store it round-trips, bound to the version.
        let loaded = store.load_current().await.unwrap().unwrap();
        assert_eq!(loaded.payload, secret);
        assert_eq!(loaded.version, 1);

        // A plaintext row (published before encryption was enabled) is
        // never served while encryption is on.
        diesel::sql_query("UPDATE dashboard_stats_snapshots SET payload = $1 WHERE version = 1")
            .bind::<Jsonb, _>(&secret)
            .execute(&mut conn)
            .await
            .unwrap();
        assert_eq!(store.current_version().await.unwrap(), Some(1));
        assert_eq!(store.load_current().await.unwrap(), None);

        // An envelope under a different version fails authentication
        // (AAD mismatch): ignored, never served, so the next walk can
        // replace it instead of every replica failing on it.
        let sealed_v2 = store.seal(2, &secret).unwrap();
        diesel::sql_query("UPDATE dashboard_stats_snapshots SET payload = $1 WHERE version = 1")
            .bind::<Jsonb, _>(&sealed_v2)
            .execute(&mut conn)
            .await
            .unwrap();
        assert_eq!(store.load_current().await.unwrap(), None);

        // A valid envelope under a key id this deployment no longer
        // holds (retired kid, or a restore under different key material)
        // is likewise ignored ...
        let sealed_v1 = store.seal(1, &secret).unwrap();
        diesel::sql_query("UPDATE dashboard_stats_snapshots SET payload = $1 WHERE version = 1")
            .bind::<Jsonb, _>(&sealed_v1)
            .execute(&mut conn)
            .await
            .unwrap();
        let other_key: Arc<dyn StorageKeyProvider> = Arc::new(
            InMemoryKeyProvider::from_dev_key(
                &base64::engine::general_purpose::STANDARD.encode([7u8; 32]),
                "k2",
            )
            .unwrap(),
        );
        let other_store = PgStatsStore::new(
            pool.clone(),
            Some(Arc::new(Aes256GcmCipher::new(other_key))),
        );
        assert_eq!(other_store.current_version().await.unwrap(), Some(1));
        assert_eq!(other_store.load_current().await.unwrap(), None);
        // ... and a publish through the store that owns the current key
        // replaces it, so the fleet recovers without manual cleanup.
        let published = other_store
            .publish(
                &fence_of(&lease),
                now,
                now,
                serde_json::json!({ "fresh": true }),
            )
            .await
            .unwrap();
        assert!(matches!(
            published,
            PublishOutcome::Published { version: 2, .. }
        ));
        assert_eq!(
            other_store.load_current().await.unwrap().unwrap().payload,
            serde_json::json!({ "fresh": true })
        );

        a.release(lease).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn request_refresh_is_deduplicated_and_consumed_by_publication() {
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 8).unwrap();
        reset(&pool).await;
        let store = std::sync::Arc::new(PgStatsStore::new(pool.clone(), None));
        let lease_name = format!("stats-req-{}", Utc::now().timestamp_micros());
        let a = PgLeaseElector::new(pool.clone(), &lease_name, "replica-a");
        let cooldown = Duration::from_secs(60);
        let stale = Duration::from_secs(600);
        let now = Utc::now();

        // Concurrent operator requests: exactly one is queued, the rest
        // see it as already queued.
        let mut handles = Vec::new();
        for i in 0..6 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .request_refresh(&format!("op-{i}"), Utc::now(), cooldown, stale)
                    .await
                    .unwrap()
            }));
        }
        let mut queued = 0;
        let mut already = 0;
        for h in handles {
            match h.await.unwrap() {
                RefreshRequestOutcome::Queued { .. } => queued += 1,
                RefreshRequestOutcome::AlreadyQueued { .. } => already += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!((queued, already), (1, 5));

        // Leader starts a walk after the request: callers see in-progress.
        let lease = a
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let fence = fence_of(&lease);
        assert!(store.mark_refresh_started(&fence, now).await.unwrap());
        assert!(matches!(
            store
                .request_refresh("op-x", now, cooldown, stale)
                .await
                .unwrap(),
            RefreshRequestOutcome::InProgress { .. }
        ));

        // Publication consumes the request; the cooldown then applies.
        store
            .publish(&fence, now, now, serde_json::json!({}))
            .await
            .unwrap();
        let control = store.read_control(now).await.unwrap();
        assert_eq!(control.refresh_requested_at, None);
        assert_eq!(control.refresh_started_at, None);
        assert!(matches!(
            store
                .request_refresh("op-y", now, cooldown, stale)
                .await
                .unwrap(),
            RefreshRequestOutcome::Cooldown { .. }
        ));

        // A failed walk clears its own marker so requests are not
        // reported as in progress forever.
        assert!(store.mark_refresh_started(&fence, now).await.unwrap());
        store.clear_refresh_started(&fence).await.unwrap();
        assert_eq!(
            store.read_control(now).await.unwrap().refresh_started_at,
            None
        );

        a.release(lease).await.unwrap();
    }
}
