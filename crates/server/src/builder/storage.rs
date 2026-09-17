use std::path::PathBuf;
use std::sync::Arc;

#[cfg(not(feature = "postgres"))]
use crate::audit::LogAuditor;
#[cfg(feature = "postgres")]
use crate::audit::PostgresAuditor;
use crate::audit::SharedAuditor;
use crate::metadata::MetadataStore;
#[cfg(not(feature = "postgres"))]
use crate::metadata::filesystem::FilesystemMetadataStore;
#[cfg(feature = "postgres")]
use crate::metadata::postgres::PostgresMetadataStore;
use crate::secret::{CredentialUrl, SecretString};
use crate::storage::StorageBackend;
use crate::storage::encryption::cipher::{Aes256GcmCipher, StorageCipher};
use crate::storage::encryption::decorator::EncryptedStorage;
use crate::storage::encryption::key_provider::{
    DEFAULT_KID, ENV_KEY, ENV_KEY_FILE, ENV_KEY_ID, ENV_SECRET_ID, InMemoryKeyProvider,
    KeyProviderError, StorageKeyProvider,
};
use crate::storage::encryption::marker::{MarkerStore, apply_startup_guard};
#[cfg(not(feature = "postgres"))]
use crate::storage::filesystem::FilesystemService;
#[cfg(feature = "postgres")]
use crate::storage::postgres::{self, PostgresService};

#[cfg(feature = "postgres")]
const ENV_DB_POOL_MAX_SIZE: &str = "GUARDIAN_DB_POOL_MAX_SIZE";
#[cfg(feature = "postgres")]
const ENV_METADATA_DB_POOL_MAX_SIZE: &str = "GUARDIAN_METADATA_DB_POOL_MAX_SIZE";

/// Builder for creating the storage backend and metadata store.
#[derive(Default)]
pub struct StorageMetadataBuilder {
    storage_path: Option<PathBuf>,
    metadata_path: Option<PathBuf>,
    database_url: Option<CredentialUrl>,
    database_pool_max_size: Option<usize>,
    metadata_pool_max_size: Option<usize>,
}

