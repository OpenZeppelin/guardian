//! The module tables on Postgres.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::QueryableByName;
use diesel::sql_types::{BigInt, Bool, Integer, Jsonb, Nullable, Text, Timestamptz};
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};

use super::store::{
    AccountParamsRow, Assigned, Change, HistoryEntry, InstanceRow, ModuleStore, ParamsKind,
    assignment_changes,
};

pub struct PgModuleStore {
    pool: Pool<AsyncPgConnection>,
}

impl PgModuleStore {
    pub fn new(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }

    async fn conn(
        &self,
    ) -> Result<diesel_async::pooled_connection::deadpool::Object<AsyncPgConnection>, String> {
        self.pool
            .get()
            .await
            .map_err(|e| format!("module store pool: {e}"))
    }
}

fn at(change: &Change<'_>) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(&change.at)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|e| format!("invalid change time: {e}"))
}

#[derive(QueryableByName)]
struct InstanceSql {
    #[diesel(sql_type = Text)]
    module_id: String,
    #[diesel(sql_type = Bool)]
    enabled: bool,
    #[diesel(sql_type = Jsonb)]
    global_params: serde_json::Value,
    #[diesel(sql_type = Jsonb)]
    seeded_params: serde_json::Value,
    #[diesel(sql_type = Timestamptz)]
    updated_at: DateTime<Utc>,
    #[diesel(sql_type = Text)]
    updated_by: String,
}

#[derive(QueryableByName)]
struct AccountParamsSql {
    #[diesel(sql_type = Text)]
    account_id: String,
    #[diesel(sql_type = Text)]
    module_id: String,
    #[diesel(sql_type = Text)]
    kind: String,
    #[diesel(sql_type = Integer)]
    position: i32,
    #[diesel(sql_type = Jsonb)]
    params: serde_json::Value,
    #[diesel(sql_type = Timestamptz)]
    updated_at: DateTime<Utc>,
    #[diesel(sql_type = Text)]
    updated_by: String,
}

#[derive(QueryableByName)]
struct HistorySql {
    #[diesel(sql_type = BigInt)]
    version: i64,
    #[diesel(sql_type = Text)]
    module_id: String,
    #[diesel(sql_type = Nullable<Text>)]
    account_id: Option<String>,
    #[diesel(sql_type = Jsonb)]
    previous_params: serde_json::Value,
    #[diesel(sql_type = Jsonb)]
    new_params: serde_json::Value,
    #[diesel(sql_type = Text)]
    operator: String,
    #[diesel(sql_type = Text)]
    reason: String,
    #[diesel(sql_type = Timestamptz)]
    changed_at: DateTime<Utc>,
}

#[derive(QueryableByName)]
struct VersionSql {
    #[diesel(sql_type = BigInt)]
    version: i64,
}

#[derive(QueryableByName)]
struct ParamsSql {
    #[diesel(sql_type = Jsonb)]
    params: serde_json::Value,
}

/// Takes the module write lock and returns the version this write commits as.
async fn bump(conn: &mut AsyncPgConnection) -> Result<i64, diesel::result::Error> {
    let rows: Vec<VersionSql> = diesel::sql_query(
        "UPDATE module_state_version SET version = version + 1 RETURNING version",
    )
    .load(conn)
    .await?;
    rows.into_iter()
        .next()
        .map(|row| row.version)
        .ok_or(diesel::result::Error::NotFound)
}

