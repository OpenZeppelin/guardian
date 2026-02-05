use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::schema::{delta_proposals, deltas, states};
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use async_trait::async_trait;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// Run database migrations. Call once at application startup.
pub async fn run_migrations(database_url: &str) -> Result<(), String> {
    let url = database_url.to_string();
    tokio::task::spawn_blocking(move || {
        let mut conn = PgConnection::establish(&url)
            .map_err(|e| format!("Failed to connect for migrations: {e}"))?;

        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|e| format!("Failed to run migrations: {e}"))?;

        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("Migration task failed: {e}"))??;

    Ok(())
}

pub struct PostgresService {
    pool: Pool<AsyncPgConnection>,
}

impl PostgresService {
    pub async fn new(database_url: &str) -> Result<Self, String> {
        let config = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
        let pool = Pool::builder(config)
            .max_size(16)
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

// Queryable structs for reading from database
#[derive(Queryable, Selectable)]
#[diesel(table_name = states)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct StateRow {
    account_id: String,
    state_json: serde_json::Value,
    commitment: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = deltas)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct DeltaRow {
    #[allow(dead_code)]
    id: i64,
    account_id: String,
    nonce: i64,
    prev_commitment: String,
    new_commitment: Option<String>,
    delta_payload: serde_json::Value,
    ack_sig: Option<String>,
    status: serde_json::Value,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = delta_proposals)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct ProposalRow {
    #[allow(dead_code)]
    id: i64,
    account_id: String,
    #[allow(dead_code)]
    commitment: String,
    nonce: i64,
    prev_commitment: String,
    new_commitment: Option<String>,
    delta_payload: serde_json::Value,
    ack_sig: Option<String>,
    status: serde_json::Value,
}

// Insertable structs for writing to database
#[derive(Insertable)]
#[diesel(table_name = states)]
struct NewState<'a> {
    account_id: &'a str,
    state_json: &'a serde_json::Value,
    commitment: &'a str,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = deltas)]
struct NewDelta<'a> {
    account_id: &'a str,
    nonce: i64,
    prev_commitment: &'a str,
    new_commitment: Option<&'a str>,
    delta_payload: &'a serde_json::Value,
    ack_sig: Option<&'a str>,
    status: serde_json::Value,
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = delta_proposals)]
struct NewProposal<'a> {
    account_id: &'a str,
    commitment: &'a str,
    nonce: i64,
    prev_commitment: &'a str,
    new_commitment: Option<&'a str>,
    delta_payload: &'a serde_json::Value,
    ack_sig: Option<&'a str>,
    status: serde_json::Value,
}

impl From<StateRow> for StateObject {
    fn from(row: StateRow) -> Self {
        StateObject {
            account_id: row.account_id,
            state_json: row.state_json,
            commitment: row.commitment,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            auth_scheme: String::new(),
        }
    }
}

impl From<DeltaRow> for DeltaObject {
    fn from(row: DeltaRow) -> Self {
        let status: DeltaStatus =
            serde_json::from_value(row.status).unwrap_or_else(|_| DeltaStatus::default());
        DeltaObject {
            account_id: row.account_id,
            nonce: row.nonce as u64,
            prev_commitment: row.prev_commitment,
            new_commitment: row.new_commitment,
            delta_payload: row.delta_payload,
            ack_sig: row.ack_sig.unwrap_or_default(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status,
        }
    }
}

impl From<ProposalRow> for DeltaObject {
    fn from(row: ProposalRow) -> Self {
        let status: DeltaStatus =
            serde_json::from_value(row.status).unwrap_or_else(|_| DeltaStatus::default());
        DeltaObject {
            account_id: row.account_id,
            nonce: row.nonce as u64,
            prev_commitment: row.prev_commitment,
            new_commitment: row.new_commitment,
            delta_payload: row.delta_payload,
            ack_sig: row.ack_sig.unwrap_or_default(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status,
        }
    }
}

#[async_trait]
impl StorageBackend for PostgresService {
    async fn submit_state(&self, state: &StateObject) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let created_at: chrono::DateTime<chrono::Utc> = state
            .created_at
            .parse()
            .map_err(|e| format!("Failed to parse created_at: {e}"))?;
        let updated_at: chrono::DateTime<chrono::Utc> = state
            .updated_at
            .parse()
            .map_err(|e| format!("Failed to parse updated_at: {e}"))?;

        let new_state = NewState {
            account_id: &state.account_id,
            state_json: &state.state_json,
            commitment: &state.commitment,
            created_at,
            updated_at,
        };

