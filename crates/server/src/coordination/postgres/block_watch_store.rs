//! Postgres [`BlockWatchStore`]: `block_watch_cursor` (one row per network)
//! and `block_watch_observations` (the feed's worklist), shared by every
//! replica. The cursor is written in one transaction that holds the
//! caller's `worker_leases` row (`FOR SHARE`, so a steal waits for the
//! write) and moves the cursor forward only. Observations need no fence: a
//! visit re-reads everything it acts on, so an observation recorded by a
//! superseded holder is at worst one redundant visit.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::OptionalExtension;
use diesel::sql_types::{Array, BigInt, Integer, Text, Timestamptz};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};

use crate::coordination::block_watch_store::{
    BlockWatchStore, Cursor, CursorWrite, FencedWrite, NewObservation, Observation, WalkRequests,
};
use crate::error::{GuardianError, Result};
use crate::network::NetworkType;
use crate::storage::LeaseFence;

pub struct PgBlockWatchStore {
    pool: Pool<AsyncPgConnection>,
}

impl PgBlockWatchStore {
    pub(crate) fn new(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }

    /// One fenced cursor write: the caller's lease checked, and its row
    /// held, in the same transaction as the upsert.
    async fn write_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
        movement: CursorMove,
    ) -> Result<CursorWrite> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        let fence = fence.clone();
        let network = network.to_string();
        let genesis = genesis.map(str::to_string);
        // A reset replaces the recorded chain and may move back; an advance
        // moves forward only and keeps the chain unless the caller names one.
        let (genesis_update, condition) = match movement {
            CursorMove::Forward => (
                "COALESCE(EXCLUDED.genesis_commitment, block_watch_cursor.genesis_commitment)",
                " WHERE block_watch_cursor.last_block < EXCLUDED.last_block",
            ),
            CursorMove::Reset => ("EXCLUDED.genesis_commitment", ""),
        };
        let upsert = format!(
            "INSERT INTO block_watch_cursor \
             (network, last_block, genesis_commitment, updated_at, holder_id, fence_token) \
             VALUES ($1, $2, $3, now(), $4, $5) \
             ON CONFLICT (network) DO UPDATE SET \
                 last_block = EXCLUDED.last_block, \
                 genesis_commitment = {genesis_update}, \
                 updated_at = now(), \
                 holder_id = EXCLUDED.holder_id, \
                 fence_token = EXCLUDED.fence_token{condition}"
        );
        let outcome = conn
            .transaction::<_, CursorTxError, _>(|conn| {
                async move {
                    hold_lease_row(conn, &fence).await?;
                    let written = diesel::sql_query(upsert)
                        .bind::<Text, _>(&network)
                        .bind::<BigInt, _>(i64::from(block))
                        .bind::<diesel::sql_types::Nullable<Text>, _>(genesis.as_deref())
                        .bind::<Text, _>(&fence.holder_id)
                        .bind::<BigInt, _>(fence.fence_token)
                        .execute(conn)
                        .await?;
                    Ok(if written == 0 {
                        CursorWrite::NotForward
                    } else {
                        CursorWrite::Advanced
                    })
                }
                .scope_boxed()
            })
            .await;
        match outcome {
            Ok(write) => Ok(write),
            Err(CursorTxError::Stale) => Ok(CursorWrite::StaleLease),
            Err(CursorTxError::Db(e)) => Err(storage_err("write cursor", e)),
        }
    }
}

/// Check the caller's lease and hold its row until the transaction
/// commits, so the lease cannot change hands between the check and the
/// write: the lease predicate canonicalization writes use.
async fn hold_lease_row(
    conn: &mut AsyncPgConnection,
    fence: &LeaseFence,
) -> std::result::Result<(), CursorTxError> {
    let held = diesel::sql_query(
        "SELECT 1 AS held FROM worker_leases \
         WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 \
           AND clock_timestamp() < expires_at \
         FOR SHARE",
    )
    .bind::<Text, _>(&fence.lease_name)
    .bind::<Text, _>(&fence.holder_id)
    .bind::<BigInt, _>(fence.fence_token)
    .get_result::<HeldRow>(conn)
    .await
    .optional()?;
    match held {
        Some(_) => Ok(()),
        None => Err(CursorTxError::Stale),
    }
}

