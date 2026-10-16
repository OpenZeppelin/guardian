//! Module state tables: `module_instances`, `module_account_params`, `module_params_history`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceRow {
    pub module_id: String,
    pub enabled: bool,
    pub global_params: serde_json::Value,
    pub seeded_params: serde_json::Value,
    pub updated_at: String,
    pub updated_by: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamsKind {
    Layer,
    Assignment,
}

impl ParamsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamsKind::Layer => "layer",
            ParamsKind::Assignment => "assignment",
        }
    }

    pub fn parse(kind: &str) -> Result<Self, String> {
        match kind {
            "layer" => Ok(ParamsKind::Layer),
            "assignment" => Ok(ParamsKind::Assignment),
            other => Err(format!("unknown params kind {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountParamsRow {
    pub account_id: String,
    pub module_id: String,
    pub kind: ParamsKind,
    pub position: i32,
    pub params: serde_json::Value,
    pub updated_at: String,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub version: u64,
    pub module_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub previous: serde_json::Value,
    pub new: serde_json::Value,
    pub operator: String,
    pub reason: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assigned {
    pub module_id: String,
    pub params: serde_json::Value,
}

/// Who changed what and why, for the history and the audit record.
pub struct Change<'a> {
    pub operator: &'a str,
    pub reason: &'a str,
    pub at: String,
}

/// Module state every replica reads. Each write updates its rows and appends the history in
/// one transaction; the highest history version is the params version.
#[async_trait]
pub trait ModuleStore: Send + Sync {
    /// Inserts the instance row the first time `module_id` is seen; later calls do nothing.
    async fn seed_instance(
        &self,
        module_id: &str,
        enabled: bool,
        params: &serde_json::Value,
        at: &str,
    ) -> Result<(), String>;

    async fn instances(&self) -> Result<Vec<InstanceRow>, String>;

    /// Switches an instance on or off for every account. Recorded in the history as
    /// `{"enabled": previous}` to `{"enabled": new}`, so the version moves and caches refresh.
    async fn set_enabled(
        &self,
        module_id: &str,
        enabled: bool,
        change: &Change<'_>,
    ) -> Result<(), String>;

    async fn set_global(
        &self,
        module_id: &str,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<(), String>;

    /// Replaces the global params only while they still equal `expected`, and returns whether
    /// it wrote. For writes derived from a value read earlier (a module migrating params), so
    /// an operator's newer write is never overwritten.
    async fn set_global_if(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<bool, String>;

    async fn account_params(&self, account_id: &str) -> Result<Vec<AccountParamsRow>, String>;

    async fn set_layer(
        &self,
        account_id: &str,
        module_id: &str,
        params: Option<serde_json::Value>,
        change: &Change<'_>,
    ) -> Result<(), String>;

    async fn set_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<(), String>;

    async fn history(&self, module_id: &str) -> Result<Vec<HistoryEntry>, String>;

    async fn version(&self) -> Result<u64, String>;
}

#[derive(Debug, Default, Clone)]
struct Tables {
    instances: BTreeMap<String, InstanceRow>,
    account_params: Vec<AccountParamsRow>,
    history: Vec<HistoryEntry>,
}

impl Tables {
    fn version(&self) -> u64 {
        self.history.last().map_or(0, |entry| entry.version)
    }

    fn append(
        &mut self,
        module_id: &str,
        account_id: Option<&str>,
        previous: serde_json::Value,
        new: serde_json::Value,
        change: &Change<'_>,
    ) {
        let version = self.version() + 1;
        self.history.push(HistoryEntry {
            version,
            module_id: module_id.to_string(),
            account_id: account_id.map(str::to_string),
            previous,
            new,
            operator: change.operator.to_string(),
            reason: change.reason.to_string(),
            at: change.at.clone(),
        });
    }

    fn row_params(&self, account_id: &str, module_id: &str, kind: ParamsKind) -> serde_json::Value {
        self.account_params
            .iter()
            .find(|row| {
                row.account_id == account_id && row.module_id == module_id && row.kind == kind
            })
            .map(|row| row.params.clone())
            .unwrap_or_default()
    }
}

const STATE_FILE: &str = "module_state.json";

/// The three tables as one file, so a write and its history row land together or not at all.
#[derive(Serialize, serde::Deserialize, Default)]
struct Snapshot {
    instances: Vec<InstanceRow>,
    account_params: Vec<AccountParamsRow>,
    history: Vec<HistoryEntry>,
}

/// The filesystem backend's copy of the three tables.
pub struct FsModuleStore {
    tables: RwLock<Tables>,
    dir: Option<PathBuf>,
}

fn read_snapshot(dir: &std::path::Path) -> Result<Snapshot, String> {
    match std::fs::read_to_string(dir.join(STATE_FILE)) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("invalid {STATE_FILE}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Snapshot::default()),
        Err(e) => Err(format!("cannot read {STATE_FILE}: {e}")),
    }
}

fn write_snapshot(dir: &std::path::Path, tables: &Tables) -> Result<(), String> {
    use std::io::Write;
    let snapshot = Snapshot {
        instances: tables.instances.values().cloned().collect(),
        account_params: tables.account_params.clone(),
        history: tables.history.clone(),
    };
    let json = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
    let path = dir.join(STATE_FILE);
    let temp = path.with_extension("tmp");
    let failed = |e: std::io::Error| format!("cannot write {STATE_FILE}: {e}");
    let mut file = std::fs::File::create(&temp).map_err(failed)?;
    file.write_all(&json).map_err(failed)?;
    file.sync_all().map_err(failed)?;
    std::fs::rename(&temp, &path).map_err(failed)?;
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(failed)
}

impl FsModuleStore {
    pub fn in_memory() -> Self {
        Self {
            tables: RwLock::new(Tables::default()),
            dir: None,
        }
    }

    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let snapshot = read_snapshot(&dir)?;
        let tables = Tables {
            instances: snapshot
                .instances
                .into_iter()
                .map(|row| (row.module_id.clone(), row))
                .collect(),
            account_params: snapshot.account_params,
            history: snapshot.history,
        };
        Ok(Self {
            tables: RwLock::new(tables),
            dir: Some(dir),
        })
    }

    fn read<T>(&self, view: impl FnOnce(&Tables) -> T) -> T {
        match self.tables.read() {
            Ok(tables) => view(&tables),
            Err(poisoned) => view(&poisoned.into_inner()),
        }
    }

    fn write<T>(&self, apply: impl FnOnce(&mut Tables) -> Result<T, String>) -> Result<T, String> {
        let mut tables = self.tables.write().map_err(|_| "module store poisoned")?;
        let mut next = tables.clone();
        let out = apply(&mut next)?;
        if let Some(dir) = &self.dir {
            write_snapshot(dir, &next)?;
        }
        *tables = next;
        Ok(out)
    }
}

#[async_trait]
impl ModuleStore for FsModuleStore {
    async fn seed_instance(
        &self,
        module_id: &str,
        enabled: bool,
        params: &serde_json::Value,
        at: &str,
    ) -> Result<(), String> {
        if self.read(|tables| tables.instances.contains_key(module_id)) {
            return Ok(());
        }
        self.write(|tables| {
            tables.instances.insert(
                module_id.to_string(),
                InstanceRow {
                    module_id: module_id.to_string(),
                    enabled,
                    global_params: params.clone(),
                    seeded_params: params.clone(),
                    updated_at: at.to_string(),
                    updated_by: "catalog".into(),
                },
            );
            let change = Change {
                operator: "catalog",
                reason: "seeded from the catalog file",
                at: at.to_string(),
            };
            tables.append(
                module_id,
                None,
                serde_json::Value::Null,
                params.clone(),
                &change,
            );
            Ok(())
        })
    }

    async fn instances(&self) -> Result<Vec<InstanceRow>, String> {
        Ok(self.read(|tables| tables.instances.values().cloned().collect()))
    }

    async fn set_enabled(
        &self,
        module_id: &str,
        enabled: bool,
        change: &Change<'_>,
    ) -> Result<(), String> {
        self.write(|tables| {
            let row = tables
                .instances
                .get_mut(module_id)
                .ok_or_else(|| format!("module {module_id} has no instance row"))?;
            let previous = std::mem::replace(&mut row.enabled, enabled);
            row.updated_at = change.at.clone();
            row.updated_by = change.operator.to_string();
            tables.append(
                module_id,
                None,
                serde_json::json!({ "enabled": previous }),
                serde_json::json!({ "enabled": enabled }),
                change,
            );
            Ok(())
        })
    }

    async fn set_global(
        &self,
        module_id: &str,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<(), String> {
        self.write(|tables| {
            let row = tables
                .instances
                .entry(module_id.to_string())
                .or_insert_with(|| InstanceRow {
                    module_id: module_id.to_string(),
                    enabled: true,
                    global_params: serde_json::Value::Null,
                    seeded_params: params.clone(),
                    updated_at: change.at.clone(),
                    updated_by: change.operator.to_string(),
                });
            let previous = std::mem::replace(&mut row.global_params, params.clone());
            row.updated_at = change.at.clone();
            row.updated_by = change.operator.to_string();
            tables.append(module_id, None, previous, params, change);
            Ok(())
        })
    }

    async fn set_global_if(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<bool, String> {
        self.write(|tables| {
            let Some(row) = tables
                .instances
                .get_mut(module_id)
                .filter(|row| row.global_params == *expected)
            else {
                return Ok(false);
            };
            let previous = std::mem::replace(&mut row.global_params, params.clone());
            row.updated_at = change.at.clone();
            row.updated_by = change.operator.to_string();
            tables.append(module_id, None, previous, params, change);
            Ok(true)
        })
    }

    async fn account_params(&self, account_id: &str) -> Result<Vec<AccountParamsRow>, String> {
        Ok(self.read(|tables| {
            let mut rows: Vec<AccountParamsRow> = tables
                .account_params
                .iter()
                .filter(|row| row.account_id == account_id)
                .cloned()
                .collect();
            rows.sort_by_key(|row| row.position);
            rows
        }))
    }

    async fn set_layer(
        &self,
        account_id: &str,
        module_id: &str,
        params: Option<serde_json::Value>,
        change: &Change<'_>,
    ) -> Result<(), String> {
        self.write(|tables| {
            let previous = tables.row_params(account_id, module_id, ParamsKind::Layer);
            tables
                .account_params
                .retain(|row| !(row.account_id == account_id && row.module_id == module_id));
            if let Some(params) = &params {
                tables.account_params.push(AccountParamsRow {
                    account_id: account_id.to_string(),
                    module_id: module_id.to_string(),
                    kind: ParamsKind::Layer,
                    position: 0,
                    params: params.clone(),
                    updated_at: change.at.clone(),
                    updated_by: change.operator.to_string(),
                });
            }
            tables.append(
                module_id,
                Some(account_id),
                previous,
                params.unwrap_or_default(),
                change,
            );
            Ok(())
        })
    }

    async fn set_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<(), String> {
        self.write(|tables| {
            let previous: Vec<AccountParamsRow> = tables
                .account_params
                .iter()
                .filter(|row| row.account_id == account_id && row.kind == ParamsKind::Assignment)
                .cloned()
                .collect();
            tables.account_params.retain(|row| {
                !(row.account_id == account_id && row.kind == ParamsKind::Assignment)
            });
            for (position, assigned) in modules.iter().enumerate() {
                tables.account_params.push(AccountParamsRow {
                    account_id: account_id.to_string(),
                    module_id: assigned.module_id.clone(),
                    kind: ParamsKind::Assignment,
                    position: position as i32,
                    params: assigned.params.clone(),
                    updated_at: change.at.clone(),
                    updated_by: change.operator.to_string(),
                });
            }
            for (module_id, before, after) in assignment_changes(&previous, &modules) {
                tables.append(&module_id, Some(account_id), before, after, change);
            }
            Ok(())
        })
    }

    async fn history(&self, module_id: &str) -> Result<Vec<HistoryEntry>, String> {
        Ok(self.read(|tables| {
            tables
                .history
                .iter()
                .filter(|entry| entry.module_id == module_id)
                .cloned()
                .collect()
        }))
    }

    async fn version(&self) -> Result<u64, String> {
        Ok(self.read(Tables::version))
    }
}

/// `(module_id, previous, new)` for every module whose assigned params changed.
pub(crate) fn assignment_changes(
    previous: &[AccountParamsRow],
    modules: &[Assigned],
) -> Vec<(String, serde_json::Value, serde_json::Value)> {
    let mut ids: Vec<&str> = previous.iter().map(|row| row.module_id.as_str()).collect();
    ids.extend(modules.iter().map(|assigned| assigned.module_id.as_str()));
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter_map(|module_id| {
            let before = previous
                .iter()
                .find(|row| row.module_id == module_id)
                .map(|row| row.params.clone())
                .unwrap_or_default();
            let after = modules
                .iter()
                .find(|assigned| assigned.module_id == module_id)
                .map(|assigned| assigned.params.clone())
                .unwrap_or_default();
            (before != after).then(|| (module_id.to_string(), before, after))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn change() -> Change<'static> {
        Change {
            operator: "0xop",
            reason: "ticket",
            at: "2026-10-15T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn seeding_happens_once_and_api_changes_win() {
        let store = FsModuleStore::in_memory();
        store
            .seed_instance("caps", true, &json!({ "a": 1 }), "t0")
            .await
            .unwrap();
        store
            .set_global("caps", json!({ "a": 2 }), &change())
            .await
            .unwrap();
        store
            .seed_instance("caps", true, &json!({ "a": 3 }), "t1")
            .await
            .unwrap();
        let rows = store.instances().await.unwrap();
        assert_eq!(rows[0].global_params, json!({ "a": 2 }));
        assert_eq!(rows[0].seeded_params, json!({ "a": 1 }));
        assert_eq!(store.version().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn the_enabled_switch_is_stored_and_moves_the_version() {
        let store = FsModuleStore::in_memory();
        store
            .seed_instance("caps", true, &json!({}), "t0")
            .await
            .unwrap();
        store.set_enabled("caps", false, &change()).await.unwrap();
        assert!(!store.instances().await.unwrap()[0].enabled);
        let history = store.history("caps").await.unwrap();
        assert_eq!(history[1].previous, json!({ "enabled": true }));
        assert_eq!(history[1].new, json!({ "enabled": false }));
        assert_eq!(store.version().await.unwrap(), 2);
        assert!(
            store
                .set_enabled("unknown", false, &change())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn layers_and_assignments_share_the_account_table() {
        let store = FsModuleStore::in_memory();
        store
            .set_layer("0xacc", "blocklist", Some(json!({ "b": 1 })), &change())
            .await
            .unwrap();
        store
            .set_assignment(
                "0xacc",
                vec![Assigned {
                    module_id: "optional".into(),
                    params: json!({ "c": 1 }),
                }],
                &change(),
            )
            .await
            .unwrap();
        let rows = store.account_params("0xacc").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.kind == ParamsKind::Layer));
        assert!(rows.iter().any(|row| row.kind == ParamsKind::Assignment));
        assert!(store.account_params("0xother").await.unwrap().is_empty());

        store
            .set_layer("0xacc", "blocklist", None, &change())
            .await
            .unwrap();
        assert_eq!(store.account_params("0xacc").await.unwrap().len(), 1);
        let history = store.history("blocklist").await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].previous, json!({ "b": 1 }));
    }

    #[tokio::test]
    async fn tables_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        {
            let store = FsModuleStore::open(dir.clone()).unwrap();
            store
                .seed_instance("caps", true, &json!({ "a": 1 }), "t0")
                .await
                .unwrap();
            store
                .set_layer("0xacc", "caps", Some(json!({ "b": 1 })), &change())
                .await
                .unwrap();
        }
        assert!(dir.join(STATE_FILE).exists());
        assert!(!dir.join(STATE_FILE).with_extension("tmp").exists());
        let reopened = FsModuleStore::open(dir.clone()).unwrap();
        assert_eq!(reopened.version().await.unwrap(), 2);
        assert_eq!(reopened.account_params("0xacc").await.unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
