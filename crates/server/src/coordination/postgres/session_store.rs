use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::coordination::Realm;
use crate::coordination::session_store::{SessionKey, SessionStore, SessionSubject, StoredSession};
use crate::error::{GuardianError, Result};
use crate::schema::auth_sessions;

#[derive(Insertable)]
#[diesel(table_name = auth_sessions)]
struct NewAuthSession {
    token_digest: Vec<u8>,
    realm: String,
    subject: serde_json::Value,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = auth_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[allow(dead_code)]
struct AuthSessionRow {
    token_digest: Vec<u8>,
    realm: String,
    subject: serde_json::Value,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl AuthSessionRow {
    fn into_stored(self) -> Result<StoredSession> {
        let subject: SessionSubject = serde_json::from_value(self.subject).map_err(|error| {
            GuardianError::StorageError(format!("session subject decode: {error}"))
        })?;
        Ok(StoredSession {
            subject,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        })
    }
}

/// Postgres-backed [`SessionStore`] bound to one realm. Expiry and revocation
/// use the database clock so every replica agrees. Any DB error surfaces as a
/// `StorageError`, which the auth path treats as fail-closed.
pub struct PgSessionStore {
    pool: Pool<AsyncPgConnection>,
    realm: Realm,
}

impl PgSessionStore {
    pub fn new(pool: Pool<AsyncPgConnection>, realm: Realm) -> Self {
        Self { pool, realm }
    }

    fn new_row(&self, key: SessionKey, session: &StoredSession) -> Result<NewAuthSession> {
        let subject = serde_json::to_value(&session.subject).map_err(|error| {
            GuardianError::StorageError(format!("session subject encode: {error}"))
        })?;
        Ok(NewAuthSession {
            token_digest: key.to_vec(),
            realm: self.realm.as_str().to_string(),
            subject,
            issued_at: session.issued_at,
            expires_at: session.expires_at,
        })
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn insert(&self, key: SessionKey, session: StoredSession) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let row = self.new_row(key, &session)?;
        // Upsert: a digest collision (astronomically unlikely) or a re-insert
        // over an unswept revoked row replaces it with the fresh, unrevoked
        // session rather than erroring.
        diesel::insert_into(auth_sessions::table)
            .values(&row)
            .on_conflict((auth_sessions::realm, auth_sessions::token_digest))
            .do_update()
            .set((
                auth_sessions::realm.eq(self.realm.as_str()),
                auth_sessions::subject.eq(&row.subject),
                auth_sessions::issued_at.eq(session.issued_at),
                auth_sessions::expires_at.eq(session.expires_at),
                auth_sessions::revoked_at.eq(None::<DateTime<Utc>>),
            ))
            .execute(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("session insert: {error}")))?;
        Ok(())
    }