impl PgBlockWatchStore {
    /// Run `statement` (binding the network as `$1`, then `value` as `$2`,
    /// then holder and fence token as `$3`/`$4` when it uses them) in a
    /// transaction fenced by `fence`.
    async fn fenced_walk_write(
        &self,
        statement: &'static str,
        network: NetworkType,
        value: WalkValue,
        fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        let fence = fence.clone();
        let network = network.to_string();
        let outcome = conn
            .transaction::<_, CursorTxError, _>(|conn| {
                async move {
                    hold_lease_row(conn, &fence).await?;
                    let query = diesel::sql_query(statement).bind::<Text, _>(&network);
                    match value {
                        WalkValue::RequestedAt(now) => {
                            query
                                .bind::<Timestamptz, _>(now)
                                .bind::<Text, _>(&fence.holder_id)
                                .bind::<BigInt, _>(fence.fence_token)
                                .execute(conn)
                                .await?
                        }
                        WalkValue::StartedUnder(seq) => {
                            query.bind::<BigInt, _>(seq).execute(conn).await?
                        }
                    };
                    Ok(())
                }
                .scope_boxed()
            })
            .await;
        match outcome {
            Ok(()) => Ok(FencedWrite::Written),
            Err(CursorTxError::Stale) => Ok(FencedWrite::StaleLease),
            Err(CursorTxError::Db(e)) => Err(storage_err("write walk request", e)),
        }
    }
}

/// The value a fenced walk write binds after the network.
#[derive(Clone, Copy)]
enum WalkValue {
    RequestedAt(DateTime<Utc>),
    StartedUnder(i64),
}

/// How a fenced cursor write treats the stored cursor.
#[derive(Clone, Copy)]
enum CursorMove {
    /// Only to a later block (the normal advance).
    Forward,
    /// To any block (a reset chain).
    Reset,
}

fn storage_err(context: &str, error: impl std::fmt::Display) -> GuardianError {
    GuardianError::StorageError(format!("block watch {context}: {error}"))
}

/// Block numbers are `u32` on chain and `BIGINT` here; a stored value
/// outside `u32` was not written by this code.
fn block_from_column(value: i64) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| storage_err("decode", format!("block number {value} out of range")))
}

#[derive(diesel::QueryableByName)]
struct CursorRow {
    #[diesel(sql_type = BigInt)]
    last_block: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    genesis_commitment: Option<String>,
}

#[derive(diesel::QueryableByName)]
struct WalkRow {
    #[diesel(sql_type = BigInt)]
    requested_seq: i64,
    #[diesel(sql_type = BigInt)]
    completed_seq: i64,
}

#[derive(diesel::QueryableByName)]
struct HeldRow {
    #[diesel(sql_type = Integer)]
    #[allow(dead_code)]
    held: i32,
}

#[derive(diesel::QueryableByName)]
struct ObservationRow {
    #[diesel(sql_type = Text)]
    account_id: String,
    #[diesel(sql_type = BigInt)]
    block_num: i64,
    #[diesel(sql_type = Text)]
    on_chain_commitment: String,
    #[diesel(sql_type = Integer)]
    attempts: i32,
}

#[derive(diesel::QueryableByName)]
struct DueAtRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<Timestamptz>)]
    due_at: Option<DateTime<Utc>>,
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

/// Why a fenced cursor transaction did not write.
enum CursorTxError {
    Stale,
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for CursorTxError {
    fn from(e: diesel::result::Error) -> Self {
        Self::Db(e)
    }
}

#[async_trait]
impl BlockWatchStore for PgBlockWatchStore {
    async fn cursor(&self, network: NetworkType) -> Result<Option<Cursor>> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        let row = diesel::sql_query(
            "SELECT last_block, genesis_commitment FROM block_watch_cursor WHERE network = $1",
        )
        .bind::<Text, _>(network.to_string())
        .get_result::<CursorRow>(&mut conn)
        .await
        .optional()
        .map_err(|e| storage_err("read cursor", e))?;
        row.map(|row| {
            Ok(Cursor {
                last_block: block_from_column(row.last_block)?,
                genesis: row.genesis_commitment,
            })
        })
        .transpose()
    }