#[allow(clippy::too_many_arguments)]
async fn append(
    conn: &mut AsyncPgConnection,
    version: i64,
    module_id: &str,
    account_id: Option<&str>,
    previous: &serde_json::Value,
    new: &serde_json::Value,
    change: &Change<'_>,
    changed_at: DateTime<Utc>,
) -> Result<usize, diesel::result::Error> {
    diesel::sql_query(
        "INSERT INTO module_params_history \
         (version, module_id, account_id, previous_params, new_params, operator, reason, \
         changed_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind::<BigInt, _>(version)
    .bind::<Text, _>(module_id)
    .bind::<Nullable<Text>, _>(account_id)
    .bind::<Jsonb, _>(previous)
    .bind::<Jsonb, _>(new)
    .bind::<Text, _>(change.operator)
    .bind::<Text, _>(change.reason)
    .bind::<Timestamptz, _>(changed_at)
    .execute(conn)
    .await
}

#[async_trait]
impl ModuleStore for PgModuleStore {
    async fn seed_instance(
        &self,
        module_id: &str,
        enabled: bool,
        params: &serde_json::Value,
        at_text: &str,
    ) -> Result<(), String> {
        let changed_at = DateTime::parse_from_rfc3339(at_text)
            .map(|at| at.with_timezone(&Utc))
            .map_err(|e| format!("invalid seed time: {e}"))?;
        let mut conn = self.conn().await?;
        let (module_id, params) = (module_id.to_string(), params.clone());
        let seeded = conn
            .transaction::<(), diesel::result::Error, _>(|conn| {
                async move {
                    let version = bump(conn).await?;
                    let inserted = diesel::sql_query(
                        "INSERT INTO module_instances \
                     (module_id, enabled, global_params, seeded_params, updated_at, updated_by) \
                     VALUES ($1, $2, $3, $3, $4, 'catalog') ON CONFLICT (module_id) DO NOTHING",
                    )
                    .bind::<Text, _>(&module_id)
                    .bind::<Bool, _>(enabled)
                    .bind::<Jsonb, _>(&params)
                    .bind::<Timestamptz, _>(changed_at)
                    .execute(conn)
                    .await?;
                    if inserted == 0 {
                        return Err(diesel::result::Error::RollbackTransaction);
                    }
                    let change = Change {
                        operator: "catalog",
                        reason: "seeded from the catalog file",
                        at: String::new(),
                    };
                    append(
                        conn,
                        version,
                        &module_id,
                        None,
                        &serde_json::Value::Null,
                        &params,
                        &change,
                        changed_at,
                    )
                    .await?;
                    Ok(())
                }
                .scope_boxed()
            })
            .await;
        match seeded {
            Ok(()) | Err(diesel::result::Error::RollbackTransaction) => Ok(()),
            Err(e) => Err(format!("seed module {e}")),
        }
    }

    async fn instances(&self) -> Result<Vec<InstanceRow>, String> {
        let mut conn = self.conn().await?;
        let rows: Vec<InstanceSql> = diesel::sql_query(
            "SELECT module_id, enabled, global_params, seeded_params, updated_at, updated_by \
             FROM module_instances ORDER BY module_id",
        )
        .load(&mut conn)
        .await
        .map_err(|e| format!("module instances: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|row| InstanceRow {
                module_id: row.module_id,
                enabled: row.enabled,
                global_params: row.global_params,
                seeded_params: row.seeded_params,
                updated_at: row.updated_at.to_rfc3339(),
                updated_by: row.updated_by,
            })
            .collect())
    }

    async fn set_enabled(
        &self,
        module_id: &str,
        enabled: bool,
        change: &Change<'_>,
    ) -> Result<(), String> {
        #[derive(QueryableByName)]
        struct EnabledSql {
            #[diesel(sql_type = Bool)]
            enabled: bool,
        }
        let changed_at = at(change)?;
        let mut conn = self.conn().await?;
        let module_id = module_id.to_string();
        conn.transaction::<(), diesel::result::Error, _>(|conn| {
            async move {
                let version = bump(conn).await?;
                let previous: Vec<EnabledSql> = diesel::sql_query(
                    "SELECT enabled FROM module_instances WHERE module_id = $1 FOR UPDATE",
                )
                .bind::<Text, _>(&module_id)
                .load(conn)
                .await?;
                let previous = previous
                    .into_iter()
                    .next()
                    .ok_or(diesel::result::Error::NotFound)?
                    .enabled;
                diesel::sql_query(
                    "UPDATE module_instances SET enabled = $2, updated_at = $3, updated_by = $4 \
                     WHERE module_id = $1",
                )
                .bind::<Text, _>(&module_id)
                .bind::<Bool, _>(enabled)
                .bind::<Timestamptz, _>(changed_at)
                .bind::<Text, _>(change.operator)
                .execute(conn)
                .await?;
                append(
                    conn,
                    version,
                    &module_id,
                    None,
                    &serde_json::json!({ "enabled": previous }),
                    &serde_json::json!({ "enabled": enabled }),
                    change,
                    changed_at,
                )
                .await?;
                Ok(())
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("set enabled: {e}"))
    }

    async fn set_global(
        &self,
        module_id: &str,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<(), String> {
        let changed_at = at(change)?;
        let mut conn = self.conn().await?;
        let module_id = module_id.to_string();
        conn.transaction::<(), diesel::result::Error, _>(|conn| {
            async move {
                let version = bump(conn).await?;
                let previous: Vec<ParamsSql> = diesel::sql_query(
                    "SELECT global_params AS params FROM module_instances \
                     WHERE module_id = $1 FOR UPDATE",
                )
                .bind::<Text, _>(&module_id)
                .load(conn)
                .await?;
                let previous = previous
                    .into_iter()
                    .next()
                    .map(|row| row.params)
                    .unwrap_or_default();
                diesel::sql_query(
                    "INSERT INTO module_instances \
                     (module_id, enabled, global_params, seeded_params, updated_at, updated_by) \
                     VALUES ($1, TRUE, $2, $2, $3, $4) ON CONFLICT (module_id) DO UPDATE SET \
                     global_params = EXCLUDED.global_params, updated_at = EXCLUDED.updated_at, \
                     updated_by = EXCLUDED.updated_by",
                )
                .bind::<Text, _>(&module_id)
                .bind::<Jsonb, _>(&params)
                .bind::<Timestamptz, _>(changed_at)
                .bind::<Text, _>(change.operator)
                .execute(conn)
                .await?;
                append(
                    conn, version, &module_id, None, &previous, &params, change, changed_at,
                )
                .await?;
                Ok(())
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("set global params: {e}"))
    }

    async fn set_global_if(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<bool, String> {
        let changed_at = at(change)?;
        let mut conn = self.conn().await?;
        let (module_id, expected) = (module_id.to_string(), expected.clone());
        let written = conn
            .transaction::<(), diesel::result::Error, _>(|conn| {
                async move {
                    let version = bump(conn).await?;
                    let current: Vec<ParamsSql> = diesel::sql_query(
                        "SELECT global_params AS params FROM module_instances \
                         WHERE module_id = $1 FOR UPDATE",
                    )
                    .bind::<Text, _>(&module_id)
                    .load(conn)
                    .await?;
                    let Some(previous) = current.into_iter().next().map(|row| row.params) else {
                        return Err(diesel::result::Error::RollbackTransaction);
                    };
                    if previous != expected {
                        return Err(diesel::result::Error::RollbackTransaction);
                    }
                    diesel::sql_query(
                        "UPDATE module_instances SET global_params = $2, updated_at = $3, \
                         updated_by = $4 WHERE module_id = $1",
                    )
                    .bind::<Text, _>(&module_id)
                    .bind::<Jsonb, _>(&params)
                    .bind::<Timestamptz, _>(changed_at)
                    .bind::<Text, _>(change.operator)
                    .execute(conn)
                    .await?;
                    append(
                        conn, version, &module_id, None, &previous, &params, change, changed_at,
                    )
                    .await?;
                    Ok(())
                }
                .scope_boxed()
            })
            .await;
        match written {
            Ok(()) => Ok(true),
            Err(diesel::result::Error::RollbackTransaction) => Ok(false),
            Err(e) => Err(format!("set global params: {e}")),
        }
    }

    async fn account_params(&self, account_id: &str) -> Result<Vec<AccountParamsRow>, String> {
        let mut conn = self.conn().await?;
        let rows: Vec<AccountParamsSql> = diesel::sql_query(
            "SELECT account_id, module_id, kind, position, params, updated_at, updated_by \
             FROM module_account_params WHERE account_id = $1 ORDER BY position, module_id",
        )
        .bind::<Text, _>(account_id)
        .load(&mut conn)
        .await
        .map_err(|e| format!("account module params: {e}"))?;
        rows.into_iter()
            .map(|row| {
                Ok(AccountParamsRow {
                    account_id: row.account_id,
                    module_id: row.module_id,
                    kind: ParamsKind::parse(&row.kind)?,
                    position: row.position,
                    params: row.params,
                    updated_at: row.updated_at.to_rfc3339(),
                    updated_by: row.updated_by,
                })
            })
            .collect()
    }

    async fn set_layer(
        &self,
        account_id: &str,
        module_id: &str,
        params: Option<serde_json::Value>,
        change: &Change<'_>,
    ) -> Result<(), String> {
        let changed_at = at(change)?;
        let mut conn = self.conn().await?;
        let (account_id, module_id) = (account_id.to_string(), module_id.to_string());
        conn.transaction::<(), diesel::result::Error, _>(|conn| {
            async move {
                let version = bump(conn).await?;
                let previous: Vec<ParamsSql> = diesel::sql_query(
                    "DELETE FROM module_account_params WHERE account_id = $1 AND module_id = $2 \
                     AND kind = 'layer' RETURNING params",
                )
                .bind::<Text, _>(&account_id)
                .bind::<Text, _>(&module_id)
                .load(conn)
                .await?;
                let previous = previous
                    .into_iter()
                    .next()
                    .map(|row| row.params)
                    .unwrap_or_default();
                if let Some(params) = &params {
                    diesel::sql_query(
                        "INSERT INTO module_account_params \
                         (account_id, module_id, kind, position, params, updated_at, updated_by) \
                         VALUES ($1, $2, 'layer', 0, $3, $4, $5)",
                    )
                    .bind::<Text, _>(&account_id)
                    .bind::<Text, _>(&module_id)
                    .bind::<Jsonb, _>(params)
                    .bind::<Timestamptz, _>(changed_at)
                    .bind::<Text, _>(change.operator)
                    .execute(conn)
                    .await?;
                }
                let new = params.unwrap_or_default();
                append(
                    conn,
                    version,
                    &module_id,
                    Some(&account_id),
                    &previous,
                    &new,
                    change,
                    changed_at,
                )
                .await?;
                Ok(())
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("set account layer: {e}"))
    }

    async fn set_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<(), String> {
        let changed_at = at(change)?;
        let mut conn = self.conn().await?;
        let account_id = account_id.to_string();
        conn.transaction::<(), diesel::result::Error, _>(|conn| {
            async move {
                let version = bump(conn).await?;
                let removed: Vec<AccountParamsSql> = diesel::sql_query(
                    "DELETE FROM module_account_params WHERE account_id = $1 \
                     AND kind = 'assignment' RETURNING account_id, module_id, kind, position, \
                     params, updated_at, updated_by",
                )
                .bind::<Text, _>(&account_id)
                .load(conn)
                .await?;
                let previous = removed
                    .into_iter()
                    .map(|row| AccountParamsRow {
                        account_id: row.account_id,
                        module_id: row.module_id,
                        kind: ParamsKind::Assignment,
                        position: row.position,
                        params: row.params,
                        updated_at: row.updated_at.to_rfc3339(),
                        updated_by: row.updated_by,
                    })
                    .collect::<Vec<_>>();
                for (position, assigned) in modules.iter().enumerate() {
                    diesel::sql_query(
                        "INSERT INTO module_account_params \
                         (account_id, module_id, kind, position, params, updated_at, updated_by) \
                         VALUES ($1, $2, 'assignment', $3, $4, $5, $6)",
                    )
                    .bind::<Text, _>(&account_id)
                    .bind::<Text, _>(&assigned.module_id)
                    .bind::<Integer, _>(position as i32)
                    .bind::<Jsonb, _>(&assigned.params)
                    .bind::<Timestamptz, _>(changed_at)
                    .bind::<Text, _>(change.operator)
                    .execute(conn)
                    .await?;
                }
                for (module_id, before, after) in assignment_changes(&previous, &modules) {
                    append(
                        conn,
                        version,
                        &module_id,
                        Some(&account_id),
                        &before,
                        &after,
                        change,
                        changed_at,
                    )
                    .await?;
                }
                Ok(())
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("set assignment: {e}"))
    }

    async fn history(&self, module_id: &str) -> Result<Vec<HistoryEntry>, String> {
        let mut conn = self.conn().await?;
        let rows: Vec<HistorySql> = diesel::sql_query(
            "SELECT version, module_id, account_id, previous_params, new_params, operator, reason, \
             changed_at FROM module_params_history WHERE module_id = $1 ORDER BY id",
        )
        .bind::<Text, _>(module_id)
        .load(&mut conn)
        .await
        .map_err(|e| format!("module params history: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|row| HistoryEntry {
                version: row.version as u64,
                module_id: row.module_id,
                account_id: row.account_id,
                previous: row.previous_params,
                new: row.new_params,
                operator: row.operator,
                reason: row.reason,
                at: row.changed_at.to_rfc3339(),
            })
            .collect())
    }

    async fn version(&self) -> Result<u64, String> {
        let mut conn = self.conn().await?;
        let rows: Vec<VersionSql> = diesel::sql_query("SELECT version FROM module_state_version")
            .load(&mut conn)
            .await
            .map_err(|e| format!("module params version: {e}"))?;
        Ok(rows.into_iter().next().map_or(0, |row| row.version as u64))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::pg::test_database_url;

    fn change(at: &str) -> Change<'static> {
        Change {
            operator: "0xop",
            reason: "ticket",
            at: at.to_string(),
        }
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn module_tables_round_trip_and_version_moves_with_history() {
        let url = test_database_url().await;
        let store = PgModuleStore::new(build_postgres_pool_lazy(&url, 2).expect("pool"));
        let other_replica = PgModuleStore::new(build_postgres_pool_lazy(&url, 2).expect("pool"));
        let t0 = "2026-10-15T00:00:00Z";
        let module = format!("blocklist-{}", rand::random::<u32>());
        let account = format!("0xacc{}", rand::random::<u32>());
        let start = store.version().await.unwrap();

        store
            .seed_instance(
                &module,
                true,
                &json!({ "recipient_denylist": ["0xaaa"] }),
                t0,
            )
            .await
            .unwrap();
        store
            .seed_instance(&module, true, &json!({ "ignored": true }), t0)
            .await
            .unwrap();
        store
            .set_global(
                &module,
                json!({ "recipient_denylist": ["0xaaa", "0xddd"] }),
                &change(t0),
            )
            .await
            .unwrap();
        store
            .set_layer(
                &account,
                &module,
                Some(json!({ "recipient_denylist": ["0xccc"] })),
                &change(t0),
            )
            .await
            .unwrap();
        store
            .set_assignment(
                &account,
                vec![Assigned {
                    module_id: format!("{module}-opt"),
                    params: json!({ "a": 1 }),
                }],
                &change(t0),
            )
            .await
            .unwrap();

        let instance = other_replica
            .instances()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.module_id == module)
            .unwrap();
        assert_eq!(
            instance.global_params,
            json!({ "recipient_denylist": ["0xaaa", "0xddd"] })
        );
        assert_eq!(
            instance.seeded_params,
            json!({ "recipient_denylist": ["0xaaa"] })
        );

        let rows = other_replica.account_params(&account).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.kind == ParamsKind::Layer));

        let history = other_replica.history(&module).await.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[2].account_id.as_deref(), Some(account.as_str()));
        assert_eq!(other_replica.version().await.unwrap(), start + 4);

        store
            .set_layer(&account, &module, None, &change(t0))
            .await
            .unwrap();
        assert_eq!(store.account_params(&account).await.unwrap().len(), 1);

        store
            .set_enabled(&module, false, &change(t0))
            .await
            .unwrap();
        let instance = other_replica
            .instances()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.module_id == module)
            .unwrap();
        assert!(!instance.enabled);
        assert!(
            store
                .set_enabled("no-such-module", false, &change(t0))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn concurrent_writes_serialize_and_the_version_follows_commit_order() {
        let url = test_database_url().await;
        let store = std::sync::Arc::new(PgModuleStore::new(
            build_postgres_pool_lazy(&url, 8).expect("pool"),
        ));
        let account = format!("0xacc{}", rand::random::<u32>());
        let t0 = "2026-10-15T00:00:00Z";
        let before = store.version().await.unwrap();
        let writes = (0..8).map(|n| {
            let store = store.clone();
            let account = account.clone();
            tokio::spawn(async move {
                store
                    .set_assignment(
                        &account,
                        vec![Assigned {
                            module_id: format!("opt-{n}"),
                            params: json!({}),
                        }],
                        &change(t0),
                    )
                    .await
            })
        });
        for write in writes {
            write.await.unwrap().unwrap();
        }
        let rows = store.account_params(&account).await.unwrap();
        assert_eq!(rows.len(), 1, "one replacement wins, never a union");
        assert!(store.version().await.unwrap() >= before + 8);
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_conditional_write_only_replaces_the_value_it_expects() {
        let url = test_database_url().await;
        let store = PgModuleStore::new(build_postgres_pool_lazy(&url, 2).expect("pool"));
        let module = format!("cas-{}", rand::random::<u32>());
        let t0 = "2026-10-16T00:00:00Z";
        store
            .seed_instance(&module, true, &json!({ "old_name": 1000 }), t0)
            .await
            .unwrap();
        store
            .set_global(&module, json!({ "new_name": 100 }), &change(t0))
            .await
            .unwrap();
        let before = store.version().await.unwrap();
        assert!(
            !store
                .set_global_if(
                    &module,
                    &json!({ "old_name": 1000 }),
                    json!({ "new_name": 1000 }),
                    &change(t0)
                )
                .await
                .unwrap(),
            "the stale expectation does not write"
        );
        assert_eq!(
            store.version().await.unwrap(),
            before,
            "and does not move the version"
        );
        assert!(
            store
                .set_global_if(
                    &module,
                    &json!({ "new_name": 100 }),
                    json!({ "new_name": 50 }),
                    &change(t0)
                )
                .await
                .unwrap()
        );
        let row = store
            .instances()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.module_id == module)
            .unwrap();
        assert_eq!(row.global_params, json!({ "new_name": 50 }));
    }
}
