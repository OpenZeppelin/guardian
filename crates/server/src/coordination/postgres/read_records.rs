use async_trait::async_trait;
use diesel::QueryableByName;
use diesel::sql_types::{BigInt, Bool, Bytea, Nullable, Text};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::builder::clock::Clock;
use crate::coordination::Realm;
use crate::coordination::read_records::{
    HeadRead, InactiveSession, PrefetchedHead, ReadRecordKey, ReadRecordOutcome, ReadRecordStore,
    SessionReadAdmission, SessionReadLookups, SessionReadRequest,
};
use crate::coordination::session_store::SessionSubject;
use crate::metrics::labels::PoolKind;
use crate::state_object::StateHead;
use crate::storage::postgres::TimedCheckout;

/// The fleet-wide record of accepted session reads, kept in the partitioned
/// `session_read_seen` table. Admission uses the database clock, so every
/// replica checks freshness against the same clock the purge uses.
pub struct PgReadRecords {
    pool: Pool<AsyncPgConnection>,
}

impl PgReadRecords {
    pub fn new(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }
}

#[derive(QueryableByName)]
struct Outcome {
    #[diesel(sql_type = Text)]
    outcome: String,
}

#[derive(QueryableByName)]
struct Admission {
    #[diesel(sql_type = Text)]
    outcome: String,
    #[diesel(sql_type = Nullable<BigInt>)]
    auth_version: Option<i64>,
    #[diesel(sql_type = Nullable<diesel::sql_types::Jsonb>)]
    subject: Option<serde_json::Value>,
    #[diesel(sql_type = Bool)]
    head_found: bool,
    #[diesel(sql_type = Nullable<Text>)]
    head_commitment: Option<String>,
    #[diesel(sql_type = Nullable<BigInt>)]
    head_nonce: Option<i64>,
}

impl Admission {
    fn head(&self, requested: HeadRead) -> Result<PrefetchedHead, String> {
        match (requested, self.outcome.as_str(), self.head_found) {
            (HeadRead::Include, "recorded", true) => Ok(PrefetchedHead::Found(StateHead {
                commitment: self
                    .head_commitment
                    .clone()
                    .ok_or("Admitted session read returned a head without a commitment")?,
                nonce: self.head_nonce.and_then(|nonce| u64::try_from(nonce).ok()),
            })),
            (HeadRead::Include, "recorded", false) => Ok(PrefetchedHead::Missing),
            _ => Ok(PrefetchedHead::NotRead),
        }
    }
}

fn record_outcome(outcome: &str) -> Result<ReadRecordOutcome, String> {
    match outcome {
        "recorded" => Ok(ReadRecordOutcome::Recorded),
        "duplicate" => Ok(ReadRecordOutcome::Duplicate),
        "outside_window" => Ok(ReadRecordOutcome::OutsideWindow),
        "stale" => Ok(ReadRecordOutcome::Stale),
        other => Err(format!("Unexpected session read outcome '{other}'")),
    }
}

#[derive(QueryableByName)]
struct Watermark {
    #[diesel(sql_type = BigInt)]
    watermark: i64,
}