    async fn set_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
    ) -> Result<CursorWrite> {
        self.write_cursor(network, block, genesis, fence, CursorMove::Forward)
            .await
    }

    async fn reset_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
    ) -> Result<CursorWrite> {
        self.write_cursor(network, block, genesis, fence, CursorMove::Reset)
            .await
    }

    async fn record_observations(
        &self,
        observations: &[NewObservation],
        now: DateTime<Utc>,
    ) -> Result<()> {
        if observations.is_empty() {
            return Ok(());
        }
        let account_ids: Vec<String> = observations.iter().map(|o| o.account_id.clone()).collect();
        let blocks: Vec<i64> = observations
            .iter()
            .map(|o| i64::from(o.block_num))
            .collect();
        let commitments: Vec<String> = observations
            .iter()
            .map(|o| o.on_chain_commitment.clone())
            .collect();
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        // One statement per block, whatever the number of observations.
        diesel::sql_query(
            "INSERT INTO block_watch_observations \
             (account_id, block_num, on_chain_commitment, observed_at, attempts, next_attempt_at) \
             SELECT account_id, block_num, on_chain_commitment, $4, 0, $4 \
             FROM unnest($1::text[], $2::bigint[], $3::text[]) \
                 AS observed(account_id, block_num, on_chain_commitment) \
             ON CONFLICT (account_id, block_num) DO NOTHING",
        )
        .bind::<Array<Text>, _>(account_ids)
        .bind::<Array<BigInt>, _>(blocks)
        .bind::<Array<Text>, _>(commitments)
        .bind::<Timestamptz, _>(now)
        .execute(&mut conn)
        .await
        .map_err(|e| storage_err("record observations", e))?;
        Ok(())
    }

    async fn next_due_observation(&self, now: DateTime<Utc>) -> Result<Option<Observation>> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        // The order of `block_watch_observations_due_idx`, so the first row
        // is read off the index rather than sorted out of every due one.
        // Account ids compare bytewise (`COLLATE "C"`), as the in-memory
        // store compares them.
        let row = diesel::sql_query(
            "SELECT account_id, block_num, on_chain_commitment, attempts \
             FROM block_watch_observations \
             WHERE next_attempt_at <= $1 \
             ORDER BY next_attempt_at, block_num, account_id COLLATE \"C\" \
             LIMIT 1",
        )
        .bind::<Timestamptz, _>(now)
        .get_result::<ObservationRow>(&mut conn)
        .await
        .optional()
        .map_err(|e| storage_err("read due observation", e))?;
        row.map(|row| {
            Ok(Observation {
                account_id: row.account_id,
                block_num: block_from_column(row.block_num)?,
                on_chain_commitment: row.on_chain_commitment,
                attempts: u32::try_from(row.attempts).unwrap_or(0),
            })
        })
        .transpose()
    }

    async fn next_observation_due_at(&self) -> Result<Option<DateTime<Utc>>> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        let row = diesel::sql_query(
            "SELECT min(next_attempt_at) AS due_at FROM block_watch_observations",
        )
        .get_result::<DueAtRow>(&mut conn)
        .await
        .map_err(|e| storage_err("read next due time", e))?;
        Ok(row.due_at)
    }

    async fn delete_observation(&self, account_id: &str, block_num: u32) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        diesel::sql_query(
            "DELETE FROM block_watch_observations WHERE account_id = $1 AND block_num = $2",
        )
        .bind::<Text, _>(account_id)
        .bind::<BigInt, _>(i64::from(block_num))
        .execute(&mut conn)
        .await
        .map_err(|e| storage_err("delete observation", e))?;
        Ok(())
    }

    async fn defer_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        diesel::sql_query(
            "UPDATE block_watch_observations \
             SET attempts = attempts + 1, next_attempt_at = $3 \
             WHERE account_id = $1 AND block_num = $2",
        )
        .bind::<Text, _>(account_id)
        .bind::<BigInt, _>(i64::from(block_num))
        .bind::<Timestamptz, _>(next_attempt_at)
        .execute(&mut conn)
        .await
        .map_err(|e| storage_err("defer observation", e))?;
        Ok(())
    }

    async fn postpone_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        diesel::sql_query(
            "UPDATE block_watch_observations SET next_attempt_at = $3 \
             WHERE account_id = $1 AND block_num = $2",
        )
        .bind::<Text, _>(account_id)
        .bind::<BigInt, _>(i64::from(block_num))
        .bind::<Timestamptz, _>(next_attempt_at)
        .execute(&mut conn)
        .await
        .map_err(|e| storage_err("postpone observation", e))?;
        Ok(())
    }

    async fn count_pending_observations(&self) -> Result<u64> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        let row = diesel::sql_query("SELECT count(*) AS count FROM block_watch_observations")
            .get_result::<CountRow>(&mut conn)
            .await
            .map_err(|e| storage_err("count observations", e))?;
        Ok(u64::try_from(row.count).unwrap_or(0))
    }

    async fn walk_requests(&self, network: NetworkType) -> Result<Option<WalkRequests>> {
        let mut conn = super::checkout(&self.pool, "block watch").await?;
        let row = diesel::sql_query(
            "SELECT requested_seq, completed_seq FROM block_watch_walks WHERE network = $1",
        )
        .bind::<Text, _>(network.to_string())
        .get_result::<WalkRow>(&mut conn)
        .await
        .optional()
        .map_err(|e| storage_err("read walk requests", e))?;
        Ok(row.map(|row| WalkRequests {
            requested_seq: row.requested_seq,
            completed_seq: row.completed_seq,
        }))
    }

    async fn request_walk(
        &self,
        network: NetworkType,
        now: DateTime<Utc>,
        fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        self.fenced_walk_write(
            "INSERT INTO block_watch_walks \
             (network, requested_seq, completed_seq, requested_at, holder_id, fence_token) \
             VALUES ($1, 1, 0, $2, $3, $4) \
             ON CONFLICT (network) DO UPDATE SET \
                 requested_seq = block_watch_walks.requested_seq + 1, \
                 requested_at = EXCLUDED.requested_at, \
                 holder_id = EXCLUDED.holder_id, \
                 fence_token = EXCLUDED.fence_token",
            network,
            WalkValue::RequestedAt(now),
            fence,
        )
        .await
    }

    async fn complete_walk(
        &self,
        network: NetworkType,
        started_under: i64,
        fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        self.fenced_walk_write(
            "UPDATE block_watch_walks \
             SET completed_seq = GREATEST(completed_seq, $2) \
             WHERE network = $1",
            network,
            WalkValue::StartedUnder(started_under),
            fence,
        )
        .await
    }
}

