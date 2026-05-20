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
    last_auth_timestamp: Option<i64>,
    paused_at: Option<chrono::DateTime<chrono::Utc>>,
    paused_reason: Option<String>,
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
    last_auth_timestamp: Option<i64>,
    paused_at: Option<chrono::DateTime<chrono::Utc>>,
    paused_reason: Option<String>,
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
            last_auth_timestamp: row.last_auth_timestamp,
            paused_at: row.paused_at,
            paused_reason: row.paused_reason,
        })
    }
}

#[async_trait]
impl MetadataStore for PostgresMetadataStore {
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
            last_auth_timestamp: metadata.last_auth_timestamp,
            paused_at: metadata.paused_at,
            paused_reason: metadata.paused_reason.clone(),
        };

        diesel::insert_into(account_metadata::table)
            .values(&new_metadata)
            .on_conflict(account_metadata::account_id)
            .do_update()
            .set((
                account_metadata::auth.eq(&auth_json),
                account_metadata::network_config.eq(&network_config_json),
                account_metadata::updated_at.eq(updated_at),
                account_metadata::has_pending_candidate.eq(metadata.has_pending_candidate),
                account_metadata::last_auth_timestamp.eq(metadata.last_auth_timestamp),
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
            // `is_not_null` hits the partial index `idx_account_metadata_paused`
            // (migration 2026-05-19-000001_account_pause_fields).
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

    /// Atomically update last_auth_timestamp using compare-and-swap.
    async fn update_last_auth_timestamp_cas(
        &self,
        account_id: &str,
        new_timestamp: i64,
        now: &str,
    ) -> Result<bool, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let updated_at: chrono::DateTime<chrono::Utc> = now
            .parse()
            .map_err(|e| format!("Failed to parse timestamp: {e}"))?;

        // Atomic CAS: only update if new_timestamp > current (or current is NULL)
        let rows_updated = diesel::update(account_metadata::table)
            .filter(account_metadata::account_id.eq(account_id))
            .filter(
                account_metadata::last_auth_timestamp
                    .is_null()
                    .or(account_metadata::last_auth_timestamp.lt(new_timestamp)),
            )
            .set((
                account_metadata::last_auth_timestamp.eq(Some(new_timestamp)),
                account_metadata::updated_at.eq(updated_at),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to update last_auth_timestamp: {e}"))?;

        Ok(rows_updated > 0)
    }

    /// First-writer-wins pause via `COALESCE` — re-pausing a paused
    /// account preserves the original `paused_at` and `paused_reason`
    /// (feature 001-account-pausing FR-013).
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
            diesel::update(account_metadata::table)
                .filter(account_metadata::account_id.eq(account_id))
                .filter(account_metadata::paused_at.is_null())
                .set((
                    account_metadata::paused_at.eq(Some(now)),
                    account_metadata::paused_reason.eq(Some(reason.to_string())),
                ))
                .execute(&mut conn)
                .await
                .map_err(|e| format!("Failed to set pause: {e}"))?;
            Ok(PauseTransition {
                before_state,
                after_state: AccountStatus::Paused,
                paused_at: Some(now),
                paused_reason: Some(reason.to_string()),
            })
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
