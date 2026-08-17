use crate::metadata::{AccountListCursor, AccountMetadata, Auth, MetadataStore, NetworkConfig};
use crate::schema::account_metadata;
use crate::services::account_status::{AccountStatus, PauseTransition};
use crate::storage::postgres::build_postgres_pool;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::Text;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

pub struct PostgresMetadataStore {
    pool: Pool<AsyncPgConnection>,
}

impl PostgresMetadataStore {
    pub async fn new(database_url: &str, pool_max_size: usize) -> Result<Self, String> {
        let pool = build_postgres_pool(database_url, pool_max_size).await?;
        Ok(Self { pool })
    }

    pub async fn with_pool(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }

    /// Clone of the underlying connection pool. Used by the
    /// feature-006-operator-authz `PostgresAuditor` to write audit
    /// rows through the same pool the rest of the metadata layer
    /// uses, so audit and metadata writes share connection capacity.
    pub fn pool_handle(&self) -> Pool<AsyncPgConnection> {
        self.pool.clone()
    }
}

/// Row shape for the cosigner-commitment lookup query. Uses `QueryableByName`
/// because the lookup is expressed as raw SQL (`@> to_jsonb($1::text)`) rather
/// than the diesel DSL.
#[derive(diesel::QueryableByName)]
struct LookupAccountIdRow {
    #[diesel(sql_type = Text)]
    account_id: String,
}

// Queryable struct for reading from database
#[derive(Queryable, Selectable)]
#[diesel(table_name = account_metadata)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct MetadataRow {
    account_id: String,
    auth: serde_json::Value,
    network_config: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    has_pending_candidate: bool,
    paused_at: Option<chrono::DateTime<chrono::Utc>>,
    paused_reason: Option<String>,
    released_at: Option<chrono::DateTime<chrono::Utc>>,
}

// Insertable struct for writing to database
#[derive(Insertable, AsChangeset)]
#[diesel(table_name = account_metadata)]
struct NewMetadata<'a> {
    account_id: &'a str,
    auth: serde_json::Value,
    network_config: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    has_pending_candidate: bool,
    paused_at: Option<chrono::DateTime<chrono::Utc>>,
    paused_reason: Option<String>,
    released_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TryFrom<MetadataRow> for AccountMetadata {
    type Error = String;

    fn try_from(row: MetadataRow) -> Result<Self, Self::Error> {
        let auth: Auth =
            serde_json::from_value(row.auth).map_err(|e| format!("Failed to parse auth: {e}"))?;
        let network_config: NetworkConfig = serde_json::from_value(row.network_config)
            .map_err(|e| format!("Failed to parse network_config: {e}"))?;

        Ok(AccountMetadata {
            account_id: row.account_id,
            auth,
            network_config,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            has_pending_candidate: row.has_pending_candidate,
            paused_at: row.paused_at,
            paused_reason: row.paused_reason,
            released_at: row.released_at,
        })
    }
}

#[async_trait]
impl MetadataStore for PostgresMetadataStore {
    fn pool_status(&self) -> Option<crate::storage::PoolStatus> {
        let status = self.pool.status();
        Some(crate::storage::PoolStatus {
            max_connections: status.max_size as u64,
            connections: status.size as u64,
            available: status.available as u64,
            pending_acquires: status.waiting as u64,
        })
    }

    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let result: Option<MetadataRow> = account_metadata::table
            .filter(account_metadata::account_id.eq(account_id))
            .select(MetadataRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| format!("Failed to get metadata: {e}"))?;

