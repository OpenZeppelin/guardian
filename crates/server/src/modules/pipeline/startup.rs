//! Startup reconciliation: the catalog file and the stored module state become the running
//! pipeline. It runs once per process, before any request, in this order:
//!
//! 1. The catalog's params are checked against the size cap.
//! 2. The stored instance rows are read and applied to the catalog: the stored enabled flag and
//!    operator-managed global params replace the catalog's, except that an instance the catalog
//!    requires is enabled ([`Stored::apply_to`]).
//! 3. The instances are built and checked against their modules
//!    ([`Registry::instantiate`](crate::modules::registry::Registry::instantiate)). A failure here
//!    stops startup before anything is written.
//! 4. The startup writes ([`StartupWrites`]): the catalog-required enables, the seeds of
//!    instances new to this Guardian, and the params a module migrated.
//! 5. Instances whose module did not answer start with their breaker held open, and each health
//!    probe runs their checks until they pass.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use super::{Cache, ModulePipeline};
use crate::modules::breaker::Breakers;
use crate::modules::catalog::Catalog;
use crate::modules::limits::Limits;
use crate::modules::registry::{Instance, Instantiated, Registry};
use crate::modules::store::{Change, InstanceRow, ModuleStore};

/// The rules every stored params value follows, applied to what a module returns as normalized
/// params: within the size cap and free of secret references.
pub(super) fn storable(limits: &Limits, params: &serde_json::Value) -> Result<(), String> {
    limits.check_params_size(params)?;
    if crate::modules::catalog::contains_secret_reference(params) {
        return Err("normalized params contain a secret reference".into());
    }
    Ok(())
}

/// The stored instance rows as startup found them, by module id.
struct Stored {
    rows: HashMap<String, InstanceRow>,
}

/// What the stored state changes in the catalog before the instances are built.
struct Applied {
    /// Operator-managed global params that differ from the catalog's and apply instead.
    effective: HashMap<String, serde_json::Value>,
    /// Instances whose stored seed differs from the catalog's params.
    seed_drift: HashSet<String>,
    /// Instances stored as disabled that the catalog declares enabled and not
    /// operator-disableable, so startup enables them.
    enabled_by_catalog: Vec<String>,
}

impl Stored {
    async fn read(store: &dyn ModuleStore) -> Result<Self, String> {
        Ok(Self {
            rows: store
                .instances()
                .await?
                .into_iter()
                .map(|row| (row.module_id.clone(), row))
                .collect(),
        })
    }

    /// The instances this Guardian has run before.
    fn known(&self) -> HashSet<String> {
        self.rows.keys().cloned().collect()
    }

    fn contains(&self, module_id: &str) -> bool {
        self.rows.contains_key(module_id)
    }

    fn global_params(&self, module_id: &str) -> Option<&serde_json::Value> {
        self.rows.get(module_id).map(|row| &row.global_params)
    }

    /// Applies the stored state to every catalog instance it has a row for. The stored flag is
    /// the switch, with one exception: an instance the catalog declares enabled and not
    /// operator-disableable cannot be left off by a disable that was allowed under an earlier
    /// catalog, and switching it on only tightens policy.
    fn apply_to(&self, catalog: &mut Catalog) -> Applied {
        let mut applied = Applied {
            effective: HashMap::new(),
            seed_drift: HashSet::new(),
            enabled_by_catalog: Vec::new(),
        };
        for config in &mut catalog.instances {
            let Some(row) = self.rows.get(&config.id) else {
                continue;
            };
            if !config.file_managed_params && row.seeded_params != config.params {
                applied.seed_drift.insert(config.id.clone());
            }
            let catalog_requires_it =
                !row.enabled && config.catalog_enabled && !config.operator_can_disable;
            if catalog_requires_it {
                tracing::warn!(module = %config.id, "Stored flag disables an instance the catalog declares enabled and not operator-disableable; enabling it");
                applied.enabled_by_catalog.push(config.id.clone());
            } else if row.enabled != config.catalog_enabled {
                tracing::warn!(module = %config.id, enabled = row.enabled, "Catalog `enabled` differs from the stored runtime flag; the stored flag applies");
            }
            config.enabled = row.enabled || catalog_requires_it;
            if !config.file_managed_params && row.global_params != config.params {
                tracing::warn!(module = %config.id, "Catalog params differ from the operator-managed global params; the stored params apply");
                applied
                    .effective
                    .insert(config.id.clone(), row.global_params.clone());
            }
        }
        applied
    }
}