#[async_trait]
impl ReadRecordStore for PgReadRecords {
    async fn admit_session_read(
        &self,
        _lookups: &dyn SessionReadLookups,
        request: SessionReadRequest<'_>,
    ) -> Result<SessionReadAdmission, String> {
        let mut conn = self
            .pool
            .get_timed(PoolKind::Metadata)
            .await
            .map_err(|e| format!("read record pool: {e}"))?;
        let row: Admission = diesel::sql_query(
            "SELECT outcome, auth_version, subject, head_found, head_commitment, head_nonce \
             FROM guardian_admit_session_read($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind::<Text, _>(request.account_id)
        .bind::<Text, _>(Realm::Miden.as_str())
        .bind::<Bytea, _>(request.session_key.to_vec())
        .bind::<Bytea, _>(request.record_key.to_vec())
        .bind::<BigInt, _>(request.timestamp_ms)
        .bind::<BigInt, _>(request.skew_ms)
        .bind::<Bool, _>(request.head == HeadRead::Include)
        .get_result(&mut conn)
        .await
        .map_err(|e| format!("Failed to admit session read: {e}"))?;
        let subject = |subject: Option<serde_json::Value>| -> Result<SessionSubject, String> {
            serde_json::from_value(
                subject.ok_or("Admitted session read without a session subject")?,
            )
            .map_err(|e| format!("session subject decode: {e}"))
        };
        Ok(match row.outcome.as_str() {
            "account_not_found" => SessionReadAdmission::AccountNotFound,
            "session_unknown" => SessionReadAdmission::Inactive(InactiveSession::Unknown),
            "session_revoked" => SessionReadAdmission::Inactive(InactiveSession::Revoked),
            "session_expired" => SessionReadAdmission::Inactive(InactiveSession::Expired),
            outcome => SessionReadAdmission::Live {
                metadata_version: row.auth_version,
                head: row.head(request.head)?,
                outcome: record_outcome(outcome)?,
                subject: subject(row.subject)?,
            },
        })
    }

    async fn record(
        &self,
        key: ReadRecordKey,
        timestamp_ms: i64,
        skew_ms: i64,
        _clock: &dyn Clock,
    ) -> Result<ReadRecordOutcome, String> {
        let mut conn = self
            .pool
            .get_timed(PoolKind::Metadata)
            .await
            .map_err(|e| format!("read record pool: {e}"))?;
        let row: Outcome =
            diesel::sql_query("SELECT guardian_record_session_read($1, $2, $3) AS outcome")
                .bind::<Bytea, _>(key.to_vec())
                .bind::<BigInt, _>(timestamp_ms)
                .bind::<BigInt, _>(skew_ms)
                .get_result(&mut conn)
                .await
                .map_err(|e| format!("Failed to record session read: {e}"))?;
        record_outcome(&row.outcome)
    }

    async fn maintain(&self, _now_ms: i64, retain_ms: i64, ahead_ms: i64) -> Result<i64, String> {
        let mut conn = self
            .pool
            .get_timed(PoolKind::Metadata)
            .await
            .map_err(|e| format!("read record pool: {e}"))?;
        diesel::sql_query("SELECT guardian_ensure_session_read_partitions($1, $2)")
            .bind::<BigInt, _>(retain_ms)
            .bind::<BigInt, _>(ahead_ms)
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to create session read partitions: {e}"))?;
        let row: Watermark =
            diesel::sql_query("SELECT guardian_advance_session_read_fence($1) AS watermark")
                .bind::<BigInt, _>(retain_ms)
                .get_result(&mut conn)
                .await
                .map_err(|e| format!("Failed to advance the session read fence: {e}"))?;
        diesel::sql_query("SELECT guardian_drop_fenced_session_read_partitions()")
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to drop fenced session read partitions: {e}"))?;
        Ok(row.watermark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::clock::test::MockClock;
    use crate::coordination::read_records::READ_RECORD_BUCKET_MS;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::pg::test_database_url;

    const SKEW: i64 = 300_000;

    async fn store() -> PgReadRecords {
        let url = test_database_url().await;
        PgReadRecords::new(build_postgres_pool_lazy(&url, 2).expect("pool"))
    }

    async fn db_now_ms(store: &PgReadRecords) -> i64 {
        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query("SELECT guardian_db_now_ms() AS watermark")
            .get_result::<Watermark>(&mut conn)
            .await
            .expect("db clock")
            .watermark
    }

    fn fresh_key() -> ReadRecordKey {
        rand::random()
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_read_is_admitted_once_across_replicas() {
        let (first, second) = (store().await, store().await);
        first.maintain(0, 600_000, 3_600_000).await.unwrap();
        let now = db_now_ms(&first).await;
        let key = fresh_key();

        assert_eq!(
            first
                .record(key, now, SKEW, &MockClock::default())
                .await
                .unwrap(),
            ReadRecordOutcome::Recorded
        );
        assert_eq!(
            second
                .record(key, now, SKEW, &MockClock::default())
                .await
                .unwrap(),
            ReadRecordOutcome::Duplicate,
            "the copy reaches another replica"
        );
        assert_eq!(
            second
                .record(fresh_key(), now, SKEW, &MockClock::default())
                .await
                .unwrap(),
            ReadRecordOutcome::Recorded,
            "another request with the same timestamp"
        );
        assert_eq!(
            first
                .record(fresh_key(), now - SKEW - 5_000, SKEW, &MockClock::default())
                .await
                .unwrap(),
            ReadRecordOutcome::OutsideWindow,
            "outside its window by the database clock"
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn the_fence_refuses_a_purged_read_and_a_missing_partition_fails_closed() {
        let store = store().await;
        store.maintain(0, 600_000, 3_600_000).await.unwrap();
        let now = db_now_ms(&store).await;
        let older = (now / READ_RECORD_BUCKET_MS) * READ_RECORD_BUCKET_MS - 1;
        let key = fresh_key();
        let any_window = i64::MAX / 4;

        assert_eq!(
            store
                .record(key, older, any_window, &MockClock::default())
                .await
                .unwrap(),
            ReadRecordOutcome::Recorded
        );
        let watermark = store.maintain(0, 0, 3_600_000).await.unwrap();
        assert!(older <= watermark);
        assert_eq!(
            store
                .record(key, older, any_window, &MockClock::default())
                .await
                .unwrap(),
            ReadRecordOutcome::Stale,
            "refused by the fence even with the window check relaxed"
        );

        let beyond_partitions = now + 3 * 3_600_000;
        assert!(
            store
                .record(
                    fresh_key(),
                    beyond_partitions,
                    any_window,
                    &MockClock::default()
                )
                .await
                .is_err(),
            "no partition: the read fails closed"
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_missing_fence_row_fails_closed() {
        let store = store().await;
        store.maintain(0, 600_000, 3_600_000).await.unwrap();
        let now = db_now_ms(&store).await;
        let mut conn = store.pool.get().await.expect("conn");
        let fence: Watermark = diesel::sql_query(
            "DELETE FROM replay_fence WHERE fence = 'session_read' RETURNING watermark_ms AS watermark",
        )
        .get_result(&mut conn)
        .await
        .unwrap();

        let outcome = store
            .record(fresh_key(), now, SKEW, &MockClock::default())
            .await;

        diesel::sql_query(format!(
            "INSERT INTO replay_fence (fence, watermark_ms) VALUES ('session_read', {})",
            fence.watermark
        ))
        .execute(&mut conn)
        .await
        .unwrap();
        assert!(
            outcome.is_err(),
            "no fence row: the read fails closed, got {outcome:?}"
        );
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::builder::clock::test::MockClock;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::pg::test_database_url;
    use async_trait::async_trait;

    struct NoLookups;

    #[async_trait]
    impl SessionReadLookups for NoLookups {
        async fn account_exists(&self, _account_id: &str) -> Result<bool, String> {
            unreachable!("Postgres admits session reads in one call")
        }
        async fn session(
            &self,
            _key: &crate::coordination::SessionKey,
        ) -> Result<crate::coordination::read_records::SessionStatus, String> {
            unreachable!("Postgres admits session reads in one call")
        }
    }

    async fn exec(store: &PgReadRecords, sql: &str) {
        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query(sql).execute(&mut conn).await.expect(sql);
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn one_call_reports_the_account_and_session_and_records_only_live_reads() {
        let url = test_database_url().await;
        let store = PgReadRecords::new(build_postgres_pool_lazy(&url, 2).expect("pool"));
        store.maintain(0, 600_000, 3_600_000).await.unwrap();
        let stamp = chrono::Utc::now().timestamp_micros();
        let account_id = format!("0xadmit{stamp}");
        exec(
            &store,
            &format!(
                "INSERT INTO account_metadata \
                 (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
                 VALUES ('{account_id}', '{{\"MidenFalconRpo\":{{\"cosigner_commitments\":[]}}}}', \
                 '{{\"kind\":\"miden\"}}', now(), now(), false)"
            ),
        )
        .await;
        let subject = serde_json::to_string(&SessionSubject::Miden {
            signer_commitment: "0x1".to_string(),
            origin: String::new(),
            guardian_commitment: "0x2".to_string(),
            network: "devnet".to_string(),
        })
        .unwrap();
        let session = |tag: u8| [tag; 32];
        let insert_session = |tag: u8, expires: &str, revoked: &str| {
            format!(
                "INSERT INTO auth_sessions (realm, token_digest, subject, issued_at, expires_at, revoked_at) \
                 VALUES ('miden', '\\x{}', '{subject}', now(), {expires}, {revoked}) \
                 ON CONFLICT DO NOTHING",
                hex::encode([tag; 32])
            )
        };
        exec(
            &store,
            &insert_session(0xa1, "now() + interval '1 hour'", "NULL"),
        )
        .await;
        exec(
            &store,
            &insert_session(0xa2, "now() + interval '1 hour'", "now()"),
        )
        .await;
        exec(
            &store,
            &insert_session(0xa3, "now() - interval '1 minute'", "NULL"),
        )
        .await;
        let now = {
            let mut conn = store.pool.get().await.expect("conn");
            diesel::sql_query("SELECT guardian_db_now_ms() AS watermark")
                .get_result::<Watermark>(&mut conn)
                .await
                .unwrap()
                .watermark
        };
        let admit_with = |account: String, key: [u8; 32], record: ReadRecordKey, head: HeadRead| {
            let store = &store;
            async move {
                store
                    .admit_session_read(
                        &NoLookups,
                        SessionReadRequest {
                            account_id: &account,
                            session_key: key,
                            record_key: record,
                            timestamp_ms: now,
                            skew_ms: 300_000,
                            clock: &MockClock::default(),
                            head,
                        },
                    )
                    .await
                    .unwrap()
            }
        };
        let admit = |account: String, key: [u8; 32], record: ReadRecordKey| {
            admit_with(account, key, record, HeadRead::Include)
        };

        let record: ReadRecordKey = rand::random();
        assert_eq!(
            admit(format!("{account_id}missing"), session(0xa1), record).await,
            SessionReadAdmission::AccountNotFound
        );
        for (key, status) in [
            (0xa9, InactiveSession::Unknown),
            (0xa2, InactiveSession::Revoked),
            (0xa3, InactiveSession::Expired),
        ] {
            assert_eq!(
                admit(account_id.clone(), session(key), record).await,
                SessionReadAdmission::Inactive(status)
            );
        }
        match admit(account_id.clone(), session(0xa1), record).await {
            SessionReadAdmission::Live {
                metadata_version,
                subject,
                outcome,
                head,
            } => {
                assert!(metadata_version.is_some_and(|version| version > 0));
                assert!(matches!(subject, SessionSubject::Miden { .. }));
                assert_eq!(
                    outcome,
                    ReadRecordOutcome::Recorded,
                    "nothing was recorded by the refused attempts"
                );
                assert_eq!(head, PrefetchedHead::Missing);
            }
            other => panic!("expected a live session, got {other:?}"),
        }
        assert!(matches!(
            admit(account_id.clone(), session(0xa1), record).await,
            SessionReadAdmission::Live {
                outcome: ReadRecordOutcome::Duplicate,
                head: PrefetchedHead::NotRead,
                ..
            }
        ));

        exec(
            &store,
            &format!(
                "INSERT INTO states (account_id, state_json, commitment, created_at, updated_at, nonce) \
                 VALUES ('{account_id}', '{{}}', '0xhead', now(), now(), 7)"
            ),
        )
        .await;
        assert!(matches!(
            admit(account_id.clone(), session(0xa1), rand::random()).await,
            SessionReadAdmission::Live {
                outcome: ReadRecordOutcome::Recorded,
                head: PrefetchedHead::Found(StateHead { ref commitment, nonce: Some(7) }),
                ..
            } if commitment == "0xhead"
        ));
        assert!(matches!(
            admit_with(
                account_id.clone(),
                session(0xa1),
                rand::random(),
                HeadRead::Skip
            )
            .await,
            SessionReadAdmission::Live {
                outcome: ReadRecordOutcome::Recorded,
                head: PrefetchedHead::NotRead,
                ..
            }
        ));
    }
}
