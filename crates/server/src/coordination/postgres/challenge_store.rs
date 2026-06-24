use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Double, Jsonb, Text};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};

use crate::coordination::Realm;
use crate::coordination::challenge_store::{ChallengePayload, ChallengeStore, StoredChallenge};
use crate::error::{GuardianError, Result};
use crate::schema::auth_challenges;

#[derive(Queryable, Selectable)]
#[diesel(table_name = auth_challenges)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[allow(dead_code)]
struct AuthChallengeRow {
    realm: String,
    challenge_key: String,
    principal: String,
    payload: serde_json::Value,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    consumed_at: Option<DateTime<Utc>>,
}

impl AuthChallengeRow {
    fn into_stored(self) -> Result<StoredChallenge> {
        Ok(StoredChallenge {
            key: self.challenge_key,
            payload: ChallengePayload::from_value(&self.payload)?,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        })
    }
}

/// Postgres-backed [`ChallengeStore`] bound to one realm. Verification matches
/// in Rust over [`ChallengeStore::active_for`]; [`ChallengeStore::consume`] is an
/// atomic single-use claim keyed by `(realm, challenge_key)`.
pub struct PgChallengeStore {
    pool: Pool<AsyncPgConnection>,
    realm: Realm,
}

impl PgChallengeStore {
    pub fn new(pool: Pool<AsyncPgConnection>, realm: Realm) -> Self {
        Self { pool, realm }
    }
}

#[async_trait]
impl ChallengeStore for PgChallengeStore {
    async fn issue(
        &self,
        principal: &str,
        challenge: StoredChallenge,
        max_outstanding: usize,
        _now: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = super::checkout(&self.pool, "challenge").await?;

        let realm = self.realm.as_str().to_string();
        let principal = principal.to_string();
        let challenge_key = challenge.key;
        let payload = challenge.payload.to_value();
        // The duration is clock-independent (both ends app-computed); anchoring on
        // the DB clock for the stored row keeps expiry/capping consistent across
        // replicas regardless of per-process clock skew.
        let ttl_secs = (challenge.expires_at - challenge.issued_at)
            .num_seconds()
            .max(0) as f64;
        let max = max_outstanding as i64;

        conn.transaction::<(), diesel::result::Error, _>(|conn| {
            async move {
                // ON CONFLICT refreshes a re-issued challenge (latest wins,
                // re-arming consumed_at) rather than aborting the transaction on
                // a duplicate `(realm, challenge_key)`. Keys are random nonces /
                // unique digests so a collision is practically a re-issue; this
                // matches InMemoryChallengeStore, which tolerates re-issue.
                diesel::sql_query(
                    "INSERT INTO auth_challenges \
                     (realm, challenge_key, principal, payload, issued_at, expires_at) \
                     VALUES ($1, $2, $3, $4, now(), now() + make_interval(secs => $5)) \
                     ON CONFLICT (realm, challenge_key) DO UPDATE SET \
                         principal = EXCLUDED.principal, \
                         payload = EXCLUDED.payload, \
                         issued_at = EXCLUDED.issued_at, \
                         expires_at = EXCLUDED.expires_at, \
                         consumed_at = NULL",
                )
                .bind::<Text, _>(&realm)
                .bind::<Text, _>(&challenge_key)
                .bind::<Text, _>(&principal)
                .bind::<Jsonb, _>(&payload)
                .bind::<Double, _>(ttl_secs)
                .execute(conn)
                .await?;

                diesel::sql_query(
                    "DELETE FROM auth_challenges \
                     WHERE realm = $1 AND principal = $2 AND expires_at < now()",
                )
                .bind::<Text, _>(&realm)
                .bind::<Text, _>(&principal)
                .execute(conn)
                .await?;

                diesel::sql_query(
                    "DELETE FROM auth_challenges WHERE ctid IN (\
                     SELECT ctid FROM auth_challenges \
                     WHERE realm = $1 AND principal = $2 \
                     ORDER BY issued_at DESC OFFSET $3)",
                )
                .bind::<Text, _>(&realm)
                .bind::<Text, _>(&principal)
                .bind::<BigInt, _>(max)
                .execute(conn)
                .await?;

                Ok(())
            }
            .scope_boxed()
        })
        .await
        .map_err(|error| GuardianError::StorageError(format!("challenge issue: {error}")))?;

        Ok(())
    }