impl StorageMetadataBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn storage_path(mut self, path: PathBuf) -> Self {
        self.storage_path = Some(path);
        self
    }

    pub fn metadata_path(mut self, path: PathBuf) -> Self {
        self.metadata_path = Some(path);
        self
    }

    pub(crate) fn database_url(mut self, url: Option<CredentialUrl>) -> Self {
        self.database_url = url;
        self
    }

    pub fn database_pool_max_size(mut self, pool_max_size: usize) -> Self {
        self.database_pool_max_size = Some(pool_max_size);
        self
    }

    pub fn metadata_pool_max_size(mut self, pool_max_size: usize) -> Self {
        self.metadata_pool_max_size = Some(pool_max_size);
        self
    }

    pub fn from_env() -> Self {
        Self::new()
            .storage_path(
                std::env::var("GUARDIAN_STORAGE_PATH")
                    .unwrap_or_else(|_| "/var/guardian/storage".to_string())
                    .into(),
            )
            .metadata_path(
                std::env::var("GUARDIAN_METADATA_PATH")
                    .unwrap_or_else(|_| "/var/guardian/metadata".to_string())
                    .into(),
            )
            .database_url(
                std::env::var("DATABASE_URL")
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .map(CredentialUrl::new),
            )
    }

    pub async fn build(
        self,
    ) -> Result<
        (
            Arc<dyn StorageBackend>,
            Arc<dyn MetadataStore>,
            SharedAuditor,
            crate::coordination::CoordinationHandles,
        ),
        String,
    > {
        #[cfg(feature = "postgres")]
        {
            let database_url = self
                .database_url
                .filter(|url| !url.expose_secret().trim().is_empty())
                .ok_or_else(|| "DATABASE_URL environment variable is required".to_string())?;
            let database_pool_max_size = resolve_storage_pool_size(self.database_pool_max_size)?;
            let metadata_pool_max_size = resolve_pool_size(
                self.metadata_pool_max_size,
                ENV_METADATA_DB_POOL_MAX_SIZE,
                database_pool_max_size,
            )?;

            let raw_url = database_url.expose_secret();
            let migration_url = postgres::preflight_tls(raw_url)?;
            postgres::run_migrations(&migration_url).await?;
            let storage = PostgresService::new(raw_url, database_pool_max_size).await?;
            let metadata = PostgresMetadataStore::new(raw_url, metadata_pool_max_size).await?;
            let auditor: SharedAuditor = Arc::new(PostgresAuditor::new(metadata.pool_handle()));

            let (storage, cipher) = wrap_with_encryption(storage).await?;
            let holder_id = format!("{}-{:016x}", std::process::id(), rand::random::<u64>());
            let coordination = crate::coordination::CoordinationHandles::postgres(
                metadata.pool_handle(),
                holder_id,
                cipher,
            );

            Ok((storage, Arc::new(metadata), auditor, coordination))
        }

        #[cfg(not(feature = "postgres"))]
        {
            reject_filesystem_in_prod(
                crate::config::stage::is_prod().map_err(|error| error.to_string())?,
            )?;
            let storage_path = self
                .storage_path
                .ok_or_else(|| "GUARDIAN_STORAGE_PATH is required".to_string())?;
            let metadata_path = self
                .metadata_path
                .ok_or_else(|| "GUARDIAN_METADATA_PATH is required".to_string())?;

            let storage = FilesystemService::new(storage_path).await?;
            let metadata = FilesystemMetadataStore::new(metadata_path).await?;
            // Filesystem-only deployment: no Postgres for `admin_actions`.
            // Fall back to structured logs (FR-021) and emit a loud
            // one-shot startup warning so the operational gap is
            // visible in deployment logs.
            tracing::warn!(
                target: "audit.admin_action.startup",
                "audit events will not be persisted (filesystem backend); structured logs only",
            );
            let auditor: SharedAuditor = Arc::new(LogAuditor::new());

            // The in-memory stats store never touches disk, so the
            // cipher is not needed on this path.
            let (storage, _cipher) = wrap_with_encryption(storage).await?;
            let coordination = crate::coordination::CoordinationHandles::in_memory();

            Ok((storage, Arc::new(metadata), auditor, coordination))
        }
    }
}

/// The filesystem backend is local to one task and cannot be shared across
/// replicas, so it is refused in the prod stage. It remains the default for
/// local development and tests.
#[cfg(not(feature = "postgres"))]
fn reject_filesystem_in_prod(is_prod: bool) -> Result<(), String> {
    if is_prod {
        return Err(
            "the filesystem storage backend is not supported in the prod stage \
                    (GUARDIAN_ENV=prod): it is single-instance only and cannot be shared across \
                    replicas. Use the Postgres image and set DATABASE_URL."
                .to_string(),
        );
    }
    Ok(())
}

/// Wrap `storage` in the encryption decorator when a key source is
/// configured. Also returns the cipher so other at-rest writers that
/// bypass `StorageBackend` (the published `/dashboard/stats` snapshot)
/// seal their payloads under the same key material.
async fn wrap_with_encryption<S>(
    storage: S,
) -> Result<(Arc<dyn StorageBackend>, Option<Arc<dyn StorageCipher>>), String>
where
    S: StorageBackend + MarkerStore + 'static,
{
    let provider = resolve_storage_key_provider().await?;
    apply_startup_guard(&storage, provider.as_deref()).await?;
    match provider {
        Some(provider) => {
            let cipher: Arc<dyn StorageCipher> = Arc::new(Aes256GcmCipher::new(provider));
            let inner: Arc<dyn StorageBackend> = Arc::new(storage);
            Ok((
                Arc::new(EncryptedStorage::new(inner, cipher.clone())),
                Some(cipher),
            ))
        }
        None => Ok((Arc::new(storage), None)),
    }
}

