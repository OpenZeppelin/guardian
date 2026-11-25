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

/// One row of an account, without its params: which module it names and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountModule {
    pub account_id: String,
    pub module_id: String,
    pub kind: ParamsKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ModuleParamsHistoryEntry)]
pub struct HistoryEntry {
    /// The entry's place in the history, increasing in write order: the cursor a page of
    /// history is read below.
    #[serde(default)]
    #[schema(required = true)]
    pub id: u64,
    /// The params version the write committed as: the instance params version for an entry
    /// without `account_id`, that account's params version for one with it.
    pub version: u64,
    pub module_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[schema(value_type = Option<Object>, required = true)]
    pub previous: serde_json::Value,
    #[schema(value_type = Option<Object>, required = true)]
    pub new: serde_json::Value,
    pub operator: String,
    pub reason: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Assigned {
    pub module_id: String,
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

/// Who changed what and why, for the history and the audit record.
pub struct Change<'a> {
    pub operator: &'a str,
    pub reason: &'a str,
    pub at: String,
    /// The params version the operator read before this write: the instance params version
    /// for a global write, the account's for a layer or assignment. Set, the write is refused
    /// as [`WriteError::Stale`] once that version has moved, so two operators editing from the
    /// same read cannot silently drop each other's change. Unset, the write is unconditional.
    pub expected_version: Option<u64>,
}

/// Why an operator write did not commit.
#[derive(Debug)]
pub enum WriteError {
    /// The write named the version it was made against, and the rows have moved since.
    Stale {
        current: u64,
    },
    Storage(String),
}

impl From<String> for WriteError {
    fn from(message: String) -> Self {
        WriteError::Storage(message)
    }
}

impl From<&str> for WriteError {
    fn from(message: &str) -> Self {
        WriteError::Storage(message.to_string())
    }
}

impl WriteError {
    /// Names the write a storage failure happened in; a stale write is left as it is.
    pub fn context(self, write: &str) -> Self {
        match self {
            WriteError::Storage(message) => WriteError::Storage(format!("{write}: {message}")),
            stale @ WriteError::Stale { .. } => stale,
        }
    }
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Stale { current } => {
                write!(
                    f,
                    "the params moved to version {current} since they were read"
                )
            }
            WriteError::Storage(message) => write!(f, "{message}"),
        }
    }
}

/// Module state every replica reads. Each write updates its rows and appends the history in
/// one transaction. There are two kinds of params version: the instance params version moves
/// with every write to the instance rows and keys each replica's instance cache, and each
/// account's params version moves with every write to that account's rows. A write to one
/// account therefore never invalidates a cache or waits for a write to another account.
#[async_trait]
pub trait ModuleStore: Send + Sync {
    /// Inserts the instance row the first time `module_id` is seen; later calls do nothing.
    /// Stores a new instance with `params` as its global params and `catalog_params`, the
    /// catalog's value as written, as its seed, so drift compares like with like even when
    /// the module normalized the params it stores.
    async fn seed_instance(
        &self,
        module_id: &str,
        enabled: bool,
        params: &serde_json::Value,
        catalog_params: &serde_json::Value,
        at: &str,
    ) -> Result<(), String>;

    async fn instances(&self) -> Result<Vec<InstanceRow>, String>;

    /// Switches an instance on or off for every account. Recorded in the history as
    /// `{"enabled": previous}` to `{"enabled": new}`, so the instance params version moves and
    /// caches refresh. Like every operator write, returns the version the write committed as,
    /// so the audit entry of the write names that version and not one read afterwards.
    async fn set_enabled(
        &self,
        module_id: &str,
        enabled: bool,
        change: &Change<'_>,
    ) -> Result<u64, String>;

