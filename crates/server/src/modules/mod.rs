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
) -> Result<Option<Arc<ModulePipeline>>, String> {
    let Some(path) = std::env::var_os(CONFIG_ENV).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let catalog_path = PathBuf::from(path);
    let mut catalog = Catalog::load(&catalog_path)?;
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
    let pipeline = ModulePipeline::new(catalog, registry, store, now, limits).await?;
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
