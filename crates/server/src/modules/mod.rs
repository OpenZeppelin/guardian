//! Policy modules: a hook pipeline the Guardian consults before approving.

pub mod admin;
pub mod breaker;
pub mod catalog;
pub mod check_limit;
pub mod disclosure;
pub mod grpc_kind;
pub mod limits;
pub mod metrics;
pub mod pipeline;
#[cfg(feature = "postgres")]
pub mod postgres_store;
pub mod registry;
pub mod request;
pub mod secrets;
pub mod store;
#[cfg(test)]
pub mod testing;

use std::path::PathBuf;
use std::sync::Arc;

pub use catalog::{CONFIG_ENV, Catalog, HookName};
pub use pipeline::ModulePipeline;
pub use registry::Registry;

/// Directory of the filesystem module tables; defaults to `<GUARDIAN_METADATA_PATH>/modules`.
pub const STATE_ENV: &str = "GUARDIAN_MODULES_STATE";

fn filesystem_store(catalog_path: &std::path::Path) -> Result<store::FsModuleStore, String> {
    let dir = std::env::var_os(STATE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("GUARDIAN_METADATA_PATH")
                .filter(|value| !value.is_empty())
                .map(|path| PathBuf::from(path).join("modules"))
        })
        .unwrap_or_else(|| catalog_path.with_extension("tables"));
    store::FsModuleStore::open(dir)
}

/// The pipeline configured by `GUARDIAN_MODULES_CONFIG`, or `None` when it is unset. `store` is
/// the backend's module tables; without one (filesystem backend) they are files.
pub async fn from_env(
    now: String,
    store: Option<Arc<dyn store::ModuleStore>>,
    registry: &Registry,
    execution_lease: std::time::Duration,
    network: crate::network::NetworkType,
) -> Result<Option<Arc<ModulePipeline>>, String> {
    load(
        std::env::var_os(CONFIG_ENV),
        now,
        store,
        registry,
        execution_lease,
        network,
    )
    .await
}

/// [`from_env`] for `config`, the value of `GUARDIAN_MODULES_CONFIG`. Only an unset variable
/// means "no modules": an empty value or a catalog that declares none is a mistake that would
/// otherwise start the server without any policy.
async fn load(
    config: Option<std::ffi::OsString>,
    now: String,
    store: Option<Arc<dyn store::ModuleStore>>,
    registry: &Registry,
    execution_lease: std::time::Duration,
    network: crate::network::NetworkType,
) -> Result<Option<Arc<ModulePipeline>>, String> {
    let Some(path) = config else {
        let stored = match &store {
            Some(store) => store.instances().await?.len(),
            None => 0,
        };
        if stored > 0 {
            tracing::warn!(
                stored_instances = stored,
                "{CONFIG_ENV} is not set: no policy module runs, although this Guardian's database holds module state from an earlier catalog"
            );
        } else {
            tracing::info!("{CONFIG_ENV} is not set: no policy module runs");
        }
        return Ok(None);
    };
    if path.is_empty() {
        return Err(format!(
            "{CONFIG_ENV} is set but empty; point it at a catalog file, or unset it to run without policy modules"
        ));
    }
    let catalog_path = PathBuf::from(path);
    let mut catalog = Catalog::load(&catalog_path)?;
    if catalog.instances.is_empty() {
        return Err(format!(
            "module catalog {} declares no module; unset {CONFIG_ENV} to run without policy modules",
            catalog_path.display()
        ));
    }
    let limits = limits::Limits::from_env()?;
    let prod = crate::config::stage::Stage::from_env()
        .map_err(|e| e.to_string())?
        .is_prod();
    if prod {
        catalog.check_production()?;
        catalog.check_budget(limits.budget, execution_lease)?;
    }
    let source = if catalog.names_secrets() {
        let catalog_dir = catalog_path.parent().unwrap_or(std::path::Path::new("."));
        Some(secrets::from_env(catalog_dir, prod).await?)
    } else {
        None
    };
    catalog.resolve_secrets(source.as_deref()).await;
    let store = match store {
        Some(store) => store,
        None => Arc::new(filesystem_store(&catalog_path)?),
    };
    let network_id = crate::metadata::network::MidenNetworkType::from(network).as_str();
    let pipeline = ModulePipeline::new(network_id, catalog, registry, store, now, limits).await?;
    let version = pipeline.store().version().await?;
    let (over_budget, orphaned) = pipeline.report_stored_assignments().await?;
    let builds = pipeline
        .instances()
        .iter()
        .map(|instance| {
            format!(
                "{}={}",
                instance.config.id,
                instance.build.as_deref().unwrap_or("unavailable")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(catalog_hash = %pipeline.catalog_hash, params_version = version, %builds, over_budget, orphaned, "Policy modules loaded");
    Ok(Some(Arc::new(pipeline)))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn loaded(config: Option<&str>) -> Result<Option<Arc<ModulePipeline>>, String> {
        load(
            config.map(Into::into),
            "2026-11-10T00:00:00Z".into(),
            Some(Arc::new(store::FsModuleStore::in_memory())),
            &Registry::first_party(),
            std::time::Duration::from_secs(60),
            crate::network::NetworkType::MidenLocal,
        )
        .await
    }

    fn catalog_file(text: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modules.toml");
        std::fs::write(&path, text).unwrap();
        let path = path.to_str().unwrap().to_string();
        (dir, path)
    }

    #[tokio::test]
    async fn only_an_unset_variable_runs_without_modules() {
        assert!(loaded(None).await.unwrap().is_none());

        let empty_value = loaded(Some(""))
            .await
            .err()
            .expect("an empty value is refused");
        assert!(empty_value.contains("is set but empty"), "{empty_value}");

        for text in ["", "# every module commented out\n"] {
            let (_dir, path) = catalog_file(text);
            let no_module = loaded(Some(&path))
                .await
                .err()
                .expect("a catalog without modules is refused");
            assert!(no_module.contains("declares no module"), "{no_module}");
        }
    }

    #[tokio::test]
    async fn a_catalog_with_a_module_loads() {
        let (_dir, path) = catalog_file(
            r#"
[[module]]
id = "caps"
kind = "basic-policy"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
"#,
        );
        let pipeline = loaded(Some(&path))
            .await
            .unwrap()
            .expect("the catalog declares a module");
        assert_eq!(pipeline.instances().len(), 1);
    }
}
