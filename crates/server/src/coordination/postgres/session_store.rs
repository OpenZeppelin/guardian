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
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn insert(&self, key: SessionKey, session: StoredSession) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "session").await?;
        let subject = serde_json::to_value(&session.subject).map_err(|error| {
            GuardianError::StorageError(format!("session subject encode: {error}"))
        })?;
        let row = NewAuthSession {
            token_digest: key.to_vec(),
            realm: self.realm.as_str().to_string(),
            subject,
            issued_at: session.issued_at,
            expires_at: session.expires_at,
        };
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
    use crate::storage::postgres::{build_postgres_pool_lazy, run_migrations};
    use crate::testing::pg::test_database_url;
    use chrono::Duration;

    fn unique_key(now: DateTime<Utc>) -> SessionKey {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&now.timestamp_micros().to_le_bytes().repeat(2)[..16]);
        key
    }

    #[tokio::test]
    async fn get_fails_closed_when_store_unreachable() {
        let pool = build_postgres_pool_lazy("postgresql://127.0.0.1:1/__guardian_coord_fault__", 1)
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
        run_migrations(&url).await.expect("migrations apply");
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
}