        match result {
            Some(row) => Ok(Some(row.try_into()?)),
            None => Ok(None),
        }
    }

    async fn set(&self, metadata: AccountMetadata) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let created_at: chrono::DateTime<chrono::Utc> = metadata
            .created_at
            .parse()
            .map_err(|e| format!("Failed to parse created_at: {e}"))?;
        let updated_at: chrono::DateTime<chrono::Utc> = metadata
            .updated_at
            .parse()
            .map_err(|e| format!("Failed to parse updated_at: {e}"))?;

        let auth_json = serde_json::to_value(&metadata.auth)
            .map_err(|e| format!("Failed to serialize auth: {e}"))?;
        let network_config_json = serde_json::to_value(&metadata.network_config)
            .map_err(|e| format!("Failed to serialize network_config: {e}"))?;

        let new_metadata = NewMetadata {
            account_id: &metadata.account_id,
            auth: auth_json.clone(),
            network_config: network_config_json.clone(),
            created_at,
            updated_at,
            has_pending_candidate: metadata.has_pending_candidate,
            paused_at: metadata.paused_at,
            paused_reason: metadata.paused_reason.clone(),
            released_at: metadata.released_at,
        };

        // Lifecycle fields are owned by their dedicated mutation methods and
        // must not be changed by a generic metadata write.
        diesel::insert_into(account_metadata::table)
            .values(&new_metadata)
            .on_conflict(account_metadata::account_id)
            .do_update()
            .set((
                account_metadata::auth.eq(&auth_json),
                account_metadata::network_config.eq(&network_config_json),
                account_metadata::updated_at.eq(updated_at),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to set metadata: {e}"))?;

        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<String> = account_metadata::table
            .select(account_metadata::account_id)
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list accounts: {e}"))?;

        Ok(rows)
    }

    async fn list_paged(
        &self,
        limit: u32,
        cursor: Option<AccountListCursor>,
        paused: Option<bool>,
    ) -> Result<Vec<AccountMetadata>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let mut query = account_metadata::table.into_boxed();
        match paused {
            // `is_not_null` hits the partial index on `paused_at`.
            Some(true) => query = query.filter(account_metadata::paused_at.is_not_null()),
            Some(false) => query = query.filter(account_metadata::paused_at.is_null()),
            None => {}
        }
        if let Some(c) = cursor {
            // Composite predicate over `(updated_at DESC, account_id ASC)`:
            //   updated_at < c.ts
            //   OR (updated_at == c.ts AND account_id > c.id)
            query = query.filter(
                account_metadata::updated_at
                    .lt(c.last_updated_at)
                    .or(account_metadata::updated_at
                        .eq(c.last_updated_at)
                        .and(account_metadata::account_id.gt(c.last_account_id))),
            );
        }

        let rows: Vec<MetadataRow> = query
            .order((
                account_metadata::updated_at.desc(),
                account_metadata::account_id.asc(),
            ))
            .limit(limit as i64)
            .select(MetadataRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list account metadata: {e}"))?;

        rows.into_iter().map(AccountMetadata::try_from).collect()
    }

    async fn list_with_pending_candidates(&self) -> Result<Vec<String>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<String> = account_metadata::table
            .filter(account_metadata::has_pending_candidate.eq(true))
            .select(account_metadata::account_id)
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list accounts with pending candidates: {e}"))?;

        Ok(rows)
    }

    async fn update_last_auth_timestamp_cas(
        &self,
        account_id: &str,
        signer_commitment: &str,
        new_timestamp: i64,
    ) -> Result<bool, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows_updated = diesel::sql_query(
            "INSERT INTO account_auth_state (account_id, signer_commitment, last_auth_timestamp) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (account_id, signer_commitment) DO UPDATE \
             SET last_auth_timestamp = EXCLUDED.last_auth_timestamp \
             WHERE account_auth_state.last_auth_timestamp < EXCLUDED.last_auth_timestamp",
        )
        .bind::<Text, _>(account_id)
        .bind::<Text, _>(signer_commitment)
        .bind::<diesel::sql_types::BigInt, _>(new_timestamp)
        .execute(&mut conn)
        .await
        .map_err(|e| format!("Failed to update last_auth_timestamp: {e}"))?;

        Ok(rows_updated > 0)
    }

    async fn set_has_pending_candidate(
        &self,
        account_id: &str,
        has_candidate: bool,
        now: &str,
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;
        let updated_at: DateTime<Utc> = now
            .parse()
            .map_err(|e| format!("Failed to parse timestamp: {e}"))?;

        let rows_updated = diesel::update(account_metadata::table)
            .filter(account_metadata::account_id.eq(account_id))
            .set((
                account_metadata::has_pending_candidate.eq(has_candidate),
                account_metadata::updated_at.eq(updated_at),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to set pending-candidate flag: {e}"))?;

        match rows_updated {
            0 => Err(format!("Account not found: {account_id}")),
            _ => Ok(()),
        }
    }

    /// Atomic override of the trait default: the candidate-row check and the
    /// flag write run in one statement against one snapshot, so a raced-in
    /// submission's flag can never be clobbered (see the trait doc).
    async fn clear_pending_candidate_if_none(
        &self,
        account_id: &str,
        now: &str,
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let updated_at: DateTime<Utc> = now
            .parse()
            .map_err(|e| format!("Failed to parse timestamp: {e}"))?;

        diesel::update(account_metadata::table)
            .filter(account_metadata::account_id.eq(account_id))
            .filter(account_metadata::has_pending_candidate.eq(true))
            .filter(diesel::dsl::not(diesel::dsl::exists(
                crate::schema::deltas::table
                    .filter(crate::schema::deltas::account_id.eq(account_id))
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "status->>'status' = 'candidate'",
                    )),
            )))
            .set((
                account_metadata::has_pending_candidate.eq(false),
                account_metadata::updated_at.eq(updated_at),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to clear pending-candidate flag: {e}"))?;

        Ok(())
    }

    /// First-writer-wins pause via `COALESCE` — re-pausing a paused
    /// account preserves the original `paused_at` and `paused_reason`.
    async fn set_pause(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        reason: &str,
    ) -> Result<PauseTransition, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        // Read existing state so the audit row can record before_state
        // accurately even on the idempotent retry path. A subsequent
        // UPDATE with COALESCE is the persistence transition.
        let before: MetadataRow = account_metadata::table
            .filter(account_metadata::account_id.eq(account_id))
            .select(MetadataRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| format!("Failed to load account_metadata: {e}"))?
            .ok_or_else(|| format!("Account not found: {account_id}"))?;
        let before_state = if before.paused_at.is_some() {
            AccountStatus::Paused
        } else {
            AccountStatus::Active
        };

        // First-writer-wins: write paused_at / paused_reason only when
        // the row is currently active. The WHERE clause encodes the
        // COALESCE semantics without an extra column read.
        if before_state == AccountStatus::Active {
            let rows_updated = diesel::update(account_metadata::table)
                .filter(account_metadata::account_id.eq(account_id))
                .filter(account_metadata::paused_at.is_null())
                .set((
                    account_metadata::paused_at.eq(Some(now)),
                    account_metadata::paused_reason.eq(Some(reason.to_string())),
                ))
                .execute(&mut conn)
                .await
                .map_err(|e| format!("Failed to set pause: {e}"))?;
            if rows_updated > 0 {
                Ok(PauseTransition {
                    before_state,
                    after_state: AccountStatus::Paused,
                    paused_at: Some(now),
                    paused_reason: Some(reason.to_string()),
                })
            } else {
                // Lost the race against a concurrent pause; return the
                // values actually persisted (first-writer-wins).
                let after: MetadataRow = account_metadata::table
                    .filter(account_metadata::account_id.eq(account_id))
                    .select(MetadataRow::as_select())
                    .first(&mut conn)
                    .await
                    .map_err(|e| format!("Failed to re-read account_metadata: {e}"))?;
                Ok(PauseTransition {
                    before_state: AccountStatus::Paused,
                    after_state: AccountStatus::Paused,
                    paused_at: after.paused_at,
                    paused_reason: after.paused_reason,
                })
            }
        } else {
            Ok(PauseTransition {
                before_state,
                after_state: AccountStatus::Paused,
                paused_at: before.paused_at,
                paused_reason: before.paused_reason,
            })
        }
    }

    async fn clear_pause(&self, account_id: &str) -> Result<PauseTransition, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let before: MetadataRow = account_metadata::table
            .filter(account_metadata::account_id.eq(account_id))
            .select(MetadataRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| format!("Failed to load account_metadata: {e}"))?
            .ok_or_else(|| format!("Account not found: {account_id}"))?;
        let before_state = if before.paused_at.is_some() {
            AccountStatus::Paused
        } else {
            AccountStatus::Active
        };

        diesel::update(account_metadata::table)
            .filter(account_metadata::account_id.eq(account_id))
            .set((
                account_metadata::paused_at.eq::<Option<DateTime<Utc>>>(None),
                account_metadata::paused_reason.eq::<Option<String>>(None),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to clear pause: {e}"))?;

        Ok(PauseTransition {
            before_state,
            after_state: AccountStatus::Active,
            paused_at: None,
            paused_reason: None,
        })
    }

    /// First-writer-wins release: the `released_at IS NULL` filter
    /// encodes the transition atomically; zero rows updated means the
    /// account was already released (or missing — disambiguated below).
    async fn set_released(&self, account_id: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows_updated = diesel::update(account_metadata::table)
            .filter(account_metadata::account_id.eq(account_id))
            .filter(account_metadata::released_at.is_null())
            .set(account_metadata::released_at.eq(Some(now)))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to set released: {e}"))?;

        if rows_updated > 0 {
            return Ok(true);
        }

        let exists: Option<MetadataRow> = account_metadata::table
            .filter(account_metadata::account_id.eq(account_id))
            .select(MetadataRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| format!("Failed to load account_metadata: {e}"))?;
        match exists {
            Some(_) => Ok(false),
            None => Err(format!("Account not found: {account_id}")),
        }
    }

    async fn clear_released(&self, account_id: &str) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows_updated = diesel::update(account_metadata::table)
            .filter(account_metadata::account_id.eq(account_id))
            .set(account_metadata::released_at.eq::<Option<DateTime<Utc>>>(None))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to clear released: {e}"))?;

        if rows_updated == 0 {
            return Err(format!("Account not found: {account_id}"));
        }
        Ok(())
    }

    async fn find_by_cosigner_commitment(&self, commitment: &str) -> Result<Vec<String>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        // The COALESCE expression must match the GIN index (see migration
        // 2026-05-05-000001_cosigner_commitment_index/up.sql) exactly so the
        // planner uses the index for `@>` containment lookups. EVM rows store
        // signers under `auth.EvmEcdsa.signers` (not `cosigner_commitments`)
        // and so coalesce to `'[]'::jsonb` — they contribute zero index entries
        // and never match.
        let rows: Vec<LookupAccountIdRow> = diesel::sql_query(
            "SELECT account_id FROM account_metadata \
             WHERE COALESCE( \
                 auth -> 'MidenFalconRpo' -> 'cosigner_commitments', \
                 auth -> 'MidenEcdsa'     -> 'cosigner_commitments', \
                 '[]'::jsonb \
             ) @> to_jsonb($1::text)",
        )
        .bind::<Text, _>(commitment)
        .load(&mut conn)
        .await
        .map_err(|e| format!("Failed to find by cosigner commitment: {e}"))?;

        Ok(rows.into_iter().map(|r| r.account_id).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::account_auth_state;
    use crate::storage::postgres::run_migrations;
    use std::sync::Arc;

    fn database_url() -> Option<String> {
        std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
    }

    fn pg_serial_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn insert_legacy_auth_state(
        conn: &mut diesel::PgConnection,
        account_id: &str,
        auth: serde_json::Value,
        last_auth_timestamp: i64,
    ) {
        diesel::RunQueryDsl::execute(
            diesel::sql_query(
                "INSERT INTO account_metadata \
                 (account_id, auth, network_config, created_at, updated_at, \
                  has_pending_candidate) \
                 VALUES ($1, $2, '{}'::jsonb, now(), now(), false)",
            )
            .bind::<Text, _>(account_id)
            .bind::<diesel::sql_types::Jsonb, _>(auth),
            conn,
        )
        .expect("insert legacy metadata row");
        diesel::RunQueryDsl::execute(
            diesel::sql_query(
                "INSERT INTO account_auth_state (account_id, last_auth_timestamp) \
                 VALUES ($1, $2)",
            )
            .bind::<Text, _>(account_id)
            .bind::<diesel::sql_types::BigInt, _>(last_auth_timestamp),
            conn,
        )
        .expect("insert account-scoped replay row");
    }

    async fn insert_account_row(store: &PostgresMetadataStore, account_id: &str) {
        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, \
                     '{\"MidenFalconRpo\":{\"cosigner_commitments\":[\
                        \"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\
                        \"0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"]}}'::jsonb, \
                     '{}'::jsonb, now(), now(), false)",
        )
        .bind::<Text, _>(account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata");
    }

    async fn stored_auth_timestamp(
        store: &PostgresMetadataStore,
        account_id: &str,
        signer_commitment: &str,
    ) -> i64 {
        let mut conn = store.pool.get().await.expect("conn");
        account_auth_state::table
            .filter(account_auth_state::account_id.eq(account_id))
            .filter(account_auth_state::signer_commitment.eq(signer_commitment))
            .select(account_auth_state::last_auth_timestamp)
            .first(&mut conn)
            .await
            .expect("stored auth timestamp")
    }

    async fn metadata_updated_at(store: &PostgresMetadataStore, account_id: &str) -> DateTime<Utc> {
        let mut conn = store.pool.get().await.expect("conn");
        account_metadata::table
            .filter(account_metadata::account_id.eq(account_id))
            .select(account_metadata::updated_at)
            .first(&mut conn)
            .await
            .expect("updated_at read")
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn cas_does_not_advance_metadata_updated_at() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");
        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        let account_id = format!("0xfrozen{}", Utc::now().timestamp_micros());
        insert_account_row(&store, &account_id).await;

        let before = metadata_updated_at(&store, &account_id).await;
        assert!(
            store
                .update_last_auth_timestamp_cas(&account_id, "0xaa", 100)
                .await
                .unwrap()
        );
        assert_eq!(
            metadata_updated_at(&store, &account_id).await,
            before,
            "authentication must not advance updated_at"
        );
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn cas_records_only_strictly_increasing_timestamps() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");
        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        let account_id = format!("0xcas{}", Utc::now().timestamp_micros());
        insert_account_row(&store, &account_id).await;

        assert!(
            store
                .update_last_auth_timestamp_cas(&account_id, "0xaa", 100)
                .await
                .unwrap(),
            "first timestamp creates the record"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas(&account_id, "0xaa", 100)
                .await
                .unwrap(),
            "equal timestamp is a replay"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas(&account_id, "0xaa", 99)
                .await
                .unwrap(),
            "older timestamp is a replay"
        );
        assert_eq!(
            stored_auth_timestamp(&store, &account_id, "0xaa").await,
            100,
            "rejected timestamps must not change the stored value"
        );
        assert!(
            store
                .update_last_auth_timestamp_cas(&account_id, "0xaa", 101)
                .await
                .unwrap()
        );
        assert_eq!(
            stored_auth_timestamp(&store, &account_id, "0xaa").await,
            101
        );
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn cas_is_scoped_per_signer_commitment() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");
        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        let account_id = format!("0xsigner{}", Utc::now().timestamp_micros());
        insert_account_row(&store, &account_id).await;

        assert!(
            store
                .update_last_auth_timestamp_cas(&account_id, "0xaa", 100)
                .await
                .unwrap()
        );
        assert!(
            store
                .update_last_auth_timestamp_cas(&account_id, "0xbb", 50)
                .await
                .unwrap(),
            "another signer's newer timestamp must not lock this signer out"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas(&account_id, "0xbb", 50)
                .await
                .unwrap(),
            "a replay from the same signer must still be rejected"
        );
        assert_eq!(
            stored_auth_timestamp(&store, &account_id, "0xaa").await,
            100
        );
        assert_eq!(stored_auth_timestamp(&store, &account_id, "0xbb").await, 50);
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn cas_for_unknown_account_is_a_storage_error_not_a_replay() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");
        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        let account_id = format!("0xghost{}", Utc::now().timestamp_micros());

        store
            .update_last_auth_timestamp_cas(&account_id, "0xaa", 100)
            .await
            .expect_err("unknown account must violate the foreign key, not record state");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn concurrent_identical_timestamps_admit_exactly_one_winner() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");
        let store = Arc::new(PostgresMetadataStore::new(&url, 8).await.expect("store"));
        let account_id = format!("0xrace{}", Utc::now().timestamp_micros());
        insert_account_row(&store, &account_id).await;

        for round in 0..4 {
            let timestamp = 1_000 + round;
            let attempts = (0..8).map(|_| {
                let store = store.clone();
                let account_id = account_id.clone();
                tokio::spawn(async move {
                    store
                        .update_last_auth_timestamp_cas(&account_id, "0xaa", timestamp)
                        .await
                })
            });
            let mut accepted = 0;
            for attempt in attempts {
                if attempt.await.expect("task").expect("cas") {
                    accepted += 1;
                }
            }
            assert_eq!(accepted, 1, "exactly one concurrent request may win");
        }
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL; reverts and re-applies the newest migration"]
    async fn migration_expands_account_scoped_replay_state_to_per_signer() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");

        let suffix = Utc::now().timestamp_micros();
        let falcon_account_id = format!("0xfalconbackfill{suffix}");
        let ecdsa_account_id = format!("0xecdsa_backfill{suffix}");
        let evm_account_id = format!("evm:1:0x{suffix:040x}");
        let falcon_signer_a = format!("0x{}", "aa".repeat(32));
        let falcon_signer_b = format!("0x{}", "bb".repeat(32));
        let ecdsa_signer = format!("0x{}", "cc".repeat(32));
        let evm_signer = format!("0x{}", "dd".repeat(20));
        {
            let url = url.clone();
            let falcon_account_id = falcon_account_id.clone();
            let ecdsa_account_id = ecdsa_account_id.clone();
            let evm_account_id = evm_account_id.clone();
            let falcon_signer_a = falcon_signer_a.clone();
            let falcon_signer_b = falcon_signer_b.clone();
            let ecdsa_signer = ecdsa_signer.clone();
            let evm_signer = evm_signer.clone();
            tokio::task::spawn_blocking(move || {
                use diesel::Connection;
                use diesel_migrations::MigrationHarness;
                let mut conn =
                    diesel::PgConnection::establish(&url).expect("sync connection for revert");
                conn.revert_last_migration(crate::storage::postgres::MIGRATIONS)
                    .expect("revert newest migration");
                insert_legacy_auth_state(
                    &mut conn,
                    &falcon_account_id,
                    serde_json::json!({
                        "MidenFalconRpo": {
                            "cosigner_commitments": [falcon_signer_a, falcon_signer_b]
                        }
                    }),
                    4242,
                );
                insert_legacy_auth_state(
                    &mut conn,
                    &ecdsa_account_id,
                    serde_json::json!({
                        "MidenEcdsa": { "cosigner_commitments": [ecdsa_signer] }
                    }),
                    3131,
                );
                insert_legacy_auth_state(
                    &mut conn,
                    &evm_account_id,
                    serde_json::json!({
                        "EvmEcdsa": { "signers": [evm_signer] }
                    }),
                    2020,
                );
            })
            .await
            .expect("blocking revert task");
        }

        run_migrations(&url).await.expect("migration reapplies");

        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        assert_eq!(
            stored_auth_timestamp(&store, &falcon_account_id, &falcon_signer_a).await,
            4242,
            "the account-scoped floor must be expanded to each authorized signer"
        );
        assert_eq!(
            stored_auth_timestamp(&store, &falcon_account_id, &falcon_signer_b).await,
            4242
        );
        assert_eq!(
            stored_auth_timestamp(&store, &ecdsa_account_id, &ecdsa_signer).await,
            3131,
            "Miden ECDSA replay state must use the same expansion"
        );
        assert_eq!(
            stored_auth_timestamp(&store, &evm_account_id, &evm_signer).await,
            2020,
            "EVM signer sets live under a different JSON key and address width"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas(&falcon_account_id, &falcon_signer_a, 4242)
                .await
                .unwrap(),
            "expanded timestamp must be enforced"
        );
        assert!(
            store
                .update_last_auth_timestamp_cas(&falcon_account_id, &falcon_signer_a, 4243)
                .await
                .unwrap()
        );
        assert!(
            store
                .update_last_auth_timestamp_cas(&falcon_account_id, &falcon_signer_b, 4243)
                .await
                .unwrap(),
            "signers must not contend after the expansion"
        );
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL; reverts and re-applies the newest migration"]
    async fn migration_rejects_non_canonical_signer_metadata() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        let _guard = pg_serial_lock().lock().await;
        run_migrations(&url).await.expect("migrations apply");

        let suffix = Utc::now().timestamp_micros();
        let coerced_object_account_id = format!("0xcoercedbackfill{suffix}");
        let short_commitment_account_id = format!("0xshortbackfill{suffix}");
        let empty_signers_account_id = format!("0xemptybackfill{suffix}");
        let malformed_accounts = [
            (
                coerced_object_account_id.clone(),
                serde_json::json!({
                    "MidenFalconRpo": { "cosigner_commitments": [{ "invalid": true }] }
                }),
            ),
            (
                short_commitment_account_id.clone(),
                serde_json::json!({
                    "MidenFalconRpo": {
                        "cosigner_commitments": [format!("0x{}", "aa".repeat(20))]
                    }
                }),
            ),
            (
                empty_signers_account_id.clone(),
                serde_json::json!({ "EvmEcdsa": { "signers": [] } }),
            ),
        ];
        {
            let url = url.clone();
            let malformed_accounts = malformed_accounts.clone();
            tokio::task::spawn_blocking(move || {
                use diesel::Connection;
                use diesel_migrations::MigrationHarness;
                let mut conn =
                    diesel::PgConnection::establish(&url).expect("sync connection for revert");
                conn.revert_last_migration(crate::storage::postgres::MIGRATIONS)
                    .expect("revert newest migration");
                for (account_id, auth) in &malformed_accounts {
                    insert_legacy_auth_state(&mut conn, account_id, auth.clone(), 4242);
                }
            })
            .await
            .expect("blocking revert task");
        }

        let migration_error = run_migrations(&url)
            .await
            .expect_err("malformed signer metadata must fail closed");

        {
            let url = url.clone();
            let malformed_accounts = malformed_accounts.clone();
            tokio::task::spawn_blocking(move || {
                use diesel::Connection;
                let mut conn =
                    diesel::PgConnection::establish(&url).expect("sync connection for cleanup");
                for (account_id, _) in &malformed_accounts {
                    diesel::RunQueryDsl::execute(
                        diesel::sql_query("DELETE FROM account_metadata WHERE account_id = $1")
                            .bind::<Text, _>(account_id),
                        &mut conn,
                    )
                    .expect("remove malformed legacy row");
                }
            })
            .await
            .expect("blocking cleanup task");
        }
        run_migrations(&url)
            .await
            .expect("migration applies after malformed rows are removed");

        assert!(
            migration_error.contains("canonical"),
            "migration error should explain the abort: {migration_error}"
        );
        for (account_id, _) in &malformed_accounts {
            assert!(
                migration_error.contains(account_id),
                "migration error should identify {account_id}: {migration_error}"
            );
        }
    }

    async fn flag(store: &PostgresMetadataStore, account_id: &str) -> bool {
        let mut conn = store.pool.get().await.expect("conn");
        account_metadata::table
            .filter(account_metadata::account_id.eq(account_id))
            .select(account_metadata::has_pending_candidate)
            .first(&mut conn)
            .await
            .expect("flag read")
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn generic_set_preserves_pending_candidate_flag() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        run_migrations(&url).await.expect("migrations apply");
        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        let account_id = format!("0xsetrace{}", Utc::now().timestamp_micros());
        let now = Utc::now().to_rfc3339();

        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), true)",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata");
        drop(conn);

        let stale_metadata = AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: now.clone(),
            updated_at: now,
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        store
            .set(stale_metadata)
            .await
            .expect("stale metadata upsert");

        assert!(
            flag(&store, &account_id).await,
            "generic metadata upsert must not clear candidate ownership",
        );

        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query("DELETE FROM account_metadata WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .expect("cleanup");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn clear_pending_candidate_is_conditional_on_candidate_rows() {
        let url = database_url().expect("DATABASE_URL must be set for this #[ignore] test");
        run_migrations(&url).await.expect("migrations apply");
        let store = PostgresMetadataStore::new(&url, 2).await.expect("store");
        let account_id = format!("0xwedge{}", Utc::now().timestamp_micros());
        let now = Utc::now().to_rfc3339();

        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), true)",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata");
        diesel::sql_query(
            "INSERT INTO deltas \
             (account_id, nonce, prev_commitment, delta_payload, status, status_kind, status_timestamp) \
             VALUES ($1, 1, '0x0', '{}'::jsonb, '{\"status\":\"candidate\"}'::jsonb, 'candidate', now())",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert candidate delta");
        drop(conn);

        // Candidate row present (as after a racing submission): the clear must
        // be a no-op.
        store
            .clear_pending_candidate_if_none(&account_id, &now)
            .await
            .expect("guarded clear");
        assert!(
            flag(&store, &account_id).await,
            "flag must stay set while a candidate-status delta exists",
        );

        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query("DELETE FROM deltas WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .expect("remove candidate");
        drop(conn);

        store
            .clear_pending_candidate_if_none(&account_id, &now)
            .await
            .expect("guarded clear with no candidates");
        assert!(
            !flag(&store, &account_id).await,
            "flag must clear once no candidate-status delta remains",
        );

        let mut conn = store.pool.get().await.expect("conn");
        diesel::sql_query("DELETE FROM account_metadata WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .expect("cleanup");
    }
}