/// The writes startup makes once the instances are built, each recorded in the params history
/// with startup as its operator.
struct StartupWrites<'a> {
    store: &'a dyn ModuleStore,
    limits: &'a Limits,
    now: &'a str,
}

impl StartupWrites<'_> {
    fn change<'c>(&self, operator: &'c str, reason: &'c str) -> Change<'c> {
        Change {
            operator,
            reason,
            at: self.now.to_string(),
        }
    }

    async fn enable_required(&self, module_ids: &[String]) -> Result<(), String> {
        for module_id in module_ids {
            self.store
                .set_enabled(
                    module_id,
                    true,
                    &self.change(
                        "catalog",
                        "the catalog declares the instance enabled and not operator-disableable",
                    ),
                )
                .await?;
        }
        Ok(())
    }

    /// Seeds every catalog instance without a stored row: with the module's normalized form of
    /// the catalog params when it is storable, an empty object for file-managed params.
    async fn seed_new(
        &self,
        catalog: &Catalog,
        stored: &Stored,
        normalized: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        for config in catalog
            .instances
            .iter()
            .filter(|config| !stored.contains(&config.id))
        {
            let seed = if config.file_managed_params {
                serde_json::json!({})
            } else {
                normalized
                    .get(&config.id)
                    .filter(|params| storable(self.limits, params).is_ok())
                    .unwrap_or(&config.params)
                    .clone()
            };
            self.store
                .seed_instance(&config.id, config.enabled, &seed, self.now)
                .await?;
        }
        Ok(())
    }

    /// Stores the form a module normalized stored global params into, only while they are still
    /// the value startup read, so an operator's newer write is never overwritten.
    async fn store_migrations(
        &self,
        instances: &[Instance],
        stored: &Stored,
        normalized: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        for instance in instances
            .iter()
            .filter(|instance| !instance.config.file_managed_params)
        {
            let (Some(params), Some(read)) = (
                normalized.get(&instance.config.id),
                stored.global_params(&instance.config.id),
            ) else {
                continue;
            };
            if let Err(error) = storable(self.limits, params) {
                tracing::warn!(module = %instance.config.id, %error, "Module's normalized params break the params rules; kept the stored params");
                continue;
            }
            let operator = format!(
                "module:{}",
                instance.build.as_deref().unwrap_or(&instance.config.kind)
            );
            let migrated = self
                .store
                .set_global_if(
                    &instance.config.id,
                    read,
                    params.clone(),
                    &self.change(&operator, "params migrated by the module"),
                )
                .await?;
            if migrated.is_some() {
                tracing::info!(module = %instance.config.id, %operator, "Module normalized its stored global params; stored the migrated form");
            } else {
                tracing::warn!(module = %instance.config.id, "Global params changed while the module migrated them; kept the newer value, migration retries next start");
            }
        }
        Ok(())
    }
}

impl ModulePipeline {
    /// Reconciles the catalog with the stored state and builds the pipeline (see the module
    /// documentation for the steps).
    pub async fn new(
        network_id: &'static str,
        mut catalog: Catalog,
        registry: &Registry,
        store: Arc<dyn ModuleStore>,
        now: String,
        limits: Limits,
    ) -> Result<Self, String> {
        for config in &catalog.instances {
            limits
                .check_params_size(&config.params)
                .map_err(|e| format!("module {}: catalog {e}", config.id))?;
        }
        let stored = Stored::read(store.as_ref()).await?;
        let applied = stored.apply_to(&mut catalog);
        let Instantiated {
            instances,
            normalized,
            unreachable,
        } = registry
            .instantiate(&catalog, &applied.effective, &stored.known())
            .await?;
        let writes = StartupWrites {
            store: store.as_ref(),
            limits: &limits,
            now: &now,
        };
        writes.enable_required(&applied.enabled_by_catalog).await?;
        writes.seed_new(&catalog, &stored, &normalized).await?;
        writes
            .store_migrations(&instances, &stored, &normalized)
            .await?;
        let breakers = Arc::new(Breakers::new(limits.breaker_failures));
        for module_id in &unreachable {
            breakers.hold_open(module_id);
        }
        Ok(Self {
            instances,
            store,
            cache: RwLock::new(Cache::default()),
            breakers,
            unchecked: std::sync::Mutex::new(unreachable),
            limits,
            catalog_hash: catalog.hash,
            network_id,
            seed_drift: applied.seed_drift,
            assignment_report: std::sync::Mutex::new((0, 0)),
        })
    }
}