    async fn active_for(
        &self,
        principal: &str,
        _now: DateTime<Utc>,
    ) -> Result<Vec<StoredChallenge>> {
        let mut conn = super::checkout(&self.pool, "challenge").await?;
        let rows = auth_challenges::table
            .filter(auth_challenges::realm.eq(self.realm.as_str()))
            .filter(auth_challenges::principal.eq(principal))
            .filter(auth_challenges::consumed_at.is_null())
            .filter(auth_challenges::expires_at.gt(diesel::dsl::now))
            .select(AuthChallengeRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("challenge load: {error}")))?;
        rows.into_iter()
            .map(AuthChallengeRow::into_stored)
            .collect()
    }

    async fn consume(&self, principal: &str, key: &str, _now: DateTime<Utc>) -> Result<bool> {
        let mut conn = super::checkout(&self.pool, "challenge").await?;
        // `principal` is part of the predicate (not just `(realm, key)`) so the
        // Postgres and in-memory impls agree that a wrong-principal consume fails.
        let affected = diesel::update(auth_challenges::table)
            .filter(auth_challenges::realm.eq(self.realm.as_str()))
            .filter(auth_challenges::principal.eq(principal))
            .filter(auth_challenges::challenge_key.eq(key))
            .filter(auth_challenges::consumed_at.is_null())
            .filter(auth_challenges::expires_at.gt(diesel::dsl::now))
            .set(auth_challenges::consumed_at.eq(diesel::dsl::now))
            .execute(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("challenge consume: {error}")))?;
        Ok(affected == 1)
    }

    async fn sweep_expired(&self, _now: DateTime<Utc>) -> Result<u64> {
        let mut conn = super::checkout(&self.pool, "challenge").await?;
        let deleted = diesel::delete(auth_challenges::table)
            .filter(auth_challenges::realm.eq(self.realm.as_str()))
            .filter(auth_challenges::expires_at.lt(diesel::dsl::now))
            .execute(&mut conn)
            .await
            .map_err(|error| GuardianError::StorageError(format!("challenge sweep: {error}")))?;
        Ok(deleted as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::postgres::{build_postgres_pool_lazy, run_migrations};
    use chrono::Duration;

    fn database_url() -> Option<String> {
        std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
    }

    #[tokio::test]
    async fn active_for_fails_closed_when_store_unreachable() {
        let pool = build_postgres_pool_lazy("postgresql://127.0.0.1:1/__guardian_coord_fault__", 1)
            .expect("lazy pool builds even with an unreachable address");
        let store = PgChallengeStore::new(pool, Realm::Operator);
        assert!(
            store.active_for("0xprincipal", Utc::now()).await.is_err(),
            "challenge lookup must fail closed when the store is unreachable",
        );
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn challenge_is_single_use_across_replicas() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        run_migrations(&url).await.expect("migrations apply");
        let replica_a = PgChallengeStore::new(
            build_postgres_pool_lazy(&url, 2).expect("pool a"),
            Realm::Evm,
        );
        let replica_b = PgChallengeStore::new(
            build_postgres_pool_lazy(&url, 2).expect("pool b"),
            Realm::Evm,
        );
        let now = Utc::now();
        let stamp = now.timestamp_micros();
        let principal = format!("0xprincipal-{stamp}");
        let key = format!("nonce-{stamp}");

        replica_a
            .issue(
                &principal,
                StoredChallenge {
                    key: key.clone(),
                    payload: ChallengePayload::EvmChallenge {
                        address: principal.clone(),
                        nonce: key.clone(),
                        issued_at: now,
                        expires_at: now + Duration::minutes(5),
                    },
                    issued_at: now,
                    expires_at: now + Duration::minutes(5),
                },
                8,
                now,
            )
            .await
            .expect("issue on replica A");

        assert!(
            replica_b
                .active_for(&principal, now)
                .await
                .expect("active_for on B")
                .iter()
                .any(|challenge| challenge.key == key),
            "a challenge issued on A must be visible on B",
        );

        assert!(
            replica_b
                .consume(&principal, &key, now)
                .await
                .expect("consume on B"),
            "first consume wins on replica B",
        );
        assert!(
            !replica_a
                .consume(&principal, &key, now)
                .await
                .expect("replay consume on A"),
            "single-use: a replay on replica A must lose",
        );
    }
}