/// Where the storage encryption key material comes from. Exactly one source
/// may be configured; presence of any source turns encryption on.
enum StorageKeySource {
    DirectKey(SecretString),
    DocumentFile(PathBuf),
    SecretsManager(String),
}

impl StorageKeySource {
    fn from_env() -> Result<Option<Self>, String> {
        let configured = [
            non_empty_env(ENV_KEY).map(|key| Self::DirectKey(SecretString::new(key))),
            non_empty_env(ENV_KEY_FILE).map(|path| Self::DocumentFile(PathBuf::from(path))),
            non_empty_env(ENV_SECRET_ID).map(Self::SecretsManager),
        ];
        let mut sources = configured.into_iter().flatten();
        let source = sources.next();
        if sources.next().is_some() {
            return Err(KeyProviderError::MultipleKeySources.to_string());
        }
        Ok(source)
    }

    async fn load(self) -> Result<InMemoryKeyProvider, String> {
        match self {
            Self::DirectKey(key) => {
                let kid = non_empty_env(ENV_KEY_ID).unwrap_or_else(|| DEFAULT_KID.to_string());
                InMemoryKeyProvider::from_dev_key(key.expose_secret(), &kid)
                    .map_err(|e| e.to_string())
            }
            Self::DocumentFile(path) => {
                InMemoryKeyProvider::from_document_file(&path).map_err(|e| e.to_string())
            }
            Self::SecretsManager(secret_id) => {
                let secret = fetch_secret_document(&secret_id).await?;
                InMemoryKeyProvider::from_secret_json(secret.expose_secret())
                    .map_err(|e| e.to_string())
            }
        }
    }
}