    async fn insert_new(&self, key: SessionKey, session: StoredSession) -> Result<bool> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let row = self.new_row(key, &session)?;
        let inserted = diesel::insert_into(auth_sessions::table)
            .values(&row)
            .on_conflict((auth_sessions::realm, auth_sessions::token_digest))
            .do_nothing()
            .execute(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("session insert: {error}")))?;
        Ok(inserted == 1)
    }

    async fn get(&self, key: &SessionKey, _now: DateTime<Utc>) -> Result<Option<StoredSession>> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let row = auth_sessions::table
            .filter(auth_sessions::token_digest.eq(key.to_vec()))
            .filter(auth_sessions::realm.eq(self.realm.as_str()))
            .filter(auth_sessions::revoked_at.is_null())
            .filter(auth_sessions::expires_at.gt(diesel::dsl::now))
            .select(AuthSessionRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|error| GuardianError::StorageError(format!("session lookup: {error}")))?;
        row.map(AuthSessionRow::into_stored).transpose()
    }

    async fn inactive_reason(&self, key: &SessionKey) -> Result<Option<bool>> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let revoked_at = auth_sessions::table
            .filter(auth_sessions::token_digest.eq(key.to_vec()))
            .filter(auth_sessions::realm.eq(self.realm.as_str()))
            .select(auth_sessions::revoked_at)
            .first::<Option<DateTime<Utc>>>(&mut conn)
            .await
            .optional()
            .map_err(|error| GuardianError::StorageError(format!("session lookup: {error}")))?;
        Ok(revoked_at.map(|revoked_at| revoked_at.is_some()))
    }

    async fn revoke(&self, key: &SessionKey) -> Result<Option<StoredSession>> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let row = diesel::update(auth_sessions::table)
            .filter(auth_sessions::token_digest.eq(key.to_vec()))
            .filter(auth_sessions::realm.eq(self.realm.as_str()))
            .filter(auth_sessions::revoked_at.is_null())
            .set(auth_sessions::revoked_at.eq(diesel::dsl::now))
            .returning(AuthSessionRow::as_returning())
            .get_result(&mut conn)
            .await
            .optional()
            .map_err(|error| GuardianError::StorageError(format!("session revoke: {error}")))?;
        row.map(AuthSessionRow::into_stored).transpose()
    }

    async fn revoke_by_subject(
        &self,
        filter: &serde_json::Value,
        issued_at_or_before: DateTime<Utc>,
        _now: DateTime<Utc>,
    ) -> Result<u64> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let revoked = diesel::update(auth_sessions::table)
            .filter(auth_sessions::realm.eq(self.realm.as_str()))
            .filter(auth_sessions::revoked_at.is_null())
            .filter(auth_sessions::expires_at.gt(diesel::dsl::now))
            .filter(auth_sessions::issued_at.le(issued_at_or_before))
            .filter(auth_sessions::subject.contains(filter.clone()))
            .set(auth_sessions::revoked_at.eq(diesel::dsl::now))
            .execute(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("session revoke: {error}")))?;
        Ok(revoked as u64)
    }

    async fn sweep_expired(&self, _now: DateTime<Utc>) -> Result<u64> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let deleted = diesel::delete(auth_sessions::table)
            .filter(auth_sessions::realm.eq(self.realm.as_str()))
            .filter(auth_sessions::expires_at.lt(diesel::dsl::now))
            .execute(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("session sweep: {error}")))?;
        Ok(deleted as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::pg::test_database_url;
    use chrono::Duration;

    fn unique_key(now: DateTime<Utc>) -> SessionKey {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&now.timestamp_micros().to_le_bytes().repeat(2)[..16]);
        key
    }

    #[tokio::test]
    async fn get_fails_closed_when_store_unreachable() {
        let pool = build_postgres_pool_lazy(
            "postgresql://127.0.0.1:1/__guardian_coord_fault__?connect_timeout=1",
            1,
        )
        .expect("lazy pool builds even with an unreachable address");
        let store = PgSessionStore::new(pool, Realm::Operator);
        assert!(
            store.get(&[7u8; 32], Utc::now()).await.is_err(),
            "session lookup must fail closed when the store is unreachable",
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn session_visible_across_replicas_and_revoke_propagates() {
        let url = test_database_url().await;
        let replica_a = PgSessionStore::new(
            build_postgres_pool_lazy(&url, 2).expect("pool a"),
            Realm::Operator,
        );
        let replica_b = PgSessionStore::new(
            build_postgres_pool_lazy(&url, 2).expect("pool b"),
            Realm::Operator,
        );
        let now = Utc::now();
        let key = unique_key(now);

        replica_a
            .insert(
                key,
                StoredSession {
                    subject: SessionSubject::Operator {
                        operator_id: "op-x".to_string(),
                        commitment: "0xc".to_string(),
                    },
                    issued_at: now,
                    expires_at: now + Duration::hours(1),
                },
            )
            .await
            .expect("insert on replica A");

        assert!(
            replica_b.get(&key, now).await.expect("get on B").is_some(),
            "a session written by replica A must be visible on replica B",
        );

        assert!(
            replica_a.revoke(&key).await.expect("revoke on A").is_some(),
            "revoke returns the prior session",
        );
        assert!(
            replica_b
                .get(&key, now)
                .await
                .expect("get on B after revoke")
                .is_none(),
            "revocation on A must be honored on B",
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn insert_new_never_revives_a_revoked_miden_session() {
        let url = test_database_url().await;
        let store = PgSessionStore::new(
            build_postgres_pool_lazy(&url, 2).expect("pool"),
            Realm::Miden,
        );
        let now = Utc::now();
        let mut key = unique_key(now);
        key[31] = 0x4d;
        let session = || StoredSession {
            subject: SessionSubject::Miden {
                signer_commitment: "0xabc".to_string(),
                origin: String::new(),
                guardian_commitment: "0xguardian".to_string(),
                network: "devnet".to_string(),
            },
            issued_at: now,
            expires_at: now + Duration::hours(1),
        };

        assert!(
            store
                .insert_new(key, session())
                .await
                .expect("first insert")
        );
        assert!(
            !store
                .insert_new(key, session())
                .await
                .expect("duplicate insert"),
            "an active session key cannot be registered twice",
        );
        let stored = store.get(&key, now).await.expect("get").expect("active");
        assert!(matches!(stored.subject, SessionSubject::Miden { .. }));

        store.revoke(&key).await.expect("revoke");
        assert!(
            !store
                .insert_new(key, session())
                .await
                .expect("insert after revoke"),
            "a revoked session must not be revived",
        );
        assert!(
            store
                .get(&key, now)
                .await
                .expect("get after revoke")
                .is_none()
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn miden_sessions_report_why_they_ended_and_revoke_by_signer_up_to_a_cutoff() {
        let url = test_database_url().await;
        let store = PgSessionStore::new(
            build_postgres_pool_lazy(&url, 2).expect("pool"),
            Realm::Miden,
        );
        let now = Utc::now();
        let signer = format!("0x{:064x}", now.timestamp_nanos_opt().unwrap_or_default());
        let session =
            |signer: &str, issued_at: DateTime<Utc>, expires_at: DateTime<Utc>| StoredSession {
                subject: SessionSubject::Miden {
                    signer_commitment: signer.to_string(),
                    origin: String::new(),
                    guardian_commitment: "0xguardian".to_string(),
                    network: "devnet".to_string(),
                },
                issued_at,
                expires_at,
            };
        let key = |tag: u8| {
            let mut key = unique_key(now);
            key[30] = 0x5e;
            key[31] = tag;
            key
        };
        let hour = Duration::hours(1);

        // (tag, signer, issued_at, expires_at)
        let rows = [
            (
                1u8,
                signer.as_str(),
                now - Duration::seconds(10),
                now + hour,
            ),
            (2, signer.as_str(), now + Duration::seconds(10), now + hour),
            (3, "0xother", now - Duration::seconds(10), now + hour),
            (4, signer.as_str(), now - hour, now - Duration::seconds(1)),
        ];
        for (tag, signer, issued_at, expires_at) in rows {
            assert!(
                store
                    .insert_new(key(tag), session(signer, issued_at, expires_at))
                    .await
                    .expect("insert")
            );
        }

        let filter = serde_json::json!({ "realm": "miden", "signer_commitment": signer });
        assert_eq!(
            store
                .revoke_by_subject(&filter, now, now)
                .await
                .expect("revoke"),
            1,
            "only the live session issued at or before the cutoff"
        );
        assert_eq!(store.inactive_reason(&key(1)).await.expect("1"), Some(true));
        assert!(
            store.get(&key(2), now).await.expect("2").is_some(),
            "issued later"
        );
        assert!(
            store.get(&key(3), now).await.expect("3").is_some(),
            "other signer"
        );
        assert_eq!(
            store.inactive_reason(&key(4)).await.expect("4"),
            Some(false),
            "expired, not revoked"
        );
        assert_eq!(store.inactive_reason(&key(9)).await.expect("9"), None);
    }
}
