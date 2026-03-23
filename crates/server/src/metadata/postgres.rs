use crate::metadata::{AccountMetadata, Auth, MetadataStore};
use crate::schema::account_metadata;
use async_trait::async_trait;
use diesel::prelude::*;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

pub struct PostgresMetadataStore {
    pool: Pool<AsyncPgConnection>,
}

impl PostgresMetadataStore {
    pub async fn new(database_url: &str, pool_max_size: usize) -> Result<Self, String> {
        let config = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
        let pool = Pool::builder(config)
            .max_size(pool_max_size)
            .build()
            .map_err(|e| format!("Failed to create connection pool: {e}"))?;

        // Test connection
        let _ = pool
            .get()
            .await
            .map_err(|e| format!("Failed to connect to Postgres: {e}"))?;

        Ok(Self { pool })
    }

    pub async fn with_pool(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }
}

// Queryable struct for reading from database
#[derive(Queryable, Selectable)]
#[diesel(table_name = account_metadata)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct MetadataRow {
    account_id: String,
    auth: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    has_pending_candidate: bool,
    last_auth_timestamp: Option<i64>,
}

// Insertable struct for writing to database
#[derive(Insertable, AsChangeset)]
#[diesel(table_name = account_metadata)]
struct NewMetadata<'a> {
    account_id: &'a str,
    auth: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    has_pending_candidate: bool,
    last_auth_timestamp: Option<i64>,
}

impl TryFrom<MetadataRow> for AccountMetadata {
    type Error = String;

    fn try_from(row: MetadataRow) -> Result<Self, Self::Error> {
        let auth: Auth =
            serde_json::from_value(row.auth).map_err(|e| format!("Failed to parse auth: {e}"))?;

        Ok(AccountMetadata {
            account_id: row.account_id,
            auth,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            has_pending_candidate: row.has_pending_candidate,
            last_auth_timestamp: row.last_auth_timestamp,
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

        let new_metadata = NewMetadata {
            account_id: &metadata.account_id,
            auth: auth_json.clone(),
            created_at,
            updated_at,
            has_pending_candidate: metadata.has_pending_candidate,
            last_auth_timestamp: metadata.last_auth_timestamp,
        };

        diesel::insert_into(account_metadata::table)
            .values(&new_metadata)
            .on_conflict(account_metadata::account_id)
            .do_update()
            .set((
                account_metadata::auth.eq(&auth_json),
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
}