async fn resolve_storage_key_provider() -> Result<Option<Arc<dyn StorageKeyProvider>>, String> {
    match StorageKeySource::from_env()? {
        Some(source) => {
            let provider: Arc<dyn StorageKeyProvider> = Arc::new(source.load().await?);
            Ok(Some(provider))
        }
        None => Ok(None),
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn fetch_secret_document(secret_id: &str) -> Result<SecretString, String> {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let client = aws_sdk_secretsmanager::Client::new(&config);
    let response = client
        .get_secret_value()
        .secret_id(secret_id)
        .send()
        .await
        .map_err(|e| {
            KeyProviderError::KeyStoreUnavailable(format!("secret {secret_id}: {e}")).to_string()
        })?;
    response
        .secret_string()
        .map(|s| SecretString::new(s.to_owned()))
        .ok_or_else(|| format!("Storage encryption secret {secret_id} has no string value"))
}

/// Storage pool size: an explicit builder value wins, then
/// `GUARDIAN_DB_POOL_MAX_SIZE`, then the [`Stage`] default (`32` in prod, `16`
/// otherwise). The metadata pool follows this value unless set separately.
#[cfg(feature = "postgres")]
fn resolve_storage_pool_size(configured_value: Option<usize>) -> Result<usize, String> {
    let stage = crate::config::stage::Stage::from_env().map_err(|error| error.to_string())?;
    resolve_pool_size(
        configured_value,
        ENV_DB_POOL_MAX_SIZE,
        stage.default_db_pool_max_size(),
    )
}

#[cfg(feature = "postgres")]
fn resolve_pool_size(
    configured_value: Option<usize>,
    env_var_name: &str,
    default_value: usize,
) -> Result<usize, String> {
    match configured_value {
        Some(pool_max_size) => validate_pool_size(pool_max_size, env_var_name),
        None => match std::env::var(env_var_name) {
            Ok(value) => {
                let pool_max_size = value.parse::<usize>().map_err(|_| {
                    format!("{env_var_name} must be a positive integer, got '{value}'")
                })?;
                validate_pool_size(pool_max_size, env_var_name)
            }
            Err(std::env::VarError::NotPresent) => Ok(default_value),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{env_var_name} must contain valid UTF-8"))
            }
        },
    }
}

#[cfg(feature = "postgres")]
fn validate_pool_size(pool_max_size: usize, env_var_name: &str) -> Result<usize, String> {
    if pool_max_size == 0 {
        return Err(format!("{env_var_name} must be greater than zero"));
    }

    Ok(pool_max_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    static ENCRYPTION_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EncEnvGuard;

    impl EncEnvGuard {
        fn clear() -> Self {
            for key in [ENV_KEY, ENV_KEY_ID, ENV_KEY_FILE, ENV_SECRET_ID] {
                // SAFETY: serialized by ENCRYPTION_ENV_LOCK
                unsafe { std::env::remove_var(key) };
            }
            Self
        }
        fn set(self, key: &str, value: &str) -> Self {
            // SAFETY: serialized by ENCRYPTION_ENV_LOCK
            unsafe { std::env::set_var(key, value) };
            self
        }
    }

    impl Drop for EncEnvGuard {
        fn drop(&mut self) {
            for key in [ENV_KEY, ENV_KEY_ID, ENV_KEY_FILE, ENV_SECRET_ID] {
                // SAFETY: serialized by ENCRYPTION_ENV_LOCK
                unsafe { std::env::remove_var(key) };
            }
        }
    }

    #[tokio::test]
    async fn resolve_provider_none_when_no_key_source() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear();
        assert!(resolve_storage_key_provider().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn resolve_provider_from_dev_key() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear().set(ENV_KEY, &BASE64.encode([5u8; 32]));
        let provider = resolve_storage_key_provider().await.unwrap().unwrap();
        assert_eq!(provider.active_key_id(), DEFAULT_KID);
    }

    #[tokio::test]
    async fn resolve_provider_honors_custom_kid() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear()
            .set(ENV_KEY, &BASE64.encode([5u8; 32]))
            .set(ENV_KEY_ID, "primary");
        let provider = resolve_storage_key_provider().await.unwrap().unwrap();
        assert_eq!(provider.active_key_id(), "primary");
    }

    #[tokio::test]
    async fn resolve_provider_rejects_invalid_dev_key() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear().set(ENV_KEY, "not-base64!!!");
        assert!(resolve_storage_key_provider().await.is_err());
    }

    #[tokio::test]
    async fn resolve_provider_rejects_short_dev_key() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear().set(ENV_KEY, &BASE64.encode([5u8; 16]));
        assert!(resolve_storage_key_provider().await.is_err());
    }

    #[tokio::test]
    async fn resolve_provider_rejects_multiple_sources() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear()
            .set(ENV_KEY, &BASE64.encode([5u8; 32]))
            .set(ENV_SECRET_ID, "some/secret/id");
        let result = resolve_storage_key_provider().await;
        assert!(matches!(&result, Err(message) if message.contains("more than one")));
    }

    fn write_key_document(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("keys.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"active":"k2","keys":{{"k1":"{}","k2":"{}"}}}}"#,
                BASE64.encode([1u8; 32]),
                BASE64.encode([2u8; 32])
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn resolve_provider_from_document_file() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = write_key_document(dir.path());
        let _env = EncEnvGuard::clear().set(ENV_KEY_FILE, path.to_str().unwrap());
        let provider = resolve_storage_key_provider().await.unwrap().unwrap();
        assert_eq!(provider.active_key_id(), "k2");
        assert_eq!(provider.key("k1").unwrap().expose_secret(), &[1u8; 32]);
    }

    #[tokio::test]
    async fn resolve_provider_rejects_document_file_alongside_direct_key() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = write_key_document(dir.path());
        let _env = EncEnvGuard::clear()
            .set(ENV_KEY, &BASE64.encode([5u8; 32]))
            .set(ENV_KEY_FILE, path.to_str().unwrap());
        let result = resolve_storage_key_provider().await;
        assert!(matches!(&result, Err(message) if message.contains("more than one")));
    }

    #[tokio::test]
    async fn resolve_provider_reports_missing_document_file() {
        let _lock = ENCRYPTION_ENV_LOCK.lock().await;
        let _env = EncEnvGuard::clear().set(ENV_KEY_FILE, "/nonexistent/guardian-keys.json");
        let result = resolve_storage_key_provider().await;
        assert!(matches!(&result, Err(message) if message.contains("unavailable")));
    }

    #[test]
    fn test_new_creates_empty_builder() {
        let builder = StorageMetadataBuilder::new();
        assert!(builder.storage_path.is_none());
        assert!(builder.metadata_path.is_none());
        assert!(builder.database_url.is_none());
        assert!(builder.database_pool_max_size.is_none());
        assert!(builder.metadata_pool_max_size.is_none());
    }

    #[test]
    fn test_default_creates_empty_builder() {
        let builder = StorageMetadataBuilder::default();
        assert!(builder.storage_path.is_none());
        assert!(builder.metadata_path.is_none());
        assert!(builder.database_url.is_none());
        assert!(builder.database_pool_max_size.is_none());
        assert!(builder.metadata_pool_max_size.is_none());
    }

    #[test]
    fn test_storage_path_sets_path() {
        let path = PathBuf::from("/test/storage");
        let builder = StorageMetadataBuilder::new().storage_path(path.clone());
        assert_eq!(builder.storage_path, Some(path));
    }

    #[test]
    fn test_metadata_path_sets_path() {
        let path = PathBuf::from("/test/metadata");
        let builder = StorageMetadataBuilder::new().metadata_path(path.clone());
        assert_eq!(builder.metadata_path, Some(path));
    }

    #[test]
    fn test_database_url_sets_url() {
        let url = "postgres://localhost/test".to_string();
        let builder =
            StorageMetadataBuilder::new().database_url(Some(CredentialUrl::new(url.clone())));
        assert_eq!(
            builder.database_url.as_ref().map(|u| u.expose_secret()),
            Some(url.as_str())
        );
    }

    #[test]
    fn test_database_pool_max_size_sets_value() {
        let builder = StorageMetadataBuilder::new().database_pool_max_size(24);
        assert_eq!(builder.database_pool_max_size, Some(24));
    }

    #[test]
    fn test_metadata_pool_max_size_sets_value() {
        let builder = StorageMetadataBuilder::new().metadata_pool_max_size(12);
        assert_eq!(builder.metadata_pool_max_size, Some(12));
    }

    #[test]
    fn test_builder_chaining() {
        let storage_path = PathBuf::from("/test/storage");
        let metadata_path = PathBuf::from("/test/metadata");
        let database_url = "postgres://localhost/test".to_string();

        let builder = StorageMetadataBuilder::new()
            .storage_path(storage_path.clone())
            .metadata_path(metadata_path.clone())
            .database_url(Some(CredentialUrl::new(database_url.clone())))
            .database_pool_max_size(24)
            .metadata_pool_max_size(12);

        assert_eq!(builder.storage_path, Some(storage_path));
        assert_eq!(builder.metadata_path, Some(metadata_path));
        assert_eq!(
            builder.database_url.as_ref().map(|u| u.expose_secret()),
            Some(database_url.as_str())
        );
        assert_eq!(builder.database_pool_max_size, Some(24));
        assert_eq!(builder.metadata_pool_max_size, Some(12));
    }

    #[test]
    fn test_from_env_returns_builder_with_paths() {
        // Test that from_env returns a builder with storage_path and metadata_path set.
        // database_url is intentionally None when DATABASE_URL is unset or empty —
        // the previous "always Some(empty string)" behavior masked a real-absent value.
        let builder = StorageMetadataBuilder::from_env();

        assert!(builder.storage_path.is_some());
        assert!(builder.metadata_path.is_some());
        match std::env::var("DATABASE_URL") {
            Ok(value) if !value.is_empty() => {
                assert!(builder.database_url.is_some());
            }
            _ => {
                assert!(builder.database_url.is_none());
            }
        }
    }

    #[cfg(not(feature = "postgres"))]
    #[test]
    fn filesystem_rejected_in_prod_stage() {
        assert!(
            reject_filesystem_in_prod(true).is_err(),
            "prod stage must refuse the filesystem backend"
        );
        assert!(
            reject_filesystem_in_prod(false).is_ok(),
            "non-prod tolerates the filesystem backend"
        );
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn test_build_without_storage_path_fails() {
        let builder = StorageMetadataBuilder::new().metadata_path(PathBuf::from("/test/metadata"));

        let result = builder.build().await;
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "GUARDIAN_STORAGE_PATH is required");
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn test_build_without_metadata_path_fails() {
        let builder = StorageMetadataBuilder::new().storage_path(PathBuf::from("/test/storage"));

        let result = builder.build().await;
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "GUARDIAN_METADATA_PATH is required");
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn test_build_with_valid_paths_succeeds() {
        let temp_dir = std::env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage_path = temp_dir.join("storage");
        let metadata_path = temp_dir.join("metadata");

        let builder = StorageMetadataBuilder::new()
            .storage_path(storage_path.clone())
            .metadata_path(metadata_path.clone());

        let result = builder.build().await;
        assert!(result.is_ok());

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// Feature 006-operator-authz FR-021 / SC-011 / T043: the
    /// filesystem-backed `StorageMetadataBuilder::build()` MUST emit
    /// one structured warning under
    /// `target = "audit.admin_action.startup"` informing operators
    /// that audit events flow to logs rather than to Postgres rows.
    /// Pinned text matches what release-build deployments are told.
    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn filesystem_build_emits_audit_startup_warning() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing::Level;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct CapturedWriter {
            buf: Arc<Mutex<Vec<u8>>>,
        }
        impl Write for CapturedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.buf.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for CapturedWriter {
            type Writer = CapturedWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let temp_dir = std::env::temp_dir().join(format!(
            "guardian_audit_startup_warn_{}",
            uuid::Uuid::new_v4()
        ));
        let storage_path = temp_dir.join("storage");
        let metadata_path = temp_dir.join("metadata");

        let writer = CapturedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_max_level(Level::TRACE)
            .with_ansi(false)
            .finish();

        let writer_for_assert = writer.clone();
        let _registry_pin = crate::testing::log_capture::dispatcher_registry_pin();
        tracing::subscriber::with_default(subscriber, || {
            futures::executor::block_on(async {
                let builder = StorageMetadataBuilder::new()
                    .storage_path(storage_path.clone())
                    .metadata_path(metadata_path.clone());
                let result = builder.build().await;
                assert!(result.is_ok(), "filesystem build should succeed");
            });
        });

        let captured = String::from_utf8(writer_for_assert.buf.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains("audit.admin_action.startup"),
            "expected startup-warning target in: {captured}",
        );
        assert!(
            captured.contains("audit events will not be persisted")
                && captured.contains("filesystem backend"),
            "expected pinned warning message in: {captured}",
        );

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[cfg(feature = "postgres")]
    mod postgres {
        use super::*;

        static POOL_SIZE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

        #[tokio::test]
        async fn test_build_without_database_url_fails() {
            let builder = StorageMetadataBuilder::new();

            let result = builder.build().await;
            assert!(result.is_err());
            assert_eq!(
                result.err().unwrap(),
                "DATABASE_URL environment variable is required"
            );
        }

        #[tokio::test]
        async fn test_build_with_empty_database_url_fails() {
            let builder =
                StorageMetadataBuilder::new().database_url(Some(CredentialUrl::new(String::new())));

            let result = builder.build().await;
            assert!(result.is_err());
            assert_eq!(
                result.err().unwrap(),
                "DATABASE_URL environment variable is required"
            );
        }

        #[test]
        fn test_resolve_pool_size_uses_default_when_env_missing() {
            let _lock = POOL_SIZE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            // SAFETY: serialized by POOL_SIZE_ENV_LOCK; this variable is private to this module.
            unsafe { std::env::remove_var(ENV_DB_POOL_MAX_SIZE) };
            let result = resolve_pool_size(None, ENV_DB_POOL_MAX_SIZE, 16).unwrap();
            assert_eq!(result, 16);
        }

        #[test]
        fn test_resolve_pool_size_uses_explicit_value() {
            let result = resolve_pool_size(Some(24), ENV_DB_POOL_MAX_SIZE, 16).unwrap();
            assert_eq!(result, 24);
        }

        #[test]
        fn test_resolve_pool_size_reads_env_override() {
            let _lock = POOL_SIZE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            // SAFETY: serialized by POOL_SIZE_ENV_LOCK; this variable is private to this module.
            unsafe { std::env::set_var(ENV_DB_POOL_MAX_SIZE, "32") };
            let result = resolve_pool_size(None, ENV_DB_POOL_MAX_SIZE, 16).unwrap();
            // SAFETY: serialized by POOL_SIZE_ENV_LOCK; this variable is private to this module.
            unsafe { std::env::remove_var(ENV_DB_POOL_MAX_SIZE) };
            assert_eq!(result, 32);
        }

        #[test]
        fn test_resolve_pool_size_rejects_invalid_env_override() {
            let _lock = POOL_SIZE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            // SAFETY: serialized by POOL_SIZE_ENV_LOCK; this variable is private to this module.
            unsafe { std::env::set_var(ENV_DB_POOL_MAX_SIZE, "nope") };
            let result = resolve_pool_size(None, ENV_DB_POOL_MAX_SIZE, 16);
            // SAFETY: serialized by POOL_SIZE_ENV_LOCK; this variable is private to this module.
            unsafe { std::env::remove_var(ENV_DB_POOL_MAX_SIZE) };
            assert!(result.is_err());
        }
    }

    #[cfg(feature = "postgres")]
    mod stage_pool_size {
        use super::super::*;
        use crate::testing::env_lock::ENV_LOCK;

        fn with_env<T>(pool: Option<&str>, stage: Option<&str>, f: impl FnOnce() -> T) -> T {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            // SAFETY: serialized by ENV_LOCK; both variables are removed before returning.
            unsafe {
                match pool {
                    Some(v) => std::env::set_var(ENV_DB_POOL_MAX_SIZE, v),
                    None => std::env::remove_var(ENV_DB_POOL_MAX_SIZE),
                }
                match stage {
                    Some(v) => std::env::set_var("GUARDIAN_ENV", v),
                    None => std::env::remove_var("GUARDIAN_ENV"),
                }
            }
            let result = f();
            // SAFETY: serialized by ENV_LOCK.
            unsafe {
                std::env::remove_var(ENV_DB_POOL_MAX_SIZE);
                std::env::remove_var("GUARDIAN_ENV");
            }
            result
        }

        #[test]
        fn unset_everything_is_the_dev_default() {
            assert_eq!(
                with_env(None, None, || resolve_storage_pool_size(None)).unwrap(),
                16
            );
        }

        #[test]
        fn prod_stage_raises_the_default() {
            assert_eq!(
                with_env(None, Some("prod"), || resolve_storage_pool_size(None)).unwrap(),
                32
            );
        }

        #[test]
        fn env_and_explicit_values_win_over_the_stage() {
            assert_eq!(
                with_env(Some("48"), Some("prod"), || resolve_storage_pool_size(None)).unwrap(),
                48
            );
            assert_eq!(
                with_env(Some("48"), Some("prod"), || resolve_storage_pool_size(
                    Some(8)
                ))
                .unwrap(),
                8
            );
        }
    }
}