    async fn set_global(
        &self,
        module_id: &str,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<u64, WriteError>;

    /// Replaces the global params only while they still equal `expected`, and returns the
    /// version it committed as, or `None` when it did not write. For writes derived from a
    /// value read earlier (a module migrating params), so an operator's newer write is never
    /// overwritten.
    async fn set_global_if(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<Option<u64>, String>;

    /// Stores params the catalog file sets, as a write by the catalog: replaces the global
    /// params with `params` (in the module's normalized form when it has one) only while they
    /// still equal `expected`, and records `seed`, the catalog value as written, as the
    /// instance's seed. Used for params that moved out of the catalog file (`expected` is the
    /// empty placeholder a file-managed seed left, so a later start no longer sees a
    /// file-managed seed and does not adopt over an operator's later write, `{}` included) and
    /// for params the catalog locks (`operator_can_set_params = false`). Returns the version it
    /// committed as, or `None` when it did not write (an operator wrote first; a later start
    /// tries again).
    async fn set_catalog_params(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        seed: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<Option<u64>, String>;

    async fn account_params(&self, account_id: &str) -> Result<Vec<AccountParamsRow>, String>;

    /// What one evaluation reads, in one read: the instance params version, the account's
    /// params version and rows and, unless `cached_version` is the instance params version,
    /// the instance rows.
    async fn snapshot(
        &self,
        account_id: &str,
        cached_version: Option<u64>,
    ) -> Result<ModuleSnapshot, String>;

    /// Which module every account row names, ordered by account. A scan for the budget and
    /// orphan report, never read on a request path; it carries no params, which are copied per
    /// account and can be large.
    async fn account_modules(&self) -> Result<Vec<AccountModule>, String>;

    /// Sets or removes the account's layer on a `scope = all` instance. Returns the account
    /// params version the write committed as.
    async fn set_layer(
        &self,
        account_id: &str,
        module_id: &str,
        params: Option<serde_json::Value>,
        change: &Change<'_>,
    ) -> Result<u64, WriteError>;

    /// Replaces the account's assignment. Returns the account params version the write
    /// committed as.
    async fn set_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<u64, WriteError>;

    /// Writes `modules` as the account's assignment only when it has no assignment and no
    /// write has touched its rows yet (account params version 0), which is how `default`
    /// instances reach a newly registered account; an operator's earlier write always wins.
    /// `modules` carries each `default` instance's global params as registration read them, in
    /// the snapshot its `before_account_registered` plan judged the account with, so the copy is
    /// what that check saw; a global write that commits after that read is not in the copy.
    /// Returns whether it wrote.
    async fn seed_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<bool, String>;

    /// The module's history, newest first: at most `limit` entries whose `id` is below
    /// `before`.
    async fn history(
        &self,
        module_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, String>;

    /// The instance params version: the version of the latest write to the instance rows.
    async fn instance_version(&self) -> Result<u64, String>;
}

/// See [`ModuleStore::snapshot`].
#[derive(Debug, Clone)]
pub struct ModuleSnapshot {
    pub instance_version: u64,
    pub account_version: u64,
    pub instances: Option<Vec<InstanceRow>>,
    pub account_params: Vec<AccountParamsRow>,
}

#[derive(Debug, Default, Clone, PartialEq)]
struct Tables {
    instances: BTreeMap<String, InstanceRow>,
    account_params: Vec<AccountParamsRow>,
    history: Vec<HistoryEntry>,
    instance_version: u64,
    account_versions: BTreeMap<String, u64>,
}

impl Tables {
    fn instance_version(&self) -> u64 {
        self.instance_version
    }

    fn account_version(&self, account_id: &str) -> u64 {
        self.account_versions.get(account_id).copied().unwrap_or(0)
    }

    /// Refuses a write made against `expected` once the version of its scope (the instance
    /// rows for `None`, that account's rows for `Some`) has moved past it.
    fn check(&self, account_id: Option<&str>, expected: Option<u64>) -> Result<(), WriteError> {
        let current = match account_id {
            None => self.instance_version,
            Some(account_id) => self.account_version(account_id),
        };
        match expected {
            Some(expected) if expected != current => Err(WriteError::Stale { current }),
            _ => Ok(()),
        }
    }

    /// Moves the version a write commits as, once per write however many history entries it
    /// appends: the instance version for `None`, that account's for `Some`.
    fn bump(&mut self, account_id: Option<&str>) -> u64 {
        let version = match account_id {
            None => &mut self.instance_version,
            Some(account_id) => self
                .account_versions
                .entry(account_id.to_string())
                .or_default(),
        };
        *version += 1;
        *version
    }

    /// The versions a state file without them implies: the highest of each scope's history
    /// entries.
    fn versions_from_history(&mut self) {
        for entry in &self.history {
            let version = match entry.account_id.as_deref() {
                None => &mut self.instance_version,
                Some(account_id) => self
                    .account_versions
                    .entry(account_id.to_string())
                    .or_default(),
            };
            *version = (*version).max(entry.version);
        }
    }

    fn account_rows(&self, account_id: &str) -> Vec<AccountParamsRow> {
        let mut rows: Vec<AccountParamsRow> = self
            .account_params
            .iter()
            .filter(|row| row.account_id == account_id)
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.position);
        rows
    }

    fn append(
        &mut self,
        version: u64,
        module_id: &str,
        account_id: Option<&str>,
        previous: serde_json::Value,
        new: serde_json::Value,
        change: &Change<'_>,
    ) {
        let id = self.history.len() as u64 + 1;
        self.history.push(HistoryEntry {
            id,
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
    #[serde(default)]
    instance_version: u64,
    #[serde(default)]
    account_versions: BTreeMap<String, u64>,
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

/// Replaces the state file with `tables`. The outer error means the old file is still in place;
/// the inner one means the new file is in place but the directory entry may not be durable yet.
fn write_snapshot(dir: &std::path::Path, tables: &Tables) -> Result<Result<(), String>, String> {
    use std::io::Write;
    let snapshot = Snapshot {
        instances: tables.instances.values().cloned().collect(),
        account_params: tables.account_params.clone(),
        history: tables.history.clone(),
        instance_version: tables.instance_version,
        account_versions: tables.account_versions.clone(),
    };
    let json = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
    let path = dir.join(STATE_FILE);
    let temp = path.with_extension("tmp");
    let failed = |e: std::io::Error| format!("cannot write {STATE_FILE}: {e}");
    let mut file = std::fs::File::create(&temp).map_err(failed)?;
    file.write_all(&json).map_err(failed)?;
    file.sync_all().map_err(failed)?;
    std::fs::rename(&temp, &path).map_err(failed)?;
    Ok(std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(failed))
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
        let mut tables = Tables {
            instances: snapshot
                .instances
                .into_iter()
                .map(|row| (row.module_id.clone(), row))
                .collect(),
            account_params: snapshot.account_params,
            history: snapshot
                .history
                .into_iter()
                .zip(1u64..)
                .map(|(entry, id)| HistoryEntry { id, ..entry })
                .collect(),
            instance_version: snapshot.instance_version,
            account_versions: snapshot.account_versions,
        };
        tables.versions_from_history();
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

    /// Applies `apply` to a copy of the tables and persists it. A write that leaves the tables
    /// as they were persists nothing. Once the new state file is in place the live tables follow
    /// it, even when making its directory entry durable then fails.
    fn write<T, E: From<String>>(
        &self,
        apply: impl FnOnce(&mut Tables) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut tables = self
            .tables
            .write()
            .map_err(|_| E::from("module store poisoned".to_string()))?;
        let mut next = tables.clone();
        let out = apply(&mut next)?;
        if next == *tables {
            return Ok(out);
        }
        let durable = match &self.dir {
            Some(dir) => write_snapshot(dir, &next).map_err(E::from)?,
            None => Ok(()),
        };
        *tables = next;
        durable.map_err(E::from)?;
        Ok(out)
    }
}

/// Whether a conditional global write also replaces the instance's seed.
pub(crate) enum Reseed {
    Keep,
    To(serde_json::Value),
}

impl FsModuleStore {
    fn write_global_if(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        change: &Change<'_>,
        reseed: Reseed,
    ) -> Result<Option<u64>, String> {
        self.write(|tables| {
            let Some(row) = tables
                .instances
                .get_mut(module_id)
                .filter(|row| row.global_params == *expected)
            else {
                return Ok(None);
            };
            let previous = std::mem::replace(&mut row.global_params, params.clone());
            match reseed {
                Reseed::Keep => {}
                Reseed::To(seed) => row.seeded_params = seed,
            }
            row.updated_at = change.at.clone();
            row.updated_by = change.operator.to_string();
            let version = tables.bump(None);
            tables.append(version, module_id, None, previous, params, change);
            Ok(Some(version))
        })
    }
}

#[async_trait]
impl ModuleStore for FsModuleStore {
    async fn seed_instance(
        &self,
        module_id: &str,
        enabled: bool,
        params: &serde_json::Value,
        catalog_params: &serde_json::Value,
        at: &str,
    ) -> Result<(), String> {
        if self.read(|tables| tables.instances.contains_key(module_id)) {
            return Ok(());
        }
        self.write(|tables| {
            if tables.instances.contains_key(module_id) {
                return Ok(());
            }
            tables.instances.insert(
                module_id.to_string(),
                InstanceRow {
                    module_id: module_id.to_string(),
                    enabled,
                    global_params: params.clone(),
                    seeded_params: catalog_params.clone(),
                    updated_at: at.to_string(),
                    updated_by: "catalog".into(),
                },
            );
            let change = Change {
                operator: "catalog",
                reason: "seeded from the catalog file",
                at: at.to_string(),
                expected_version: None,
            };
            let version = tables.bump(None);
            tables.append(
                version,
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
    ) -> Result<u64, String> {
        self.write(|tables| {
            let row = tables
                .instances
                .get_mut(module_id)
                .ok_or_else(|| format!("module {module_id} has no instance row"))?;
            let previous = std::mem::replace(&mut row.enabled, enabled);
            row.updated_at = change.at.clone();
            row.updated_by = change.operator.to_string();
            let version = tables.bump(None);
            tables.append(
                version,
                module_id,
                None,
                serde_json::json!({ "enabled": previous }),
                serde_json::json!({ "enabled": enabled }),
                change,
            );
            Ok(version)
        })
    }

    async fn set_global(
        &self,
        module_id: &str,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<u64, WriteError> {
        self.write(|tables| {
            tables.check(None, change.expected_version)?;
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
            let version = tables.bump(None);
            tables.append(version, module_id, None, previous, params, change);
            Ok(version)
        })
    }

    async fn set_global_if(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<Option<u64>, String> {
        self.write_global_if(module_id, expected, params, change, Reseed::Keep)
    }

    async fn set_catalog_params(
        &self,
        module_id: &str,
        expected: &serde_json::Value,
        params: serde_json::Value,
        seed: serde_json::Value,
        change: &Change<'_>,
    ) -> Result<Option<u64>, String> {
        self.write_global_if(module_id, expected, params, change, Reseed::To(seed))
    }

    async fn account_params(&self, account_id: &str) -> Result<Vec<AccountParamsRow>, String> {
        Ok(self.read(|tables| tables.account_rows(account_id)))
    }

    async fn snapshot(
        &self,
        account_id: &str,
        cached_version: Option<u64>,
    ) -> Result<ModuleSnapshot, String> {
        Ok(self.read(|tables| {
            let instance_version = tables.instance_version();
            ModuleSnapshot {
                instance_version,
                account_version: tables.account_version(account_id),
                instances: (cached_version != Some(instance_version))
                    .then(|| tables.instances.values().cloned().collect()),
                account_params: tables.account_rows(account_id),
            }
        }))
    }

    async fn account_modules(&self) -> Result<Vec<AccountModule>, String> {
        Ok(self.read(|tables| {
            let mut rows: Vec<&AccountParamsRow> = tables.account_params.iter().collect();
            rows.sort_by(|a, b| (&a.account_id, a.position).cmp(&(&b.account_id, b.position)));
            rows.into_iter()
                .map(|row| AccountModule {
                    account_id: row.account_id.clone(),
                    module_id: row.module_id.clone(),
                    kind: row.kind,
                })
                .collect()
        }))
    }

    async fn set_layer(
        &self,
        account_id: &str,
        module_id: &str,
        params: Option<serde_json::Value>,
        change: &Change<'_>,
    ) -> Result<u64, WriteError> {
        self.write(|tables| {
            tables.check(Some(account_id), change.expected_version)?;
            let version = tables.bump(Some(account_id));
            let previous = tables.row_params(account_id, module_id, ParamsKind::Layer);
            tables.account_params.retain(|row| {
                !(row.account_id == account_id
                    && row.module_id == module_id
                    && row.kind == ParamsKind::Layer)
            });
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
                version,
                module_id,
                Some(account_id),
                previous,
                params.unwrap_or_default(),
                change,
            );
            Ok(version)
        })
    }

    async fn set_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<u64, WriteError> {
        self.write(|tables| {
            tables.check(Some(account_id), change.expected_version)?;
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
            let version = tables.bump(Some(account_id));
            for (module_id, before, after) in assignment_changes(&previous, &modules) {
                tables.append(version, &module_id, Some(account_id), before, after, change);
            }
            Ok(version)
        })
    }

    async fn seed_assignment(
        &self,
        account_id: &str,
        modules: Vec<Assigned>,
        change: &Change<'_>,
    ) -> Result<bool, String> {
        self.write(|tables| {
            let assigned = tables
                .account_params
                .iter()
                .any(|row| row.account_id == account_id && row.kind == ParamsKind::Assignment);
            if assigned || tables.account_version(account_id) != 0 || modules.is_empty() {
                return Ok(false);
            }
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
            let version = tables.bump(Some(account_id));
            for (module_id, before, after) in assignment_changes(&[], &modules) {
                tables.append(version, &module_id, Some(account_id), before, after, change);
            }
            Ok(true)
        })
    }

    async fn history(
        &self,
        module_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, String> {
        Ok(self.read(|tables| {
            tables
                .history
                .iter()
                .rev()
                .filter(|entry| entry.module_id == module_id)
                .filter(|entry| before.is_none_or(|before| entry.id < before))
                .take(limit)
                .cloned()
                .collect()
        }))
    }

    async fn instance_version(&self) -> Result<u64, String> {
        Ok(self.read(Tables::instance_version))
    }
}

/// `(module_id, previous, new)`: one history entry per module whose params or evaluation position the write changes: order
/// decides which module refuses first and which ones the budget cuts.
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
            let old = previous.iter().find(|row| row.module_id == module_id);
            let new = modules
                .iter()
                .position(|assigned| assigned.module_id == module_id);
            let before = old.map(|row| row.params.clone()).unwrap_or_default();
            let after = new
                .map(|index| modules[index].params.clone())
                .unwrap_or_default();
            let moved =
                matches!((old, new), (Some(row), Some(index)) if row.position as usize != index);
            (before != after || moved).then(|| (module_id.to_string(), before, after))
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
            expected_version: None,
        }
    }

    #[tokio::test]
    async fn seeding_happens_once_and_api_changes_win() {
        let store = FsModuleStore::in_memory();
        store
            .seed_instance("caps", true, &json!({ "a": 1 }), &json!({ "a": 1 }), "t0")
            .await
            .unwrap();
        store
            .set_global("caps", json!({ "a": 2 }), &change())
            .await
            .unwrap();
        store
            .seed_instance("caps", true, &json!({ "a": 3 }), &json!({ "a": 3 }), "t1")
            .await
            .unwrap();
        let rows = store.instances().await.unwrap();
        assert_eq!(rows[0].global_params, json!({ "a": 2 }));
        assert_eq!(rows[0].seeded_params, json!({ "a": 1 }));
        assert_eq!(store.instance_version().await.unwrap(), 2);
    }

    async fn account_version(store: &FsModuleStore, account_id: &str) -> u64 {
        store
            .snapshot(account_id, None)
            .await
            .unwrap()
            .account_version
    }

    #[tokio::test]
    async fn each_write_reports_the_version_it_committed_as() {
        let store = FsModuleStore::in_memory();
        store
            .seed_instance("caps", true, &json!({}), &json!({}), "t0")
            .await
            .unwrap();
        let assigned = || {
            vec![Assigned {
                module_id: "caps".into(),
                params: json!({}),
            }]
        };
        let instance_writes = [
            store
                .set_global("caps", json!({ "a": 1 }), &change())
                .await
                .unwrap(),
            store.set_enabled("caps", false, &change()).await.unwrap(),
            store
                .set_global_if("caps", &json!({ "a": 1 }), json!({ "a": 2 }), &change())
                .await
                .unwrap()
                .expect("the expectation holds"),
        ];
        assert!(
            instance_writes.windows(2).all(|pair| pair[0] < pair[1]),
            "later instance writes commit as later versions: {instance_writes:?}"
        );
        assert_eq!(
            instance_writes.last(),
            Some(&store.instance_version().await.unwrap())
        );
        let account_writes = [
            store
                .set_layer("0xacc", "caps", Some(json!({})), &change())
                .await
                .unwrap(),
            store
                .set_assignment("0xacc", assigned(), &change())
                .await
                .unwrap(),
        ];
        assert_eq!(account_writes, [1, 2], "each account counts its own writes");
        assert_eq!(account_version(&store, "0xacc").await, 2);
        assert_eq!(
            store
                .set_global_if("caps", &json!({ "a": 1 }), json!({ "a": 3 }), &change())
                .await
                .unwrap(),
            None,
            "a stale expectation writes nothing and reports no version"
        );
    }

    #[tokio::test]
    async fn a_write_moves_its_version_once_whatever_it_changes() {
        let store = FsModuleStore::in_memory();
        let assigned = |ids: &[&str]| {
            ids.iter()
                .map(|id| Assigned {
                    module_id: id.to_string(),
                    params: json!({ "id": id }),
                })
                .collect::<Vec<_>>()
        };
        let copied = store
            .seed_assignment("0xacc", assigned(&["a", "b", "c"]), &change())
            .await
            .unwrap();
        assert!(copied);
        assert_eq!(
            account_version(&store, "0xacc").await,
            1,
            "one copy, one version"
        );
        for id in ["a", "b", "c"] {
            let entries = store.history(id, None, 1).await.unwrap();
            assert_eq!(
                entries[0].version, 1,
                "the entries of one write share its version"
            );
        }
        let unchanged = store
            .set_assignment("0xacc", assigned(&["a", "b", "c"]), &change())
            .await
            .unwrap();
        assert_eq!(
            unchanged, 2,
            "a write that changes nothing still commits as a version"
        );
    }

    #[tokio::test]
    async fn an_operator_write_that_changed_nothing_still_blocks_the_default_copy() {
        let store = FsModuleStore::in_memory();
        store
            .set_assignment("0xacc", Vec::new(), &change())
            .await
            .unwrap();
        assert!(
            store
                .history("screening", None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let copied = store
            .seed_assignment(
                "0xacc",
                vec![Assigned {
                    module_id: "screening".into(),
                    params: json!({}),
                }],
                &change(),
            )
            .await
            .unwrap();
        assert!(!copied, "the operator wrote the account first");
        assert!(store.account_params("0xacc").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_account_write_moves_only_that_accounts_version() {
        let store = FsModuleStore::in_memory();
        store
            .seed_instance("caps", true, &json!({}), &json!({}), "t0")
            .await
            .unwrap();
        let instance_version = store.instance_version().await.unwrap();
        store
            .set_layer("0xacc", "caps", Some(json!({ "b": 1 })), &change())
            .await
            .unwrap();
        assert_eq!(
            store.instance_version().await.unwrap(),
            instance_version,
            "replicas keep their instance cache"
        );
        assert_eq!(account_version(&store, "0xacc").await, 1);
        assert_eq!(account_version(&store, "0xother").await, 0);
        let snapshot = store
            .snapshot("0xacc", Some(instance_version))
            .await
            .unwrap();
        assert!(
            snapshot.instances.is_none(),
            "the cached instance rows are current"
        );
        store
            .set_global("caps", json!({ "a": 1 }), &change())
            .await
            .unwrap();
        assert_eq!(
            account_version(&store, "0xacc").await,
            1,
            "an instance write leaves every account's version"
        );
    }

    #[tokio::test]
    async fn the_enabled_switch_is_stored_and_moves_the_version() {
        let store = FsModuleStore::in_memory();
        store
            .seed_instance("caps", true, &json!({}), &json!({}), "t0")
            .await
            .unwrap();
        store.set_enabled("caps", false, &change()).await.unwrap();
        assert!(!store.instances().await.unwrap()[0].enabled);
        let history = store.history("caps", None, 10).await.unwrap();
        assert_eq!(history[0].previous, json!({ "enabled": true }));
        assert_eq!(history[0].new, json!({ "enabled": false }));
        assert_eq!(store.instance_version().await.unwrap(), 2);
        assert!(
            store
                .set_enabled("unknown", false, &change())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn reordering_an_assignment_is_recorded_and_moves_the_version() {
        let store = FsModuleStore::in_memory();
        let assigned = |ids: &[&str]| {
            ids.iter()
                .map(|id| Assigned {
                    module_id: id.to_string(),
                    params: json!({}),
                })
                .collect::<Vec<_>>()
        };
        store
            .set_assignment("0xacc", assigned(&["a", "b"]), &change())
            .await
            .unwrap();
        let version = account_version(&store, "0xacc").await;
        store
            .set_assignment("0xacc", assigned(&["b", "a"]), &change())
            .await
            .unwrap();
        assert_eq!(
            account_version(&store, "0xacc").await,
            version + 1,
            "two modules moved, one write"
        );
        assert_eq!(store.history("a", None, 10).await.unwrap().len(), 2);
        let order: Vec<_> = store
            .account_params("0xacc")
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.module_id)
            .collect();
        assert_eq!(order, ["b", "a"]);
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
        let history = store.history("blocklist", None, 10).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].previous, json!({ "b": 1 }));
        let older = store
            .history("blocklist", Some(history[0].id), 1)
            .await
            .unwrap();
        assert_eq!(
            older,
            vec![history[1].clone()],
            "paged below an entry, newest first"
        );
    }

    #[tokio::test]
    async fn default_copies_never_overwrite_an_operator_write_or_refill_an_emptied_assignment() {
        let store = FsModuleStore::in_memory();
        let change = |at: &str| Change {
            operator: "catalog",
            reason: "default",
            at: at.into(),
            expected_version: None,
        };
        let copy = || {
            vec![Assigned {
                module_id: "screening".into(),
                params: serde_json::json!({ "limit": 5 }),
            }]
        };
        assert!(
            store
                .seed_assignment("0xnew", copy(), &change("t1"))
                .await
                .unwrap()
        );
        assert!(
            !store
                .seed_assignment("0xnew", copy(), &change("t2"))
                .await
                .unwrap(),
            "an assignment already exists"
        );
        store
            .set_assignment("0xnew", Vec::new(), &change("t3"))
            .await
            .unwrap();
        assert!(
            !store
                .seed_assignment("0xnew", copy(), &change("t4"))
                .await
                .unwrap(),
            "an assignment an operator emptied is not refilled"
        );
        assert!(store.account_params("0xnew").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_state_file_without_versions_takes_them_from_its_history() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = |id: u64, version: u64, account_id: Option<&str>| {
            json!({
                "id": id, "version": version, "module_id": "caps", "account_id": account_id,
                "previous": null, "new": {}, "operator": "0xop", "reason": "r", "at": "t"
            })
        };
        let old_file = json!({
            "instances": [],
            "account_params": [],
            "history": [entry(1, 1, None), entry(2, 2, Some("0xacc")), entry(3, 3, None)],
        });
        std::fs::write(dir.join(STATE_FILE), old_file.to_string()).unwrap();
        let store = FsModuleStore::open(dir.clone()).unwrap();
        assert_eq!(store.instance_version().await.unwrap(), 3);
        assert_eq!(account_version(&store, "0xacc").await, 2);
        assert!(
            !store
                .seed_assignment(
                    "0xacc",
                    vec![Assigned {
                        module_id: "caps".into(),
                        params: json!({}),
                    }],
                    &change(),
                )
                .await
                .unwrap(),
            "an account with history keeps its operator's choices"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_version_moved_by_a_write_that_changed_nothing_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        {
            let store = FsModuleStore::open(dir.clone()).unwrap();
            store
                .set_assignment("0xacc", Vec::new(), &change())
                .await
                .unwrap();
        }
        let reopened = FsModuleStore::open(dir.clone()).unwrap();
        assert_eq!(account_version(&reopened, "0xacc").await, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn tables_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        {
            let store = FsModuleStore::open(dir.clone()).unwrap();
            store
                .seed_instance("caps", true, &json!({ "a": 1 }), &json!({ "a": 1 }), "t0")
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
        assert_eq!(reopened.instance_version().await.unwrap(), 1);
        assert_eq!(account_version(&reopened, "0xacc").await, 1);
        let ids: Vec<u64> = reopened
            .history("caps", None, 10)
            .await
            .unwrap()
            .iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, [2, 1], "entries keep their place across a restart");
        assert_eq!(reopened.account_params("0xacc").await.unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_write_that_declines_persists_nothing() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        let store = FsModuleStore::open(dir.clone()).unwrap();
        store
            .seed_instance("caps", true, &json!({ "a": 1 }), &json!({ "a": 1 }), "t0")
            .await
            .unwrap();
        let assigned = || {
            vec![Assigned {
                module_id: "caps".into(),
                params: json!({}),
            }]
        };
        assert!(
            store
                .seed_assignment("0xacc", assigned(), &change())
                .await
                .unwrap()
        );
        std::fs::create_dir(dir.join(STATE_FILE).with_extension("tmp")).unwrap();
        assert!(
            !store
                .seed_assignment("0xacc", assigned(), &change())
                .await
                .unwrap()
        );
        store
            .seed_instance("caps", false, &json!({ "a": 2 }), &json!({ "a": 2 }), "t1")
            .await
            .unwrap();
        assert!(
            store
                .set_layer("0xacc", "caps", Some(json!({ "b": 1 })), &change())
                .await
                .is_err()
        );
        assert_eq!(account_version(&store, "0xacc").await, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn adopted_catalog_params_replace_the_placeholder_and_the_seed_once() {
        let store = FsModuleStore::in_memory();
        let file_managed = json!({ "api_key": { "$secret": "vendor-key" }, "cap": 5 });
        store
            .seed_instance(
                "vendor",
                true,
                &json!({}),
                &file_managed,
                "2026-10-15T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(
            store
                .set_catalog_params(
                    "vendor",
                    &json!({}),
                    json!({ "cap": "5" }),
                    json!({ "cap": 5 }),
                    &change()
                )
                .await
                .unwrap()
                .is_some()
        );
        let row = store.instances().await.unwrap().remove(0);
        assert_eq!(
            row.global_params,
            json!({ "cap": "5" }),
            "the normalized params apply"
        );
        assert_eq!(
            row.seeded_params,
            json!({ "cap": 5 }),
            "the seed is the catalog value as written"
        );
        assert_eq!(
            store
                .set_catalog_params(
                    "vendor",
                    &json!({}),
                    json!({ "cap": 9 }),
                    json!({ "cap": 9 }),
                    &change()
                )
                .await
                .unwrap(),
            None,
            "params that are no longer the placeholder are not adopted over"
        );
    }
}
