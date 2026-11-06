use async_trait::async_trait;
use diesel::QueryableByName;
use diesel::sql_types::{BigInt, Bool, Bytea, Jsonb, Nullable, Record, Text};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::builder::clock::Clock;
use crate::coordination::Realm;
use crate::coordination::read_records::{
    HeadRead, InactiveSession, PrefetchedHead, ReadRecordKey, ReadRecordOutcome, ReadRecordStore,
    ReadRule, SessionReadAdmission, SessionReadLookups, SessionReadRequest,
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

diesel::define_sql_function! {
    /// Typed so the statement is prepared once per connection and reused;
    /// a raw `sql_query` is prepared again on every call, which costs a
    /// round trip and a transaction of its own.
    fn guardian_admit_session_read(
        account_id: Text,
        realm: Text,
        token_digest: Bytea,
        record_key: Bytea,
        ts: BigInt,
        skew_ms: BigInt,
        with_head: Bool,
    ) -> Record<(Text, Nullable<BigInt>, Nullable<Jsonb>, Bool, Nullable<Text>, Nullable<BigInt>)>;
}

diesel::define_sql_function! {
    fn guardian_serve_session_read(
        account_id: Text,
        realm: Text,
        token_digest: Bytea,
        ts: BigInt,
        past_ms: BigInt,
        future_ms: BigInt,
        with_head: Bool,
    ) -> Record<(Text, Nullable<BigInt>, Nullable<Jsonb>, Bool, Nullable<Text>, Nullable<BigInt>)>;
}

type AdmissionRow = (
    String,
    Option<i64>,
    Option<serde_json::Value>,
    bool,
    Option<String>,
    Option<i64>,
);

struct Admission {
    outcome: String,
    auth_version: Option<i64>,
    subject: Option<serde_json::Value>,
    head_found: bool,
    head_commitment: Option<String>,
    head_nonce: Option<i64>,
}

impl From<AdmissionRow> for Admission {
    fn from(
        (outcome, auth_version, subject, head_found, head_commitment, head_nonce): AdmissionRow,
    ) -> Self {
        Self {
            outcome,
            auth_version,
            subject,
            head_found,
            head_commitment,
            head_nonce,
        }
    }
}

impl Admission {
    fn head(&self, requested: HeadRead) -> Result<PrefetchedHead, String> {
        let accepted = matches!(self.outcome.as_str(), "recorded" | "served");
        match (requested, accepted, self.head_found) {
            (HeadRead::Include, true, true) => Ok(PrefetchedHead::Found(StateHead {
                commitment: self
                    .head_commitment
                    .clone()
                    .ok_or("Admitted session read returned a head without a commitment")?,
                nonce: self.head_nonce.and_then(|nonce| u64::try_from(nonce).ok()),
            })),
            (HeadRead::Include, true, false) => Ok(PrefetchedHead::Missing),
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
        "served" => Ok(ReadRecordOutcome::Served),
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
        let with_head = request.head == HeadRead::Include;
        let row: Admission = match request.rule {
            ReadRule::Record { key, skew_ms } => {
                diesel::select(guardian_admit_session_read(
                    request.account_id,
                    Realm::Miden.as_str(),
                    request.session_key.to_vec(),
                    key.to_vec(),
                    request.timestamp_ms,
                    skew_ms,
                    with_head,
                ))
                .get_result::<AdmissionRow>(&mut conn)
                .await
            }
            ReadRule::WindowOnly(window) => {
                diesel::select(guardian_serve_session_read(
                    request.account_id,
                    Realm::Miden.as_str(),
                    request.session_key.to_vec(),
                    request.timestamp_ms,
                    window.past_ms,
                    window.future_ms,
                    with_head,
                ))
                .get_result::<AdmissionRow>(&mut conn)
                .await
            }
        }
        .map_err(|e| format!("Failed to admit session read: {e}"))?
        .into();
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
    use crate::metadata::auth::TimestampWindow;
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
                            rule: ReadRule::Record {
                                key: record,
                                skew_ms: 300_000,
                            },
                            timestamp_ms: now,
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

        let window = TimestampWindow {
            past_ms: 60_000,
            future_ms: 30_000,
        };
        let serve = |account: String, key: [u8; 32], timestamp_ms: i64| {
            let store = &store;
            async move {
                store
                    .admit_session_read(
                        &NoLookups,
                        SessionReadRequest {
                            account_id: &account,
                            session_key: key,
                            rule: ReadRule::WindowOnly(window),
                            timestamp_ms,
                            clock: &MockClock::default(),
                            head: HeadRead::Include,
                        },
                    )
                    .await
                    .unwrap()
            }
        };
        let recorded = || async {
            let mut conn = store.pool.get().await.expect("conn");
            diesel::sql_query("SELECT count(*) AS watermark FROM session_read_seen")
                .get_result::<Watermark>(&mut conn)
                .await
                .unwrap()
                .watermark
        };
        let before = recorded().await;
        assert_eq!(
            serve(format!("{account_id}missing"), session(0xa1), now).await,
            SessionReadAdmission::AccountNotFound
        );
        for (key, status) in [
            (0xa9, InactiveSession::Unknown),
            (0xa2, InactiveSession::Revoked),
            (0xa3, InactiveSession::Expired),
        ] {
            assert_eq!(
                serve(account_id.clone(), session(key), now).await,
                SessionReadAdmission::Inactive(status)
            );
        }
        for _ in 0..3 {
            assert!(matches!(
                serve(account_id.clone(), session(0xa1), now).await,
                SessionReadAdmission::Live {
                    outcome: ReadRecordOutcome::Served,
                    head: PrefetchedHead::Found(StateHead { ref commitment, nonce: Some(7) }),
                    ..
                } if commitment == "0xhead"
            ));
        }
        for timestamp_ms in [now - 120_000, now + 120_000] {
            assert!(matches!(
                serve(account_id.clone(), session(0xa1), timestamp_ms).await,
                SessionReadAdmission::Live {
                    outcome: ReadRecordOutcome::OutsideWindow,
                    head: PrefetchedHead::NotRead,
                    ..
                }
            ));
        }
        assert_eq!(
            recorded().await,
            before,
            "a window-only read records nothing"
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_session_ending_while_admission_waits_is_refused_and_leaves_no_record() {
        let url = test_database_url().await;
        let store = PgReadRecords::new(build_postgres_pool_lazy(&url, 6).expect("pool"));
        store.maintain(0, 600_000, 3_600_000).await.unwrap();
        let account_id = format!("0xending{}", chrono::Utc::now().timestamp_micros());
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
        let new_session = |expires: &'static str| {
            let store = &store;
            let subject = &subject;
            async move {
                let session: [u8; 32] = rand::random();
                exec(
                    store,
                    &format!(
                        "INSERT INTO auth_sessions (realm, token_digest, subject, issued_at, expires_at, revoked_at) \
                         VALUES ('miden', '\\x{}', '{subject}', now(), {expires}, NULL)",
                        hex::encode(session)
                    ),
                )
                .await;
                session
            }
        };
        let now = {
            let mut conn = store.pool.get().await.expect("conn");
            diesel::sql_query("SELECT guardian_db_now_ms() AS watermark")
                .get_result::<Watermark>(&mut conn)
                .await
                .unwrap()
                .watermark
        };
        let admit = |session: [u8; 32], rule: ReadRule| {
            let store = &store;
            let account_id = &account_id;
            async move {
                store
                    .admit_session_read(
                        &NoLookups,
                        SessionReadRequest {
                            account_id,
                            session_key: session,
                            rule,
                            timestamp_ms: now,
                            clock: &MockClock::default(),
                            head: HeadRead::Include,
                        },
                    )
                    .await
                    .unwrap()
            }
        };
        let hold = |lock_sql: &'static str| {
            let store = &store;
            async move {
                let mut conn = store.pool.get().await.expect("conn");
                for sql in ["BEGIN", lock_sql] {
                    diesel::sql_query(sql).execute(&mut conn).await.expect(sql);
                }
                conn
            }
        };
        let blocked = |waiters: i64| {
            let store = &store;
            async move {
                for _ in 0..200 {
                    let mut conn = store.pool.get().await.expect("conn");
                    let waiting = diesel::sql_query(
                        "SELECT count(*) AS watermark FROM pg_locks WHERE NOT granted",
                    )
                    .get_result::<Watermark>(&mut conn)
                    .await
                    .unwrap()
                    .watermark;
                    if waiting >= waiters {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                panic!("{waiters} admission(s) never blocked on the held lock");
            }
        };
        let expiring = "clock_timestamp() + interval '2 seconds'";
        let window_only = ReadRule::WindowOnly(TimestampWindow {
            past_ms: 60_000,
            future_ms: 30_000,
        });
        let assert_live = |session: [u8; 32]| {
            let admit = &admit;
            async move {
                assert!(
                    matches!(
                        admit(session, window_only).await,
                        SessionReadAdmission::Live {
                            outcome: ReadRecordOutcome::Served,
                            ..
                        }
                    ),
                    "the session is live before the wait"
                );
            }
        };

        let session = new_session(expiring).await;
        assert_live(session).await;
        let mut lock = hold("LOCK TABLE account_metadata IN ACCESS EXCLUSIVE MODE").await;
        let release = async {
            blocked(1).await;
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            diesel::sql_query("COMMIT")
                .execute(&mut lock)
                .await
                .expect("COMMIT");
        };
        let (served, ()) = tokio::join!(admit(session, window_only), release);
        assert_eq!(
            served,
            SessionReadAdmission::Inactive(InactiveSession::Expired)
        );

        let expired = new_session(expiring).await;
        let revoked = new_session("now() + interval '1 hour'").await;
        assert_live(expired).await;
        assert_live(revoked).await;
        let (expired_key, revoked_key): (ReadRecordKey, ReadRecordKey) =
            (rand::random(), rand::random());
        let mut fence = hold("SELECT pg_advisory_xact_lock(7310317001)").await;
        let revoke_then_release = async {
            blocked(2).await;
            exec(
                &store,
                &format!(
                    "UPDATE auth_sessions SET revoked_at = now() WHERE token_digest = '\\x{}'",
                    hex::encode(revoked)
                ),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
            diesel::sql_query("COMMIT")
                .execute(&mut fence)
                .await
                .expect("COMMIT");
        };
        let record = |key| ReadRule::Record {
            key,
            skew_ms: 300_000,
        };
        let (after_expiry, after_revoke, ()) = tokio::join!(
            admit(expired, record(expired_key)),
            admit(revoked, record(revoked_key)),
            revoke_then_release,
        );
        assert_eq!(
            after_expiry,
            SessionReadAdmission::Inactive(InactiveSession::Expired)
        );
        assert_eq!(
            after_revoke,
            SessionReadAdmission::Inactive(InactiveSession::Revoked)
        );
        exec(
            &store,
            &format!(
                "INSERT INTO states (account_id, state_json, commitment, created_at, updated_at, nonce) \
                 VALUES ('{account_id}', '{{}}', '0xhead', now(), now(), 7)"
            ),
        )
        .await;
        let served_session = new_session("now() + interval '1 hour'").await;
        let recorded_session = new_session("now() + interval '1 hour'").await;
        assert_live(served_session).await;
        assert_live(recorded_session).await;
        let head_key: ReadRecordKey = rand::random();
        let mut head_lock = hold("LOCK TABLE states IN ACCESS EXCLUSIVE MODE").await;
        let revoke_both_then_release = async {
            blocked(2).await;
            for session in [served_session, recorded_session] {
                exec(
                    &store,
                    &format!(
                        "UPDATE auth_sessions SET revoked_at = now() WHERE token_digest = '\\x{}'",
                        hex::encode(session)
                    ),
                )
                .await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            diesel::sql_query("COMMIT")
                .execute(&mut head_lock)
                .await
                .expect("COMMIT");
        };
        let (served_after_head_wait, recorded_after_head_wait, ()) = tokio::join!(
            admit(served_session, window_only),
            admit(recorded_session, record(head_key)),
            revoke_both_then_release,
        );
        assert_eq!(
            served_after_head_wait,
            SessionReadAdmission::Inactive(InactiveSession::Revoked)
        );
        assert_eq!(
            recorded_after_head_wait,
            SessionReadAdmission::Inactive(InactiveSession::Revoked)
        );

        let mut conn = store.pool.get().await.expect("conn");
        let left = diesel::sql_query(format!(
            "SELECT count(*) AS watermark FROM session_read_seen \
             WHERE record_key IN ('\\x{}', '\\x{}', '\\x{}')",
            hex::encode(expired_key),
            hex::encode(revoked_key),
            hex::encode(head_key)
        ))
        .get_result::<Watermark>(&mut conn)
        .await
        .unwrap()
        .watermark;
        assert_eq!(left, 0, "a refused read leaves no record");
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_signer_change_while_admission_waits_returns_the_new_metadata_version() {
        let url = test_database_url().await;
        let store = PgReadRecords::new(build_postgres_pool_lazy(&url, 4).expect("pool"));
        store.maintain(0, 600_000, 3_600_000).await.unwrap();
        let account_id = format!("0xsigners{}", chrono::Utc::now().timestamp_micros());
        exec(
            &store,
            &format!(
                "INSERT INTO account_metadata \
                 (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
                 VALUES ('{account_id}', '{{\"MidenFalconRpo\":{{\"cosigner_commitments\":[\"0x1\"]}}}}', \
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
        let session: [u8; 32] = rand::random();
        exec(
            &store,
            &format!(
                "INSERT INTO auth_sessions (realm, token_digest, subject, issued_at, expires_at, revoked_at) \
                 VALUES ('miden', '\\x{}', '{subject}', now(), now() + interval '1 hour', NULL)",
                hex::encode(session)
            ),
        )
        .await;
        let scalar = |sql: String| {
            let store = &store;
            async move {
                let mut conn = store.pool.get().await.expect("conn");
                diesel::sql_query(sql)
                    .get_result::<Watermark>(&mut conn)
                    .await
                    .unwrap()
                    .watermark
            }
        };
        let version_sql = format!(
            "SELECT auth_version AS watermark FROM account_metadata WHERE account_id = '{account_id}'"
        );
        let before = scalar(version_sql.clone()).await;
        let now = scalar("SELECT guardian_db_now_ms() AS watermark".to_string()).await;

        let mut fence = store.pool.get().await.expect("conn");
        for sql in ["BEGIN", "SELECT pg_advisory_xact_lock(7310317001)"] {
            diesel::sql_query(sql).execute(&mut fence).await.expect(sql);
        }
        let remove_signer_then_release = async {
            let fence_waiters = "SELECT count(*) AS watermark FROM pg_locks \
                 WHERE locktype = 'advisory' AND NOT granted \
                 AND ((classid::bigint << 32) | objid::bigint) = 7310317001";
            let mut waited = false;
            for _ in 0..200 {
                if scalar(fence_waiters.to_string()).await >= 1 {
                    waited = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            assert!(waited, "the admission never waited on the read fence");
            exec(
                &store,
                &format!(
                    "UPDATE account_metadata SET auth = \
                     '{{\"MidenFalconRpo\":{{\"cosigner_commitments\":[\"0x3\"]}}}}' \
                     WHERE account_id = '{account_id}'"
                ),
            )
            .await;
            diesel::sql_query("COMMIT")
                .execute(&mut fence)
                .await
                .expect("COMMIT");
        };
        let clock = MockClock::default();
        let admit = store.admit_session_read(
            &NoLookups,
            SessionReadRequest {
                account_id: &account_id,
                session_key: session,
                rule: ReadRule::Record {
                    key: rand::random(),
                    skew_ms: 300_000,
                },
                timestamp_ms: now,
                clock: &clock,
                head: HeadRead::Include,
            },
        );
        let (admission, ()) = tokio::join!(admit, remove_signer_then_release);
        let after = scalar(version_sql).await;
        assert!(after > before, "the signer change bumps the version");
        match admission.unwrap() {
            SessionReadAdmission::Live {
                metadata_version,
                outcome: ReadRecordOutcome::Recorded,
                ..
            } => assert_eq!(
                metadata_version,
                Some(after),
                "the grant is checked against the signer set after the wait"
            ),
            other => panic!("expected a recorded read, got {other:?}"),
        }
    }
}