#[cfg(test)]
mod tests {
    use guardian_module_sdk::HookRequest;

    use super::super::*;
    use crate::modules::breaker::CircuitState;
    use crate::modules::catalog::{Catalog, HookName};
    use crate::modules::registry::Registry;
    use crate::modules::store::ModuleStore;

    const HOOK: HookName = HookName::BeforeTransactionApproved;

    async fn run(pipeline: &ModulePipeline) -> Result<(), GuardianError> {
        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
    }

    fn caps_catalog(params: &str) -> Catalog {
        Catalog::parse(
            &format!(
                r#"
[[module]]
id = "caps"
kind = "spending-limit"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = {params}
"#
            ),
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn invalid_catalog_params_stop_startup_without_seeding_them() {
        let store: Arc<dyn ModuleStore> =
            Arc::new(crate::modules::store::FsModuleStore::in_memory());
        let registry = Registry::first_party();
        let now = "2026-10-15T00:00:00Z".to_string();
        let started = ModulePipeline::new(
            "local",
            caps_catalog(r#"{ max_per_transactionx = {} }"#),
            &registry,
            store.clone(),
            now.clone(),
            crate::modules::limits::Limits::default(),
        )
        .await;
        assert!(started.is_err());
        assert!(store.instances().await.unwrap().is_empty());

        let fixed = caps_catalog(
            r#"{ max_per_transaction = { "0x0000000000000000000000000000aa" = "5" } }"#,
        );
        let started = ModulePipeline::new(
            "local",
            fixed,
            &registry,
            store.clone(),
            now,
            crate::modules::limits::Limits::default(),
        )
        .await
        .expect("the corrected catalog starts");
        assert!(
            started.instances()[0]
                .build
                .as_deref()
                .is_some_and(|build| build.starts_with("spending-limit"))
        );
        assert_eq!(store.instances().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stored_params_not_the_stale_catalog_seed_are_checked_at_startup() {
        let store: Arc<dyn ModuleStore> =
            Arc::new(crate::modules::store::FsModuleStore::in_memory());
        let registry = Registry::first_party();
        let now = "2026-10-15T00:00:00Z".to_string();
        store
            .seed_instance(
                "caps",
                true,
                &serde_json::json!({ "max_per_transaction": {} }),
                &now,
            )
            .await
            .unwrap();
        ModulePipeline::new(
            "local",
            caps_catalog(r#"{ max_per_transactionx = {} }"#),
            &registry,
            store.clone(),
            now.clone(),
            crate::modules::limits::Limits::default(),
        )
        .await
        .expect("the stored params apply, so the stale seed is not checked");

        store
            .set_global(
                "caps",
                serde_json::json!({ "unknown": true }),
                &crate::modules::store::Change {
                    operator: "0xop",
                    reason: "test",
                    at: now.clone(),
                },
            )
            .await
            .unwrap();
        let valid_seed = caps_catalog(r#"{ max_per_transaction = {} }"#);
        assert!(
            ModulePipeline::new(
                "local",
                valid_seed,
                &registry,
                store,
                now,
                crate::modules::limits::Limits::default()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn normalized_params_that_break_the_params_rules_are_not_stored() {
        use crate::modules::testing::{FixedFactory, ProtocolStub};
        let catalog = Catalog::parse(
            r#"
[[module]]
id = "migrating"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = { old_name = 5 }
"#,
            None,
        )
        .unwrap();
        for normalized in [
            serde_json::json!({ "new_name": "x".repeat(200) }),
            serde_json::json!({ "key": { "$secret": "vendor-key" } }),
        ] {
            let mut stub = ProtocolStub::new();
            stub.normalized = Some(normalized.clone());
            let mut registry = Registry::first_party();
            registry
                .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
                .unwrap();
            let store: Arc<dyn ModuleStore> =
                Arc::new(crate::modules::store::FsModuleStore::in_memory());
            ModulePipeline::new(
                "local",
                catalog.clone(),
                &registry,
                store.clone(),
                "2026-10-16T00:00:00Z".to_string(),
                crate::modules::limits::Limits {
                    params_max_bytes: 100,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                store.instances().await.unwrap()[0].global_params,
                serde_json::json!({ "old_name": 5 }),
                "kept the catalog params instead of {normalized}"
            );
        }
    }

    const OUTAGE_CATALOG: &str = r#"
[[module]]
id = "screening"
kind = "outage-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
"#;

    async fn start_with(
        module: Arc<crate::modules::testing::Outage>,
        catalog: &str,
        store: Arc<dyn ModuleStore>,
    ) -> Result<ModulePipeline, String> {
        let mut registry = Registry::first_party();
        registry
            .register(
                "outage-stub",
                Arc::new(crate::modules::testing::FixedFactory(module)),
            )
            .unwrap();
        ModulePipeline::new(
            "local",
            Catalog::parse(catalog, None).unwrap(),
            &registry,
            store,
            "2026-11-10T00:00:00Z".to_string(),
            crate::modules::limits::Limits::default(),
        )
        .await
    }

    #[tokio::test]
    async fn a_known_instance_whose_module_is_down_starts_unavailable_and_recovers_on_a_probe() {
        use std::sync::atomic::Ordering;
        let module = crate::modules::testing::Outage::new(false);
        let store: Arc<dyn ModuleStore> =
            Arc::new(crate::modules::store::FsModuleStore::in_memory());
        start_with(module.clone(), OUTAGE_CATALOG, store.clone())
            .await
            .expect("the first start passes the checks and makes the instance known");

        module.set_down(true);
        let pipeline = start_with(module.clone(), OUTAGE_CATALOG, store)
            .await
            .expect("a module outage does not keep the Guardian from restarting");
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
        assert!(
            matches!(
                run(&pipeline).await,
                Err(GuardianError::ModuleUnavailable { .. })
            ),
            "its accounts are refused meanwhile, not approved"
        );

        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Open,
            "still down"
        );

        module.set_down(false);
        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::HalfOpen
        );
        run(&pipeline).await.expect("the module decides again");
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Closed);
        assert_eq!(module.evaluations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_instance_new_to_this_guardian_must_answer_at_startup() {
        let module = crate::modules::testing::Outage::new(true);
        let store: Arc<dyn ModuleStore> =
            Arc::new(crate::modules::store::FsModuleStore::in_memory());
        let refused = start_with(module, OUTAGE_CATALOG, store.clone())
            .await
            .err()
            .expect("a wrong endpoint in a new catalog entry is found at deploy time");
        assert!(refused.contains("screening"), "{refused}");
        assert!(store.instances().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_known_instance_that_answers_incompatibly_still_stops_startup() {
        use crate::modules::testing::{FixedFactory, ProtocolStub};
        const CATALOG: &str = r#"
[[module]]
id = "screening"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
"#;
        let start = |protocol_version: u32, store: Arc<dyn ModuleStore>| async move {
            let mut stub = ProtocolStub::new();
            stub.protocol_version = protocol_version;
            let mut registry = Registry::first_party();
            registry
                .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
                .unwrap();
            ModulePipeline::new(
                "local",
                Catalog::parse(CATALOG, None).unwrap(),
                &registry,
                store,
                "2026-11-10T00:00:00Z".to_string(),
                crate::modules::limits::Limits::default(),
            )
            .await
        };
        let store: Arc<dyn ModuleStore> =
            Arc::new(crate::modules::store::FsModuleStore::in_memory());
        start(guardian_module_sdk::PROTOCOL_VERSION, store.clone())
            .await
            .unwrap();
        assert!(
            start(guardian_module_sdk::PROTOCOL_VERSION + 1, store)
                .await
                .is_err(),
            "only a module that does not answer may start unavailable"
        );
    }

    #[tokio::test]
    async fn an_instance_the_catalog_requires_is_enabled_at_startup() {
        let disabled_in_store = || async {
            let store: Arc<dyn ModuleStore> =
                Arc::new(crate::modules::store::FsModuleStore::in_memory());
            store
                .seed_instance("screening", false, &serde_json::json!({}), "t0")
                .await
                .unwrap();
            store
        };

        let store = disabled_in_store().await;
        let pipeline = start_with(
            crate::modules::testing::Outage::new(false),
            OUTAGE_CATALOG,
            store.clone(),
        )
        .await
        .unwrap();
        assert!(
            store.instances().await.unwrap()[0].enabled,
            "enabled and not operator-disableable in the catalog: a stored disable does not stand"
        );
        let history = store.history("screening", None, 10).await.unwrap();
        assert_eq!(history[0].operator, "catalog");
        assert_eq!(history[0].new, serde_json::json!({ "enabled": true }));
        assert!(
            pipeline
                .applies(HookName::BeforeTransactionApproved, "0xacc")
                .await
                .unwrap()
        );

        let store = disabled_in_store().await;
        let disableable = format!("{OUTAGE_CATALOG}operator_can_disable = true\n");
        let pipeline = start_with(
            crate::modules::testing::Outage::new(false),
            &disableable,
            store.clone(),
        )
        .await
        .unwrap();
        assert!(
            !store.instances().await.unwrap()[0].enabled,
            "an operator may hold a disableable instance off, so the stored flag stands"
        );
        assert!(
            !pipeline
                .applies(HookName::BeforeTransactionApproved, "0xacc")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn startup_stores_params_the_module_migrated() {
        use crate::modules::testing::{FixedFactory, ProtocolStub};
        let catalog = || {
            Catalog::parse(
                r#"
[[module]]
id = "migrating"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = { old_name = 5 }
"#,
                None,
            )
            .unwrap()
        };
        let mut stub = ProtocolStub::new();
        stub.normalized = Some(serde_json::json!({ "new_name": 5 }));
        let mut registry = Registry::first_party();
        registry
            .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
            .unwrap();
        let store: Arc<dyn ModuleStore> =
            Arc::new(crate::modules::store::FsModuleStore::in_memory());
        let now = "2026-10-16T00:00:00Z".to_string();

        ModulePipeline::new(
            "local",
            catalog(),
            &registry,
            store.clone(),
            now.clone(),
            crate::modules::limits::Limits::default(),
        )
        .await
        .unwrap();
        let seeded = store.instances().await.unwrap();
        assert_eq!(
            seeded[0].global_params,
            serde_json::json!({ "new_name": 5 }),
            "a new instance is seeded in the migrated shape"
        );

        store
            .set_global(
                "migrating",
                serde_json::json!({ "old_name": 7 }),
                &crate::modules::store::Change {
                    operator: "0xop",
                    reason: "old shape",
                    at: now.clone(),
                },
            )
            .await
            .unwrap();
        ModulePipeline::new(
            "local",
            catalog(),
            &registry,
            store.clone(),
            now,
            crate::modules::limits::Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            store.instances().await.unwrap()[0].global_params,
            serde_json::json!({ "new_name": 5 })
        );
        let last = store.history("migrating", None, 1).await.unwrap().remove(0);
        assert_eq!(last.operator, "module:protocol-stub 1");
        assert_eq!(last.reason, "params migrated by the module");
        assert_eq!(last.previous, serde_json::json!({ "old_name": 7 }));
    }

    /// An operator lowers the stored params while the module is migrating them at startup;
    /// the migration must not replace the operator's newer value with its stale result.
    #[tokio::test]
    async fn a_startup_migration_never_overwrites_a_newer_operator_write() {
        use crate::modules::testing::{FixedFactory, ProtocolStub, RacingStore, RacingWrite};
        let catalog = Catalog::parse(
            r#"
[[module]]
id = "migrating"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = { old_name = 1000 }
"#,
            None,
        )
        .unwrap();
        let mut stub = ProtocolStub::new();
        stub.normalized = Some(serde_json::json!({ "new_name": 1000 }));
        let mut registry = Registry::first_party();
        registry
            .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
            .unwrap();
        let store = Arc::new(RacingStore::new());
        store
            .inner
            .seed_instance(
                "migrating",
                true,
                &serde_json::json!({ "old_name": 1000 }),
                "2026-10-16T00:00:00Z",
            )
            .await
            .unwrap();
        *store.before_set_global_if.lock().unwrap() = vec![RacingWrite::Global(
            "migrating",
            serde_json::json!({ "new_name": 100 }),
        )];

        ModulePipeline::new(
            "local",
            catalog,
            &registry,
            store.clone(),
            "2026-10-16T00:00:02Z".into(),
            crate::modules::limits::Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            store.instances().await.unwrap()[0].global_params,
            serde_json::json!({ "new_name": 100 }),
            "the operator's cap of 100 stays"
        );
        let last = store.history("migrating", None, 1).await.unwrap().remove(0);
        assert_eq!(
            last.operator, "0xracing-operator",
            "no migration entry after it"
        );
    }
}