        diesel::insert_into(states::table)
            .values(&new_state)
            .on_conflict(states::account_id)
            .do_update()
            .set((
                states::state_json.eq(&state.state_json),
                states::commitment.eq(&state.commitment),
                states::updated_at.eq(updated_at),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to submit state: {e}"))?;

        Ok(())
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let status_json = serde_json::to_value(&delta.status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;

        let new_delta = NewDelta {
            account_id: &delta.account_id,
            nonce: delta.nonce as i64,
            prev_commitment: &delta.prev_commitment,
            new_commitment: delta.new_commitment.as_deref(),
            delta_payload: &delta.delta_payload,
            ack_sig: Some(delta.ack_sig.as_str()),
            status: status_json.clone(),
        };

        diesel::insert_into(deltas::table)
            .values(&new_delta)
            .on_conflict((deltas::account_id, deltas::nonce))
            .do_update()
            .set((
                deltas::prev_commitment.eq(&delta.prev_commitment),
                deltas::new_commitment.eq(&delta.new_commitment),
                deltas::delta_payload.eq(&delta.delta_payload),
                deltas::ack_sig.eq(Some(&delta.ack_sig)),
                deltas::status.eq(&status_json),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to submit delta: {e}"))?;

        Ok(())
    }

    async fn pull_state(&self, account_id: &str) -> Result<StateObject, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let row: StateRow = states::table
            .filter(states::account_id.eq(account_id))
            .select(StateRow::as_select())
            .first(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull state: {e}"))?;

        Ok(row.into())
    }

    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let row: DeltaRow = deltas::table
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::nonce.eq(nonce as i64))
            .select(DeltaRow::as_select())
            .first(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull delta: {e}"))?;

        Ok(row.into())
    }

    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<DeltaRow> = deltas::table
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::nonce.ge(from_nonce as i64))
            .order(deltas::nonce.asc())
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull deltas: {e}"))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn has_pending_candidate(&self, account_id: &str) -> Result<bool, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        // Query for any delta with candidate status
        let count: i64 = deltas::table
            .filter(deltas::account_id.eq(account_id))
            .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                "status->>'status' = 'candidate'",
            ))
            .count()
            .get_result(&mut conn)
            .await
            .map_err(|e| format!("Failed to check pending candidate: {e}"))?;

        Ok(count > 0)
    }

    async fn pull_canonical_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<DeltaRow> = deltas::table
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::nonce.ge(from_nonce as i64))
            .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                "status->>'status' = 'canonical'",
            ))
            .order(deltas::nonce.asc())
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull canonical deltas: {e}"))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let status_json = serde_json::to_value(&proposal.status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;

        let new_proposal = NewProposal {
            account_id: &proposal.account_id,
            commitment,
            nonce: proposal.nonce as i64,
            prev_commitment: &proposal.prev_commitment,
            new_commitment: proposal.new_commitment.as_deref(),
            delta_payload: &proposal.delta_payload,
            ack_sig: Some(proposal.ack_sig.as_str()),
            status: status_json,
        };

        diesel::insert_into(delta_proposals::table)
            .values(&new_proposal)
            .on_conflict((delta_proposals::account_id, delta_proposals::commitment))
            .do_nothing()
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to submit delta proposal: {e}"))?;

        Ok(())
    }

    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let row: ProposalRow = delta_proposals::table
            .filter(delta_proposals::account_id.eq(account_id))
            .filter(delta_proposals::commitment.eq(commitment))
            .select(ProposalRow::as_select())
            .first(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull delta proposal: {e}"))?;

        Ok(row.into())
    }

    async fn pull_all_delta_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<ProposalRow> = delta_proposals::table
            .filter(delta_proposals::account_id.eq(account_id))
            .order(delta_proposals::nonce.asc())
            .select(ProposalRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull all delta proposals: {e}"))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn pull_pending_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<ProposalRow> = delta_proposals::table
            .filter(delta_proposals::account_id.eq(account_id))
            .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                "status->>'status' = 'pending'",
            ))
            .order(delta_proposals::nonce.asc())
            .select(ProposalRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull pending proposals: {e}"))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let status_json = serde_json::to_value(&proposal.status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;

        diesel::update(delta_proposals::table)
            .filter(delta_proposals::account_id.eq(&proposal.account_id))
            .filter(delta_proposals::commitment.eq(commitment))
            .set((
                delta_proposals::nonce.eq(proposal.nonce as i64),
                delta_proposals::prev_commitment.eq(&proposal.prev_commitment),
                delta_proposals::new_commitment.eq(&proposal.new_commitment),
                delta_proposals::delta_payload.eq(&proposal.delta_payload),
                delta_proposals::ack_sig.eq(Some(&proposal.ack_sig)),
                delta_proposals::status.eq(&status_json),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to update delta proposal: {e}"))?;

        Ok(())
    }

    async fn delete_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        diesel::delete(delta_proposals::table)
            .filter(delta_proposals::account_id.eq(account_id))
            .filter(delta_proposals::commitment.eq(commitment))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to delete delta proposal: {e}"))?;

        Ok(())
    }

    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        diesel::delete(deltas::table)
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::nonce.eq(nonce as i64))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to delete delta: {e}"))?;

        Ok(())
    }

    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let status_json = serde_json::to_value(&status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;

        diesel::update(deltas::table)
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::nonce.eq(nonce as i64))
            .set(deltas::status.eq(&status_json))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to update delta status: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_delta(account_id: &str, nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: Some("0x456".to_string()),
            delta_payload: serde_json::json!({"test": "payload"}),
            ack_sig: "0xsig".to_string(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Canonical {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
            },
        }
    }

    fn create_test_state(account_id: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            commitment: "0x789".to_string(),
            state_json: serde_json::json!({"test": "state"}),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    #[test]
    fn test_create_test_delta() {
        let delta = create_test_delta("0x123", 1);
        assert_eq!(delta.account_id, "0x123");
        assert_eq!(delta.nonce, 1);
    }

    #[test]
    fn test_create_test_state() {
        let state = create_test_state("0x123");
        assert_eq!(state.account_id, "0x123");
    }
}