#[cfg(test)]
mod postgres_tests {
    use std::time::Duration;

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
        for table in [
            "block_watch_cursor",
            "block_watch_observations",
            "block_watch_walks",
        ] {
            diesel::sql_query(format!("DELETE FROM {table}"))
                .execute(&mut conn)
                .await
                .unwrap();
        }
    }

    fn observed(account_id: &str, block_num: u32, commitment: &str) -> NewObservation {
        NewObservation {
            account_id: account_id.to_string(),
            block_num,
            on_chain_commitment: commitment.to_string(),
        }
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn owed_walks_are_fenced_counted_and_cleared_only_up_to_a_walks_start() {
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 4).unwrap();
        reset(&pool).await;
        let store = PgBlockWatchStore::new(pool.clone());
        let lease_name = format!("block-watch-walks-{}", Utc::now().timestamp_micros());
        let a = PgLeaseElector::new(pool.clone(), &lease_name, "replica-a");
        let b = PgLeaseElector::new(pool.clone(), &lease_name, "replica-b");
        let network = NetworkType::MidenDevnet;
        let now = Utc::now();
        let bogus = LeaseFence {
            lease_name: lease_name.clone(),
            holder_id: "nobody".into(),
            fence_token: 0,
        };
        assert_eq!(
            store.request_walk(network, now, &bogus).await.unwrap(),
            FencedWrite::StaleLease,
            "a request needs the lease"
        );
        assert_eq!(store.walk_requests(network).await.unwrap(), None);

        let lease_a = a
            .try_acquire(Duration::from_secs(1))
            .await
            .unwrap()
            .expect("A acquires");
        let fence_a = fence_of(&lease_a);
        store.request_walk(network, now, &fence_a).await.unwrap();
        let walks = store.walk_requests(network).await.unwrap().unwrap();
        assert_eq!((walks.requested_seq, walks.completed_seq), (1, 0));
        assert!(walks.owed());

        // A starts a walk under 1, a second request lands mid-walk, then A
        // loses the lease before the walk completes.
        store.request_walk(network, now, &fence_a).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let lease_b = b
            .try_acquire(Duration::from_secs(5))
            .await
            .unwrap()
            .expect("B takes over");
        let fence_b = fence_of(&lease_b);
        assert_eq!(
            store.complete_walk(network, 1, &fence_a).await.unwrap(),
            FencedWrite::StaleLease,
            "a superseded holder cannot clear a walk"
        );
        let walks = store.walk_requests(network).await.unwrap().unwrap();
        assert_eq!((walks.requested_seq, walks.completed_seq), (2, 0));

        // B's walk started under 1 (before the second request): it covers
        // only the first, so one walk is still owed.
        store.complete_walk(network, 1, &fence_b).await.unwrap();
        let walks = store.walk_requests(network).await.unwrap().unwrap();
        assert_eq!((walks.requested_seq, walks.completed_seq), (2, 1));
        assert!(walks.owed());
        store.complete_walk(network, 2, &fence_b).await.unwrap();
        // A late completion of an older walk never moves it back.
        store.complete_walk(network, 1, &fence_b).await.unwrap();
        let walks = store.walk_requests(network).await.unwrap().unwrap();
        assert_eq!((walks.requested_seq, walks.completed_seq), (2, 2));
        assert!(!walks.owed());
        assert_eq!(
            store
                .walk_requests(NetworkType::MidenTestnet)
                .await
                .unwrap(),
            None,
            "per network"
        );
        b.release(lease_b).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn the_cursor_is_fenced_by_the_lease_and_only_moves_forward() {
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 4).unwrap();
        reset(&pool).await;
        let store = PgBlockWatchStore::new(pool.clone());
        let lease_name = format!("block-watch-test-{}", Utc::now().timestamp_micros());
        let a = PgLeaseElector::new(pool.clone(), &lease_name, "replica-a");
        let b = PgLeaseElector::new(pool.clone(), &lease_name, "replica-b");
        let network = NetworkType::MidenDevnet;

        // Without holding the lease nothing is written.
        let bogus = LeaseFence {
            lease_name: lease_name.clone(),
            holder_id: "nobody".into(),
            fence_token: 0,
        };
        assert_eq!(
            store.set_cursor(network, 5, None, &bogus).await.unwrap(),
            CursorWrite::StaleLease
        );
        assert_eq!(store.cursor(network).await.unwrap(), None);

        let lease_a = a
            .try_acquire(Duration::from_secs(1))
            .await
            .unwrap()
            .expect("A acquires");
        let fence_a = fence_of(&lease_a);
        assert_eq!(
            store
                .set_cursor(network, 10, Some("0xgenesis"), &fence_a)
                .await
                .unwrap(),
            CursorWrite::Advanced
        );
        for stale in [10, 9] {
            assert_eq!(
                store
                    .set_cursor(network, stale, None, &fence_a)
                    .await
                    .unwrap(),
                CursorWrite::NotForward,
                "a cursor never moves backwards"
            );
        }
        assert_eq!(
            store.cursor(network).await.unwrap(),
            Some(Cursor {
                last_block: 10,
                genesis: Some("0xgenesis".into())
            })
        );
        assert_eq!(store.cursor(NetworkType::MidenTestnet).await.unwrap(), None);

        // A stops renewing; B takes over and continues from A's cursor.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let lease_b = b
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .expect("B takes over");
        assert_eq!(
            store.set_cursor(network, 11, None, &fence_a).await.unwrap(),
            CursorWrite::StaleLease,
            "the superseded holder can no longer move the cursor"
        );
        assert_eq!(
            store
                .set_cursor(network, 11, None, &fence_of(&lease_b))
                .await
                .unwrap(),
            CursorWrite::Advanced
        );
        assert_eq!(
            store.cursor(network).await.unwrap(),
            Some(Cursor {
                last_block: 11,
                genesis: Some("0xgenesis".into())
            }),
            "an advance that names no chain keeps the recorded one"
        );
        // A reset chain: the cursor moves back, onto the new chain, and is
        // fenced like any write.
        assert_eq!(
            store
                .reset_cursor(network, 2, Some("0xnew"), &fence_a)
                .await
                .unwrap(),
            CursorWrite::StaleLease
        );
        assert_eq!(
            store
                .reset_cursor(network, 2, Some("0xnew"), &fence_of(&lease_b))
                .await
                .unwrap(),
            CursorWrite::Advanced
        );
        assert_eq!(
            store.cursor(network).await.unwrap(),
            Some(Cursor {
                last_block: 2,
                genesis: Some("0xnew".into())
            })
        );
        b.release(lease_b).await.unwrap();
        reset(&pool).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn observations_are_idempotent_ordered_and_deferred() {
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 4).unwrap();
        reset(&pool).await;
        let store = PgBlockWatchStore::new(pool.clone());
        // Postgres keeps microseconds.
        let now = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();

        store.record_observations(&[], now).await.unwrap();
        store
            .record_observations(
                &[
                    observed("0xb", 12, "0xb12"),
                    observed("0xa", 12, "0xa12"),
                    observed("0xc", 11, "0xc11"),
                ],
                now,
            )
            .await
            .unwrap();
        // A replayed block changes nothing.
        store
            .record_observations(&[observed("0xa", 12, "0xother")], now)
            .await
            .unwrap();
        assert_eq!(store.count_pending_observations().await.unwrap(), 3);

        let first = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!(
            first,
            Observation {
                account_id: "0xc".into(),
                block_num: 11,
                on_chain_commitment: "0xc11".into(),
                attempts: 0,
            }
        );
        store.delete_observation("0xc", 11).await.unwrap();
        let later = now + chrono::Duration::seconds(60);
        store.defer_observation("0xa", 12, later).await.unwrap();
        let due = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!(
            (due.account_id.as_str(), due.on_chain_commitment.as_str()),
            ("0xb", "0xb12")
        );
        store.delete_observation("0xb", 12).await.unwrap();
        assert_eq!(store.next_due_observation(now).await.unwrap(), None);
        assert_eq!(store.next_observation_due_at().await.unwrap(), Some(later));
        let retried = store.next_due_observation(later).await.unwrap().unwrap();
        assert_eq!((retried.account_id.as_str(), retried.attempts), ("0xa", 1));
        store.delete_observation("0xa", 12).await.unwrap();
        assert_eq!(store.count_pending_observations().await.unwrap(), 0);
        assert_eq!(store.next_observation_due_at().await.unwrap(), None);
        reset(&pool).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn the_observation_due_longest_comes_first_whatever_its_block() {
        // An observation that keeps coming back (one that waits) is due
        // again after each visit; one recorded since is due from then.
        // Serving the oldest due first keeps the returning ones from
        // holding a newer one back (PR #586 review), like the in-memory
        // store, account ids compared bytewise.
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 4).unwrap();
        reset(&pool).await;
        let store = PgBlockWatchStore::new(pool.clone());
        let now = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
        let later = |seconds| now + chrono::Duration::seconds(seconds);

        store
            .record_observations(&[observed("0xwaiting", 5, "0xw5")], now)
            .await
            .unwrap();
        store
            .postpone_observation("0xwaiting", 5, later(60))
            .await
            .unwrap();
        store
            .record_observations(
                &[observed("0xb", 9, "0xb9"), observed("0xB", 9, "0xB9")],
                later(30),
            )
            .await
            .unwrap();

        let order: Vec<(String, u32)> = {
            let mut order = Vec::new();
            while let Some(due) = store.next_due_observation(later(60)).await.unwrap() {
                store
                    .delete_observation(&due.account_id, due.block_num)
                    .await
                    .unwrap();
                order.push((due.account_id, due.block_num));
            }
            order
        };
        assert_eq!(
            order,
            vec![
                ("0xB".to_string(), 9),
                ("0xb".to_string(), 9),
                ("0xwaiting".to_string(), 5),
            ]
        );
        reset(&pool).await;
    }
}
