use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::metadata::MetadataStore;
use crate::schema::{account_metadata, delta_proposals, deltas, states, storage_encryption_marker};
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use crate::storage::encryption::marker::{EncryptionMarker, MarkerStore};
use crate::storage::{
    AbandonIntent, AccountDeltaCursor, AccountProposalCursor, CandidatePromotion,
    CandidateSubmission, CanonicalWrite, DeltaStatusCounts, DeltaStatusKind, GlobalDeltaCursor,
    GlobalDeltaRow, GlobalProposalCursor, LeaseFence, PromotableKind, PromoteWrite, ProposalRecord,
    StorageType,
};
use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use diesel::ConnectionError;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::ManagerConfig;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use futures_util::FutureExt;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use std::sync::{Arc, Once};
use tokio_postgres_rustls::MakeRustlsConnect;
use url::Url;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

const MIGRATION_ADVISORY_LOCK_KEY: i64 = 0x4755_4152_4449_414E;

fn postgres_timestamp_precision(timestamp: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    timestamp
        .with_nanosecond(timestamp.nanosecond() / 1_000 * 1_000)
        .ok_or_else(|| "Failed to normalize timestamp to Postgres precision".to_string())
}
const MIGRATION_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MIGRATION_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(diesel::QueryableByName)]
struct AdvisoryLockAcquired {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    acquired: bool,
}

/// Run database migrations. Call once at application startup.
///
/// Migrations run under a session advisory lock so that replicas booting
/// simultaneously serialize: the first holder migrates and the rest block,
/// then find nothing pending. The lock is released explicitly and, as a
/// backstop, on connection drop.
pub async fn run_migrations(database_url: &str) -> Result<(), String> {
    let url = database_url.to_string();
    tokio::task::spawn_blocking(move || {
        let mut conn = PgConnection::establish(&url)
            .map_err(|e| format!("Failed to connect for migrations: {e}"))?;

        let deadline = std::time::Instant::now() + MIGRATION_LOCK_TIMEOUT;
        loop {
            let attempt = diesel::RunQueryDsl::get_result::<AdvisoryLockAcquired>(
                diesel::sql_query(format!(
                    "SELECT pg_try_advisory_lock({MIGRATION_ADVISORY_LOCK_KEY}) AS acquired"
                )),
                &mut conn,
            )
            .map_err(|e| format!("Failed to attempt migration advisory lock: {e}"))?;
            if attempt.acquired {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "Timed out after {}s waiting for the migration advisory lock; \
                     another replica may be stuck mid-migration",
                    MIGRATION_LOCK_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(MIGRATION_LOCK_POLL);
        }

        let result = conn
            .run_pending_migrations(MIGRATIONS)
            .map(|_| ())
            .map_err(|e| format!("Failed to run migrations: {e}"));

        let _ = diesel::RunQueryDsl::execute(
            diesel::sql_query(format!(
                "SELECT pg_advisory_unlock({MIGRATION_ADVISORY_LOCK_KEY})"
            )),
            &mut conn,
        );

        result
    })
    .await
    .map_err(|e| format!("Migration task failed: {e}"))??;

    Ok(())
}

pub struct PostgresService {
    pool: Pool<AsyncPgConnection>,
}

impl PostgresService {
    pub async fn new(database_url: &str, pool_max_size: usize) -> Result<Self, String> {
        let pool = build_postgres_pool(database_url, pool_max_size).await?;
        Ok(Self { pool })
    }

    pub async fn with_pool(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyLevel {
    Ca,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TlsPlan {
    Disable,
    EncryptOnly,
    Verify { level: VerifyLevel, ca_path: String },
}

fn single_query_value(url: &Url, key: &str) -> Result<Option<String>, String> {
    let mut found: Option<String> = None;
    for (k, v) in url.query_pairs() {
        if k == key {
            if found.is_some() {
                return Err(format!("Duplicate '{key}' in DATABASE_URL"));
            }
            found = Some(v.into_owned());
        }
    }
    Ok(found)
}

fn parse_tls_plan(database_url: &str) -> Result<TlsPlan, String> {
    let url = Url::parse(database_url).map_err(|err| {
        format!(
            "DATABASE_URL must be a postgres:// or postgresql:// URL \
             (libpq keyword/value strings are not supported): {err}"
        )
    })?;

    match url.scheme() {
        "postgres" | "postgresql" => {}
        other => {
            return Err(format!(
                "Unsupported DATABASE_URL scheme '{other}'; expected postgres:// or postgresql://"
            ));
        }
    }

    if url.host_str().is_some_and(|host| host.contains(',')) {
        return Err("Multi-host DATABASE_URL is not supported".to_string());
    }

    let sslmode = single_query_value(&url, "sslmode")?;
    let sslrootcert = single_query_value(&url, "sslrootcert")?;

    if let Some(path) = sslrootcert.as_deref() {
        if path.is_empty() {
            return Err("sslrootcert is set but empty; provide a CA bundle file path".to_string());
        }
        if path == "system" {
            return Err(
                "sslrootcert=system (host trust store) is not supported; provide an explicit CA bundle file"
                    .to_string(),
            );
        }
    }

    let plan = match sslmode.as_deref() {
        None | Some("disable") => TlsPlan::Disable,
        Some("allow") | Some("prefer") => {
            return Err(
                "sslmode 'allow'/'prefer' is not supported (would allow plaintext fallback); use disable, require, verify-ca, or verify-full"
                    .to_string(),
            );
        }
        Some("require") => match sslrootcert {
            Some(ca_path) => TlsPlan::Verify {
                level: VerifyLevel::Ca,
                ca_path,
            },
            None => TlsPlan::EncryptOnly,
        },
        Some("verify-ca") => match sslrootcert {
            Some(ca_path) => TlsPlan::Verify {
                level: VerifyLevel::Ca,
                ca_path,
            },
            None => {
                return Err("sslmode=verify-ca requires sslrootcert=<CA bundle path>".to_string());
            }
        },
        Some("verify-full") => match sslrootcert {
            Some(ca_path) => TlsPlan::Verify {
                level: VerifyLevel::Full,
                ca_path,
            },
            None => {
                return Err("sslmode=verify-full requires sslrootcert=<CA bundle path>".to_string());
            }
        },
        Some(other) => return Err(format!("Unrecognized sslmode '{other}'")),
    };

    Ok(plan)
}

fn rebuild_url(
    database_url: &str,
    sslmode: &str,
    sslrootcert: Option<&str>,
) -> Result<String, String> {
    let mut url =
        Url::parse(database_url).map_err(|error| format!("Invalid DATABASE_URL: {error}"))?;
    let preserved: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "sslmode" && key != "sslrootcert")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (key, value) in &preserved {
            pairs.append_pair(key, value);
        }
        pairs.append_pair("sslmode", sslmode);
        if let Some(path) = sslrootcert {
            pairs.append_pair("sslrootcert", path);
        }
    }
    Ok(url.into())
}

fn normalized_sync_url(database_url: &str, plan: &TlsPlan) -> Result<String, String> {
    match plan {
        TlsPlan::Disable => rebuild_url(database_url, "disable", None),
        TlsPlan::EncryptOnly => rebuild_url(database_url, "require", None),
        TlsPlan::Verify {
            level: VerifyLevel::Ca,
            ca_path,
        } => rebuild_url(database_url, "verify-ca", Some(ca_path)),
        TlsPlan::Verify {
            level: VerifyLevel::Full,
            ca_path,
        } => rebuild_url(database_url, "verify-full", Some(ca_path)),
    }
}

fn sanitized_async_url(database_url: &str, plan: &TlsPlan) -> Result<String, String> {
    match plan {
        TlsPlan::Disable => rebuild_url(database_url, "disable", None),
        _ => rebuild_url(database_url, "require", None),
    }
}

fn load_root_store(ca_path: &str) -> Result<RootCertStore, String> {
    let file = std::fs::File::open(ca_path)
        .map_err(|error| format!("Failed to open CA bundle '{ca_path}': {error}"))?;
    let mut reader = std::io::BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Failed to parse CA bundle '{ca_path}': {error}"))?;
    if certs.is_empty() {
        return Err(format!("CA bundle '{ca_path}' contains no certificates"));
    }
    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots
            .add(cert)
            .map_err(|error| format!("Invalid certificate in CA bundle '{ca_path}': {error}"))?;
    }
    Ok(roots)
}

/// Verifies the certificate chain against the configured roots without
/// matching the server hostname, implementing `verify-ca` semantics.
#[derive(Debug)]
struct ChainOnlyVerifier {
    inner: Arc<WebPkiServerVerifier>,
}

impl ServerCertVerifier for ChainOnlyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(verified) => Ok(verified),
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            Err(other) => Err(other),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn install_rustls_provider() {
    static INSTALL_PROVIDER: Once = Once::new();

    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Debug)]
struct NoCertificateVerification;

impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

fn build_tls_client_config(plan: &TlsPlan) -> Result<Option<Arc<ClientConfig>>, String> {
    let config = match plan {
        TlsPlan::Disable => return Ok(None),
        TlsPlan::EncryptOnly => {
            install_rustls_provider();
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
                .with_no_client_auth()
        }
        TlsPlan::Verify { level, ca_path } => {
            install_rustls_provider();
            let roots = load_root_store(ca_path)?;
            match level {
                VerifyLevel::Full => ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
                VerifyLevel::Ca => {
                    let inner = WebPkiServerVerifier::builder(Arc::new(roots))
                        .build()
                        .map_err(|error| {
                            format!("Failed to build certificate verifier: {error}")
                        })?;
                    ClientConfig::builder()
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(ChainOnlyVerifier { inner }))
                        .with_no_client_auth()
                }
            }
        }
    };
    Ok(Some(Arc::new(config)))
}

/// Validate the TLS configuration in `DATABASE_URL` and return the
/// connection string for the synchronous (libpq) migration path. Runs
/// before any database connection so misconfiguration fails closed.
pub(crate) fn preflight_tls(database_url: &str) -> Result<String, String> {
    let plan = parse_tls_plan(database_url)?;
    build_tls_client_config(&plan)?;
    normalized_sync_url(database_url, &plan)
}

async fn establish_tls_connection(
    database_url: &str,
    config: Arc<ClientConfig>,
) -> diesel::ConnectionResult<AsyncPgConnection> {
    let tls = MakeRustlsConnect::new((*config).clone());
    let (client, connection) = tokio_postgres::connect(database_url, tls)
        .await
        .map_err(|error| ConnectionError::BadConnection(error.to_string()))?;

    AsyncPgConnection::try_from_client_and_connection(client, connection).await
}

fn make_connection_manager(
    database_url: &str,
) -> Result<AsyncDieselConnectionManager<AsyncPgConnection>, String> {
    let plan = parse_tls_plan(database_url)?;
    let connect_url = sanitized_async_url(database_url, &plan)?;
    let manager = match build_tls_client_config(&plan)? {
        None => AsyncDieselConnectionManager::<AsyncPgConnection>::new(connect_url),
        Some(config) => {
            let mut manager_config = ManagerConfig::default();
            manager_config.custom_setup =
                Box::new(move |url| establish_tls_connection(url, config.clone()).boxed());
            AsyncDieselConnectionManager::<AsyncPgConnection>::new_with_config(
                connect_url,
                manager_config,
            )
        }
    };
    Ok(manager)
}

pub(crate) async fn build_postgres_pool(
    database_url: &str,
    pool_max_size: usize,
) -> Result<Pool<AsyncPgConnection>, String> {
    let pool = Pool::builder(make_connection_manager(database_url)?)
        .max_size(pool_max_size)
        .build()
        .map_err(|error| format!("Failed to create connection pool: {error}"))?;

    let _ = pool
        .get()
        .await
        .map_err(|error| format!("Failed to connect to Postgres: {error}"))?;

    Ok(pool)
}

/// Build a connection pool without eagerly validating the URL. Test
/// helper used by feature-006-operator-authz fault-injection coverage
/// to construct a deliberately-broken pool whose `get()` will fail at
/// use time rather than at construction. Not exposed outside `#[cfg(test)]`.
#[cfg(test)]
pub(crate) fn build_postgres_pool_lazy(
    database_url: &str,
    pool_max_size: usize,
) -> Result<Pool<AsyncPgConnection>, String> {
    Pool::builder(make_connection_manager(database_url)?)
        .max_size(pool_max_size)
        .build()
        .map_err(|error| format!("Failed to create connection pool: {error}"))
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
    // Typed mirrors of the lifecycle status kept in `status` Jsonb.
    // Read-side optimization for dashboard queries; write-side is
    // dual-populated by Self::derive_status_columns.
    #[allow(dead_code)]
    status_kind: String,
    #[allow(dead_code)]
    status_timestamp: chrono::DateTime<chrono::Utc>,
    metadata: Option<serde_json::Value>,
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
    #[allow(dead_code)]
    status_kind: String,
    #[allow(dead_code)]
    status_timestamp: chrono::DateTime<chrono::Utc>,
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
    status_kind: &'a str,
    status_timestamp: chrono::DateTime<chrono::Utc>,
    metadata: Option<&'a serde_json::Value>,
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
    status_kind: &'a str,
    status_timestamp: chrono::DateTime<chrono::Utc>,
}

/// Decompose a [`DeltaStatus`] into the typed `(status_kind,
/// status_timestamp)` pair stored in the indexed columns alongside the
/// Jsonb `status` blob. Callers must write the Jsonb and the typed
/// columns atomically (in the same `INSERT`/`UPDATE`) to keep the two
/// representations in lock-step. A malformed or empty embedded
/// timestamp surfaces as `Err` rather than silently rewriting the
/// indexed column to wall-clock now (which would re-order the global
/// feeds and pollute `latest_activity` on every write to a legacy
/// row). Spec: feature `005-operator-dashboard-metrics`, Decision 1
/// (revised).
fn derive_status_columns(
    status: &DeltaStatus,
) -> Result<(&'static str, chrono::DateTime<chrono::Utc>), String> {
    let kind = match status {
        DeltaStatus::Pending { .. } => "pending",
        DeltaStatus::Candidate { .. } => "candidate",
        DeltaStatus::Canonical { .. } => "canonical",
        DeltaStatus::Retained { .. } => "retained",
        DeltaStatus::Discarded { .. } => "discarded",
    };
    let raw = status.timestamp();
    if raw.is_empty() {
        return Err(format!(
            "DeltaStatus::{kind} missing timestamp; refusing to write indexed status_timestamp"
        ));
    }
    let timestamp = chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| format!("DeltaStatus::{kind} timestamp '{raw}' is not RFC-3339: {e}"))?;
    Ok((kind, timestamp))
}

/// SQL predicate matching the JSONB reason of a client-abandoned
/// discard (issue #319). Paired with `status_kind = 'discarded'` at
/// every call site; kept as one fragment so the reason string cannot
/// drift between the recoverable reads, the submit-time supersede, and
/// the promotion gate.
///
/// Index note: the per-tick recoverable scans stay off the heap-scan
/// path via `idx_deltas_status_kind_status_timestamp` (`status_kind,
/// status_timestamp DESC, account_id, nonce`) — the retained arm is a
/// leading-column equality, and the abandoned arm is an equality plus
/// `status_timestamp >= cutoff` range on the same index (a BitmapOr of
/// the two), so this JSONB extraction only ever filters the handful of
/// recently-discarded rows those ranges return, never the canonical
/// history that dominates the table.
fn client_abandoned_reason() -> diesel::expression::SqlLiteral<diesel::sql_types::Bool> {
    diesel::dsl::sql::<diesel::sql_types::Bool>("status->>'reason' = 'client_abandoned'")
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
        let metadata = row
            .metadata
            .and_then(crate::delta_summary::metadata_from_value);
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
            metadata,
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
            metadata: None,
        }
    }
}

fn proposal_row_to_record(row: ProposalRow) -> ProposalRecord {
    ProposalRecord {
        account_id: row.account_id.clone(),
        commitment: row.commitment.clone(),
        proposal: row.into(),
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = storage_encryption_marker)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct MarkerRow {
    id: bool,
    scheme_version: i16,
    init_kid: String,
}

#[async_trait]
impl MarkerStore for PostgresService {
    async fn read_encryption_marker(&self) -> Result<Option<EncryptionMarker>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;
        let row: Option<MarkerRow> = storage_encryption_marker::table
            .select(MarkerRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| format!("Failed to read encryption marker: {e}"))?;
        row.map(|row| {
            let scheme_version = u8::try_from(row.scheme_version).map_err(|_| {
                format!(
                    "storage encryption marker scheme version {} is out of range",
                    row.scheme_version
                )
            })?;
            Ok(EncryptionMarker {
                scheme_version,
                init_kid: row.init_kid,
            })
        })
        .transpose()
    }

    async fn write_encryption_marker(&self, marker: &EncryptionMarker) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;
        let row = MarkerRow {
            id: true,
            scheme_version: marker.scheme_version as i16,
            init_kid: marker.init_kid.clone(),
        };
        diesel::insert_into(storage_encryption_marker::table)
            .values(&row)
            .on_conflict(storage_encryption_marker::id)
            .do_update()
            .set((
                storage_encryption_marker::scheme_version.eq(row.scheme_version),
                storage_encryption_marker::init_kid.eq(&row.init_kid),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to write encryption marker: {e}"))?;
        Ok(())
    }

    async fn has_payload_records(&self) -> Result<bool, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;
        diesel::select(
            diesel::dsl::exists(states::table.select(states::account_id))
                .or(diesel::dsl::exists(deltas::table.select(deltas::id)))
                .or(diesel::dsl::exists(
                    delta_proposals::table.select(delta_proposals::commitment),
                )),
        )
        .get_result(&mut conn)
        .await
        .map_err(|e| format!("Failed to probe payload records: {e}"))
    }
}

/// A canonicalization write reached the Postgres backend without a lease
/// fence. On this backend every lifecycle write must be fenced — an unfenced
/// write means the processor was wired with a single-process elector against
/// shared storage, which the builder is supposed to reject. Fail closed.
fn unfenced_write_error(operation: &str) -> String {
    format!(
        "Postgres canonicalization writes require a lease fence; \
         refusing unfenced {operation} (wire CoordinationHandles::postgres)"
    )
}

#[derive(diesel::QueryableByName)]
struct CurrentLeaseRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    #[allow(dead_code)]
    held: i32,
}

/// Validate the caller's lease at the canonicalization write boundary.
/// Already-started conditional writes may finish during a leadership transfer.
async fn lease_fence_is_current(
    conn: &mut AsyncPgConnection,
    fence: &LeaseFence,
) -> Result<bool, diesel::result::Error> {
    use diesel::sql_types::{BigInt, Text};
    let row = diesel::sql_query(
        "SELECT 1 AS held FROM worker_leases \
         WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3 \
           AND clock_timestamp() < expires_at",
    )
    .bind::<Text, _>(&fence.lease_name)
    .bind::<Text, _>(&fence.holder_id)
    .bind::<BigInt, _>(fence.fence_token)
    .get_result::<CurrentLeaseRow>(conn)
    .await
    .optional()?;
    Ok(row.is_some())
}

/// Lock the account's metadata row for the duration of the transaction. Every
/// canonicalization write and every candidate submission takes this lock first,
/// serializing them per account so the conditional pending-flag clear can never
/// miss a concurrently committed candidate.
async fn lock_account_metadata(
    conn: &mut AsyncPgConnection,
    account_id: &str,
) -> Result<(), diesel::result::Error> {
    account_metadata::table
        .filter(account_metadata::account_id.eq(account_id))
        .select(account_metadata::account_id)
        .for_update()
        .first::<String>(conn)
        .await
        .map(|_| ())
}

/// Clear `has_pending_candidate` when no candidate rows remain, inside the
/// caller's transaction (transactional twin of
/// `PostgresMetadataStore::clear_pending_candidate_if_none`).
async fn clear_pending_flag_if_none(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), diesel::result::Error> {
    diesel::update(account_metadata::table)
        .filter(account_metadata::account_id.eq(account_id))
        .filter(account_metadata::has_pending_candidate.eq(true))
        .filter(diesel::dsl::not(diesel::dsl::exists(
            deltas::table
                .filter(deltas::account_id.eq(account_id))
                .filter(deltas::status_kind.eq("candidate")),
        )))
        .set((
            account_metadata::has_pending_candidate.eq(false),
            account_metadata::updated_at.eq(updated_at),
        ))
        .execute(conn)
        .await
        .map(|_| ())
}

#[async_trait]
impl StorageBackend for PostgresService {
    fn kind(&self) -> StorageType {
        StorageType::Postgres
    }

    fn pool_status(&self) -> Option<crate::storage::PoolStatus> {
        let status = self.pool.status();
        Some(crate::storage::PoolStatus {
            max_connections: status.max_size as u64,
            connections: status.size as u64,
            available: status.available as u64,
            pending_acquires: status.waiting as u64,
        })
    }

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
        let (status_kind, status_timestamp) = derive_status_columns(&delta.status)?;
        let metadata_json = delta
            .metadata
            .as_ref()
            .map(crate::delta_summary::metadata_to_value);

        let new_delta = NewDelta {
            account_id: &delta.account_id,
            nonce: delta.nonce as i64,
            prev_commitment: &delta.prev_commitment,
            new_commitment: delta.new_commitment.as_deref(),
            delta_payload: &delta.delta_payload,
            ack_sig: Some(delta.ack_sig.as_str()),
            status: status_json.clone(),
            status_kind,
            status_timestamp,
            metadata: metadata_json.as_ref(),
        };

        use diesel::dsl::sql;
        use diesel::sql_types::{Jsonb, Nullable};

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
                deltas::status_kind.eq(status_kind),
                deltas::status_timestamp.eq(status_timestamp),
                deltas::metadata.eq(sql::<Nullable<Jsonb>>(
                    "COALESCE(EXCLUDED.metadata, deltas.metadata)",
                )),
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

    async fn pull_states_batch(
        &self,
        account_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, StateObject>, String> {
        if account_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let owned: Vec<String> = account_ids.iter().map(|s| (*s).to_string()).collect();
        let rows: Vec<StateRow> = states::table
            .filter(states::account_id.eq_any(&owned))
            .select(StateRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to batch-pull states: {e}"))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let state: StateObject = r.into();
                (state.account_id.clone(), state)
            })
            .collect())
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

    async fn pull_candidate_deltas(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<DeltaRow> = deltas::table
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::status_kind.eq("candidate"))
            .order(deltas::nonce.asc())
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull candidate deltas: {e}"))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn pull_recent_candidate_deltas(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        cursor: Option<&crate::storage::RecentCandidateCursor>,
        limit: u32,
    ) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let mut query = deltas::table
            .filter(deltas::status_kind.eq("candidate"))
            .filter(deltas::status_timestamp.gt(since))
            .into_boxed();

        if let Some(cursor) = cursor {
            let last_nonce = i64::try_from(cursor.last_nonce)
                .map_err(|_| "Recent candidate cursor nonce exceeds i64".to_string())?;
            let last_status_timestamp = postgres_timestamp_precision(cursor.last_status_timestamp)?;
            query = query.filter(
                deltas::status_timestamp
                    .gt(last_status_timestamp)
                    .or(deltas::status_timestamp
                        .eq(last_status_timestamp)
                        .and(deltas::account_id.gt(cursor.last_account_id.clone())))
                    .or(deltas::status_timestamp
                        .eq(last_status_timestamp)
                        .and(deltas::account_id.eq(cursor.last_account_id.clone()))
                        .and(deltas::nonce.gt(last_nonce))),
            );
        }

        let rows: Vec<DeltaRow> = query
            .order((
                deltas::status_timestamp.asc(),
                deltas::account_id.asc(),
                deltas::nonce.asc(),
            ))
            .limit(i64::from(limit))
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull recent candidate deltas: {e}"))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn pull_recoverable_deltas(
        &self,
        account_id: &str,
        abandoned_since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<DeltaRow> = deltas::table
            .filter(deltas::account_id.eq(account_id))
            .filter(
                deltas::status_kind.eq("retained").or(deltas::status_kind
                    .eq("discarded")
                    .and(client_abandoned_reason())
                    .and(deltas::status_timestamp.ge(abandoned_since))),
            )
            .order(deltas::nonce.asc())
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to pull recoverable deltas: {e}"))?;

        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn list_accounts_with_recoverable_deltas(
        &self,
        abandoned_since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        deltas::table
            .filter(
                deltas::status_kind.eq("retained").or(deltas::status_kind
                    .eq("discarded")
                    .and(client_abandoned_reason())
                    .and(deltas::status_timestamp.ge(abandoned_since))),
            )
            .select(deltas::account_id)
            .distinct()
            .load::<String>(&mut conn)
            .await
            .map_err(|e| format!("Failed to list accounts with recoverable deltas: {e}"))
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
        let (status_kind, status_timestamp) = derive_status_columns(&proposal.status)?;

        let new_proposal = NewProposal {
            account_id: &proposal.account_id,
            commitment,
            nonce: proposal.nonce as i64,
            prev_commitment: &proposal.prev_commitment,
            new_commitment: proposal.new_commitment.as_deref(),
            delta_payload: &proposal.delta_payload,
            ack_sig: Some(proposal.ack_sig.as_str()),
            status: status_json,
            status_kind,
            status_timestamp,
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

    async fn pull_all_delta_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String> {
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

        Ok(rows.into_iter().map(proposal_row_to_record).collect())
    }

    async fn pull_pending_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String> {
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

        Ok(rows.into_iter().map(proposal_row_to_record).collect())
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
        let (status_kind, status_timestamp) = derive_status_columns(&proposal.status)?;

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
                delta_proposals::status_kind.eq(status_kind),
                delta_proposals::status_timestamp.eq(status_timestamp),
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

    async fn request_candidate_abandon(
        &self,
        account_id: &str,
        nonce: u64,
        now: &str,
    ) -> Result<AbandonIntent, String> {
        use diesel::OptionalExtension;

        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let account_id = account_id.to_string();
        let now = now.to_string();

        // Row-locked read + conditional in-place JSONB update: the intent
        // annotation touches only `abandon_requested_at`, so worker-owned
        // counters in the same status blob are never overwritten, and the
        // `status_kind` filter guarantees a concurrently promoted or
        // discarded delta is left untouched.
        conn.transaction::<AbandonIntent, diesel::result::Error, _>(|conn| {
            async move {
                let existing: Option<Option<String>> = deltas::table
                    .filter(deltas::account_id.eq(&account_id))
                    .filter(deltas::nonce.eq(nonce as i64))
                    .filter(deltas::status_kind.eq("candidate"))
                    .select(diesel::dsl::sql::<
                        diesel::sql_types::Nullable<diesel::sql_types::Text>,
                    >("status->>'abandon_requested_at'"))
                    .for_update()
                    .first(conn)
                    .await
                    .optional()?;

                match existing {
                    None => Ok(AbandonIntent::NotCandidate),
                    Some(Some(requested_at)) => {
                        Ok(AbandonIntent::AlreadyRequested { requested_at })
                    }
                    Some(None) => {
                        diesel::update(deltas::table)
                            .filter(deltas::account_id.eq(&account_id))
                            .filter(deltas::nonce.eq(nonce as i64))
                            .filter(deltas::status_kind.eq("candidate"))
                            .set(
                                deltas::status.eq(diesel::dsl::sql::<diesel::sql_types::Jsonb>(
                                    "jsonb_set(status, '{abandon_requested_at}', to_jsonb(",
                                )
                                .bind::<diesel::sql_types::Text, _>(now)
                                .sql("::text))")),
                            )
                            .execute(conn)
                            .await?;
                        Ok(AbandonIntent::Recorded)
                    }
                }
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to record abandon request: {e}"))
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
        let (status_kind, status_timestamp) = derive_status_columns(&status)?;

        diesel::update(deltas::table)
            .filter(deltas::account_id.eq(account_id))
            .filter(deltas::nonce.eq(nonce as i64))
            .set((
                deltas::status.eq(&status_json),
                deltas::status_kind.eq(status_kind),
                deltas::status_timestamp.eq(status_timestamp),
            ))
            .execute(&mut conn)
            .await
            .map_err(|e| format!("Failed to update delta status: {e}"))?;

        Ok(())
    }

    async fn submit_candidate(
        &self,
        _metadata: &dyn MetadataStore,
        delta: &DeltaObject,
        now: &str,
    ) -> Result<CandidateSubmission, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let status_json = serde_json::to_value(&delta.status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;
        let (status_kind, status_timestamp) = derive_status_columns(&delta.status)?;
        let metadata_json = delta
            .metadata
            .as_ref()
            .map(crate::delta_summary::metadata_to_value);
        let updated_at: chrono::DateTime<chrono::Utc> = now
            .parse()
            .map_err(|e| format!("Failed to parse timestamp: {e}"))?;
        let delta = delta.clone();

        conn.transaction::<CandidateSubmission, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &delta.account_id).await?;

                let current_commitment = states::table
                    .filter(states::account_id.eq(&delta.account_id))
                    .select(states::commitment)
                    .first::<String>(conn)
                    .await?;
                if current_commitment != delta.prev_commitment {
                    return Ok(CandidateSubmission::CommitmentMismatch {
                        expected: current_commitment,
                    });
                }

                // Race-proof twin of the service-layer admission gate:
                // two submissions that both passed the pre-commit scan
                // serialize on the account lock, and the loser sees the
                // winner's candidate here.
                let pending: bool = diesel::select(diesel::dsl::exists(
                    deltas::table
                        .filter(deltas::account_id.eq(&delta.account_id))
                        .filter(deltas::status_kind.eq("candidate")),
                ))
                .get_result(conn)
                .await?;
                if pending {
                    return Ok(CandidateSubmission::Conflict);
                }

                // A retained row (issue #345) or client-abandoned
                // discard (issue #319) at this nonce is a best-effort
                // recovery/history artifact, never settled canonical
                // history: the client re-supplying its intent for the
                // slot supersedes it, so the reconcile pass can never
                // resurrect a base out from under this new candidate —
                // and the resubmission the abandon endpoint exists to
                // enable is not refused at the nonce's unique
                // constraint.
                let superseded = diesel::delete(deltas::table)
                    .filter(deltas::account_id.eq(&delta.account_id))
                    .filter(deltas::nonce.eq(delta.nonce as i64))
                    .filter(
                        deltas::status_kind.eq("retained").or(deltas::status_kind
                            .eq("discarded")
                            .and(client_abandoned_reason())),
                    )
                    .execute(conn)
                    .await?;
                if superseded > 0 {
                    tracing::info!(
                        event = "reconcile_superseded",
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        "Recoverable row superseded by a new candidate at its nonce"
                    );
                }

                // DO NOTHING (not upsert): a row already at this nonce is
                // settled history and must never be overwritten by a
                // delayed submission.
                let inserted = diesel::insert_into(deltas::table)
                    .values(&NewDelta {
                        account_id: &delta.account_id,
                        nonce: delta.nonce as i64,
                        prev_commitment: &delta.prev_commitment,
                        new_commitment: delta.new_commitment.as_deref(),
                        delta_payload: &delta.delta_payload,
                        ack_sig: Some(delta.ack_sig.as_str()),
                        status: status_json.clone(),
                        status_kind,
                        status_timestamp,
                        metadata: metadata_json.as_ref(),
                    })
                    .on_conflict((deltas::account_id, deltas::nonce))
                    .do_nothing()
                    .execute(conn)
                    .await?;
                if inserted == 0 {
                    return Ok(CandidateSubmission::Conflict);
                }

                diesel::update(account_metadata::table)
                    .filter(account_metadata::account_id.eq(&delta.account_id))
                    .set((
                        account_metadata::has_pending_candidate.eq(true),
                        account_metadata::updated_at.eq(updated_at),
                    ))
                    .execute(conn)
                    .await?;

                Ok(CandidateSubmission::Submitted)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to submit candidate: {e}"))
    }

    async fn promote_candidate(
        &self,
        _metadata: &dyn MetadataStore,
        promotion: CandidatePromotion,
    ) -> Result<PromoteWrite, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let CandidatePromotion {
            state,
            delta,
            new_auth,
            now,
            fence,
            source,
        } = promotion;

        let state_updated_at: chrono::DateTime<chrono::Utc> = state
            .updated_at
            .parse()
            .map_err(|e| format!("Failed to parse updated_at: {e}"))?;
        let metadata_updated_at: chrono::DateTime<chrono::Utc> = now
            .parse()
            .map_err(|e| format!("Failed to parse timestamp: {e}"))?;
        let status_json = serde_json::to_value(&delta.status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;
        let (status_kind, status_timestamp) = derive_status_columns(&delta.status)?;
        let auth_json = new_auth
            .map(|auth| serde_json::to_value(&auth))
            .transpose()
            .map_err(|e| format!("Failed to serialize auth: {e}"))?;
        let fence = fence.ok_or_else(|| unfenced_write_error("promote_candidate"))?;

        let result = conn
            .transaction::<PromoteWrite, diesel::result::Error, _>(|conn| {
                async move {
                    lock_account_metadata(conn, &state.account_id).await?;
                    if !lease_fence_is_current(conn, &fence).await? {
                        return Ok(PromoteWrite::StaleLease);
                    }

                    // Retained rows (issue #345) and client-abandoned
                    // discards (issue #319, the late-landing recovery
                    // net) are promotable too — but the gate is the
                    // EXACT kind the pass verified, never the union: a
                    // client submission can supersede a retained or
                    // abandoned row at this nonce between the reconcile
                    // read and this write, and a union gate would stamp
                    // the client's fresh candidate canonical with the
                    // old delta's commitment. The exact gate turns a
                    // superseded promotion into a no-op instead.
                    let source_gate: Box<
                        dyn diesel::BoxableExpression<
                                deltas::table,
                                diesel::pg::Pg,
                                SqlType = diesel::sql_types::Bool,
                            >,
                    > = match source {
                        PromotableKind::Candidate => Box::new(deltas::status_kind.eq("candidate")),
                        PromotableKind::Retained => Box::new(deltas::status_kind.eq("retained")),
                        PromotableKind::ClientAbandoned => Box::new(
                            deltas::status_kind
                                .eq("discarded")
                                .and(client_abandoned_reason()),
                        ),
                    };
                    let flipped = diesel::update(deltas::table)
                        .filter(deltas::account_id.eq(&delta.account_id))
                        .filter(deltas::nonce.eq(delta.nonce as i64))
                        .filter(source_gate)
                        .set((
                            deltas::status.eq(&status_json),
                            deltas::status_kind.eq(status_kind),
                            deltas::status_timestamp.eq(status_timestamp),
                            deltas::new_commitment.eq(&delta.new_commitment),
                        ))
                        .execute(conn)
                        .await?;
                    if flipped == 0 {
                        return Ok(PromoteWrite::NotCandidate);
                    }

                    // The base gate lives in the UPDATE predicate itself, not
                    // a prior read: `submit_state` writers take no account
                    // lock, so only the row-level write lock makes the
                    // comparison race-proof. Zero rows means the state moved
                    // (or vanished) since this pass read it — the delta flip
                    // above must not survive that, hence the explicit
                    // rollback mapped to `StaleBase` below.
                    let advanced = diesel::update(states::table)
                        .filter(states::account_id.eq(&state.account_id))
                        .filter(states::commitment.eq(&delta.prev_commitment))
                        .set((
                            states::state_json.eq(&state.state_json),
                            states::commitment.eq(&state.commitment),
                            states::updated_at.eq(state_updated_at),
                        ))
                        .execute(conn)
                        .await?;
                    if advanced == 0 {
                        return Err(diesel::result::Error::RollbackTransaction);
                    }

                    if let Some(auth_json) = &auth_json {
                        diesel::update(account_metadata::table)
                            .filter(account_metadata::account_id.eq(&state.account_id))
                            .set((
                                account_metadata::auth.eq(auth_json),
                                account_metadata::updated_at.eq(metadata_updated_at),
                            ))
                            .execute(conn)
                            .await?;
                    }

                    clear_pending_flag_if_none(conn, &state.account_id, metadata_updated_at)
                        .await?;
                    Ok(PromoteWrite::Applied)
                }
                .scope_boxed()
            })
            .await;

        match result {
            Ok(outcome) => Ok(outcome),
            Err(diesel::result::Error::RollbackTransaction) => Ok(PromoteWrite::StaleBase),
            Err(e) => Err(format!("Failed to promote candidate: {e}")),
        }
    }

    async fn discard_candidate(
        &self,
        _metadata: &dyn MetadataStore,
        account_id: &str,
        nonce: u64,
        kind: DeltaStatusKind,
        now: &str,
        fence: Option<&LeaseFence>,
    ) -> Result<CanonicalWrite, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let updated_at: chrono::DateTime<chrono::Utc> = now
            .parse()
            .map_err(|e| format!("Failed to parse timestamp: {e}"))?;
        let account_id = account_id.to_string();
        let fence = fence
            .cloned()
            .ok_or_else(|| unfenced_write_error("discard_candidate"))?;

        conn.transaction::<CanonicalWrite, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &account_id).await?;
                if !lease_fence_is_current(conn, &fence).await? {
                    return Ok(CanonicalWrite::StaleLease);
                }

                let deleted = diesel::delete(deltas::table)
                    .filter(deltas::account_id.eq(&account_id))
                    .filter(deltas::nonce.eq(nonce as i64))
                    .filter(deltas::status_kind.eq(kind.as_str()))
                    .execute(conn)
                    .await?;
                if deleted == 0 {
                    return Ok(CanonicalWrite::NotCandidate);
                }

                clear_pending_flag_if_none(conn, &account_id, updated_at).await?;
                Ok(CanonicalWrite::Applied)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to discard candidate: {e}"))
    }

    async fn update_candidate_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
        fence: Option<&LeaseFence>,
    ) -> Result<CanonicalWrite, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let status_json = serde_json::to_value(&status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;
        let (status_kind, status_timestamp) = derive_status_columns(&status)?;
        let account_id = account_id.to_string();
        let fence = fence
            .cloned()
            .ok_or_else(|| unfenced_write_error("update_candidate_status"))?;

        conn.transaction::<CanonicalWrite, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &account_id).await?;
                if !lease_fence_is_current(conn, &fence).await? {
                    return Ok(CanonicalWrite::StaleLease);
                }

                // Row-locked read of the stored abandon request: the new
                // status is computed from the worker's tick-start snapshot,
                // so an intent recorded concurrently by
                // `request_candidate_abandon` (which takes the same row
                // lock) must be carried into the overwrite or it would be
                // silently wiped.
                use diesel::OptionalExtension;
                let stored_requested_at: Option<Option<String>> = deltas::table
                    .filter(deltas::account_id.eq(&account_id))
                    .filter(deltas::nonce.eq(nonce as i64))
                    .filter(deltas::status_kind.eq("candidate"))
                    .select(diesel::dsl::sql::<
                        diesel::sql_types::Nullable<diesel::sql_types::Text>,
                    >("status->>'abandon_requested_at'"))
                    .for_update()
                    .first(conn)
                    .await
                    .optional()?;
                let Some(stored_requested_at) = stored_requested_at else {
                    return Ok(CanonicalWrite::NotCandidate);
                };

                // A concurrently recorded abandon intent must not be
                // wiped into a retained status, which has no field to
                // carry it: refuse the flip — the next worker tick sees
                // the intent in its snapshot and resolves the abandon
                // instead of retaining.
                if status_kind == "retained" && stored_requested_at.is_some() {
                    return Ok(CanonicalWrite::NotCandidate);
                }

                let mut status_json = status_json;
                if status_kind == "candidate"
                    && status_json
                        .get("abandon_requested_at")
                        .is_none_or(serde_json::Value::is_null)
                    && let Some(stored) = stored_requested_at
                {
                    status_json["abandon_requested_at"] = serde_json::Value::String(stored);
                }

                let updated = diesel::update(deltas::table)
                    .filter(deltas::account_id.eq(&account_id))
                    .filter(deltas::nonce.eq(nonce as i64))
                    .filter(deltas::status_kind.eq("candidate"))
                    .set((
                        deltas::status.eq(&status_json),
                        deltas::status_kind.eq(status_kind),
                        deltas::status_timestamp.eq(status_timestamp),
                    ))
                    .execute(conn)
                    .await?;
                if updated == 0 {
                    return Ok(CanonicalWrite::NotCandidate);
                }
                Ok(CanonicalWrite::Applied)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to update candidate status: {e}"))
    }

    // ----------------------------------------------------------------------
    // Dashboard read APIs (feature `005-operator-dashboard-metrics`).
    //
    // SQL pushdown over the typed `status_kind` / `status_timestamp`
    // columns plus the composite indexes from migration
    // 2026-05-10-000001. Single query per request — no fan-out.
    // ----------------------------------------------------------------------

    async fn list_account_deltas_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let mut query = deltas::table
            .filter(deltas::account_id.eq(account_id))
            // pending entries are returned via the proposal queue.
            .filter(deltas::status_kind.ne("pending"))
            .into_boxed();

        if let Some(c) = cursor {
            query = query.filter(deltas::nonce.lt(c.last_nonce));
        }

        let rows: Vec<DeltaRow> = query
            .order(deltas::nonce.desc())
            .limit(limit as i64)
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list account deltas: {e}"))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn list_account_proposals_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let mut query = delta_proposals::table
            .filter(delta_proposals::account_id.eq(account_id))
            .filter(delta_proposals::status_kind.eq("pending"))
            .into_boxed();

        if let Some(c) = cursor {
            // Composite cursor predicate on `(nonce DESC, commitment
            // DESC)`. `(account_id, nonce)` is NOT unique on
            // `delta_proposals` — two operators can submit competing
            // proposals at the same nonce — so the commitment is the
            // deterministic tiebreaker.
            query = query.filter(
                delta_proposals::nonce
                    .lt(c.last_nonce)
                    .or(delta_proposals::nonce
                        .eq(c.last_nonce)
                        .and(delta_proposals::commitment.lt(c.last_commitment.clone()))),
            );
        }

        let rows: Vec<ProposalRow> = query
            .order((
                delta_proposals::nonce.desc(),
                delta_proposals::commitment.desc(),
            ))
            .limit(limit as i64)
            .select(ProposalRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list account proposals: {e}"))?;

        Ok(rows
            .into_iter()
            .map(|row| ProposalRecord {
                account_id: row.account_id.clone(),
                commitment: row.commitment.clone(),
                proposal: row.into(),
            })
            .collect())
    }

    async fn list_global_deltas_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalDeltaCursor>,
        status_filter: Option<Vec<DeltaStatusKind>>,
    ) -> Result<Vec<GlobalDeltaRow>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let mut query = deltas::table
            // Pending entries don't surface on the delta feed even
            // without an explicit filter (they live on the proposal
            // feed).
            .filter(deltas::status_kind.ne("pending"))
            .into_boxed();

        if let Some(kinds) = status_filter {
            // Coerce typed enum to the stable string column values.
            let allowed: Vec<String> = kinds.iter().map(|k| k.as_str().to_string()).collect();
            query = query.filter(deltas::status_kind.eq_any(allowed));
        }

        if let Some(c) = cursor {
            // Cursor predicate over the composite sort key
            // `(status_timestamp DESC, account_id ASC, nonce ASC)`.
            // `(account_id, nonce)` is unique on `deltas`, so this
            // composite tuple is fully deterministic.
            query = query.filter(
                deltas::status_timestamp
                    .lt(c.last_status_timestamp)
                    .or(deltas::status_timestamp
                        .eq(c.last_status_timestamp)
                        .and(deltas::account_id.gt(c.last_account_id.clone())))
                    .or(deltas::status_timestamp
                        .eq(c.last_status_timestamp)
                        .and(deltas::account_id.eq(c.last_account_id))
                        .and(deltas::nonce.gt(c.last_nonce))),
            );
        }

        let rows: Vec<DeltaRow> = query
            .order((
                deltas::status_timestamp.desc(),
                deltas::account_id.asc(),
                deltas::nonce.asc(),
            ))
            .limit(limit as i64)
            .select(DeltaRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list global deltas: {e}"))?;

        Ok(rows
            .into_iter()
            .map(|row| GlobalDeltaRow {
                account_id: row.account_id.clone(),
                delta: row.into(),
            })
            .collect())
    }

    async fn list_global_proposals_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let mut query = delta_proposals::table
            .filter(delta_proposals::status_kind.eq("pending"))
            .into_boxed();

        if let Some(c) = cursor {
            // Composite cursor on `(status_timestamp DESC, account_id
            // ASC, nonce ASC, commitment ASC)`. The four-tuple is
            // unique because `(account_id, commitment)` is the
            // delta_proposals UNIQUE constraint.
            query = query.filter(
                delta_proposals::status_timestamp
                    .lt(c.last_originating_timestamp)
                    .or(delta_proposals::status_timestamp
                        .eq(c.last_originating_timestamp)
                        .and(delta_proposals::account_id.gt(c.last_account_id.clone())))
                    .or(delta_proposals::status_timestamp
                        .eq(c.last_originating_timestamp)
                        .and(delta_proposals::account_id.eq(c.last_account_id.clone()))
                        .and(delta_proposals::nonce.gt(c.last_nonce)))
                    .or(delta_proposals::status_timestamp
                        .eq(c.last_originating_timestamp)
                        .and(delta_proposals::account_id.eq(c.last_account_id))
                        .and(delta_proposals::nonce.eq(c.last_nonce))
                        .and(delta_proposals::commitment.gt(c.last_commitment))),
            );
        }

        let rows: Vec<ProposalRow> = query
            .order((
                delta_proposals::status_timestamp.desc(),
                delta_proposals::account_id.asc(),
                delta_proposals::nonce.asc(),
                delta_proposals::commitment.asc(),
            ))
            .limit(limit as i64)
            .select(ProposalRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list global proposals: {e}"))?;

        Ok(rows
            .into_iter()
            .map(|row| ProposalRecord {
                account_id: row.account_id.clone(),
                commitment: row.commitment.clone(),
                proposal: row.into(),
            })
            .collect())
    }

    async fn count_deltas_by_status(&self) -> Result<DeltaStatusCounts, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let rows: Vec<(String, i64)> = deltas::table
            .group_by(deltas::status_kind)
            .select((deltas::status_kind, diesel::dsl::count_star()))
            .load::<(String, i64)>(&mut conn)
            .await
            .map_err(|e| format!("Failed to count deltas by status: {e}"))?;

        let mut counts = DeltaStatusCounts::default();
        for (kind, n) in rows {
            let n = n.max(0) as u64;
            match kind.as_str() {
                "candidate" => counts.candidate = n,
                "canonical" => counts.canonical = n,
                "retained" => counts.retained = n,
                "discarded" => counts.discarded = n,
                // `pending` is exposed via count_in_flight_proposals,
                // not the delta status counts.
                "pending" => {}
                other => {
                    // The migration's CHECK constraint should make this
                    // unreachable. Log so a future lifecycle status
                    // addition shows up in tests/ops instead of
                    // silently zeroing the counter.
                    tracing::warn!(
                        unexpected_status_kind = other,
                        count = n,
                        "count_deltas_by_status: unknown status_kind in deltas table"
                    );
                }
            }
        }
        Ok(counts)
    }

    async fn count_in_flight_proposals(&self) -> Result<u64, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let n: i64 = delta_proposals::table
            .filter(delta_proposals::status_kind.eq("pending"))
            .count()
            .get_result(&mut conn)
            .await
            .map_err(|e| format!("Failed to count in-flight proposals: {e}"))?;

        Ok(n.max(0) as u64)
    }

    async fn latest_activity_timestamp(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))?;

        let max_delta: Option<chrono::DateTime<chrono::Utc>> = deltas::table
            .select(diesel::dsl::max(deltas::status_timestamp))
            .first(&mut conn)
            .await
            .map_err(|e| format!("Failed to read max delta status_timestamp: {e}"))?;

        let max_proposal: Option<chrono::DateTime<chrono::Utc>> = delta_proposals::table
            .select(diesel::dsl::max(delta_proposals::status_timestamp))
            .first(&mut conn)
            .await
            .map_err(|e| format!("Failed to read max proposal status_timestamp: {e}"))?;

        Ok(match (max_delta, max_proposal) {
            (None, None) => None,
            (Some(a), None) | (None, Some(a)) => Some(a),
            (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_timestamp_precision_truncates_sub_microseconds() {
        let timestamp = DateTime::parse_from_rfc3339("2026-07-22T12:34:56.123456789Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        assert_eq!(
            postgres_timestamp_precision(timestamp).expect("normalization succeeds"),
            DateTime::parse_from_rfc3339("2026-07-22T12:34:56.123456Z")
                .expect("valid timestamp")
                .with_timezone(&Utc)
        );
    }

    fn url_with_mode(query: &str) -> String {
        if query.is_empty() {
            "postgres://guardian:pw@db.example.com:5432/guardian".to_string()
        } else {
            format!("postgres://guardian:pw@db.example.com:5432/guardian?{query}")
        }
    }

    #[test]
    fn absent_sslmode_is_disable() {
        assert_eq!(
            parse_tls_plan(&url_with_mode("")).unwrap(),
            TlsPlan::Disable
        );
    }

    #[test]
    fn explicit_disable_is_disable() {
        assert_eq!(
            parse_tls_plan(&url_with_mode("sslmode=disable")).unwrap(),
            TlsPlan::Disable
        );
    }

    #[test]
    fn require_without_rootcert_is_encrypt_only() {
        assert_eq!(
            parse_tls_plan(&url_with_mode("sslmode=require")).unwrap(),
            TlsPlan::EncryptOnly
        );
    }

    #[test]
    fn require_with_rootcert_promotes_to_verify_ca() {
        assert_eq!(
            parse_tls_plan(&url_with_mode("sslmode=require&sslrootcert=/etc/ca.pem")).unwrap(),
            TlsPlan::Verify {
                level: VerifyLevel::Ca,
                ca_path: "/etc/ca.pem".to_string(),
            }
        );
    }

    #[test]
    fn verify_ca_with_rootcert() {
        assert_eq!(
            parse_tls_plan(&url_with_mode("sslmode=verify-ca&sslrootcert=/etc/ca.pem")).unwrap(),
            TlsPlan::Verify {
                level: VerifyLevel::Ca,
                ca_path: "/etc/ca.pem".to_string(),
            }
        );
    }

    #[test]
    fn verify_full_with_rootcert() {
        assert_eq!(
            parse_tls_plan(&url_with_mode(
                "sslmode=verify-full&sslrootcert=/etc/ca.pem"
            ))
            .unwrap(),
            TlsPlan::Verify {
                level: VerifyLevel::Full,
                ca_path: "/etc/ca.pem".to_string(),
            }
        );
    }

    #[test]
    fn verify_modes_require_rootcert() {
        assert!(parse_tls_plan(&url_with_mode("sslmode=verify-ca")).is_err());
        assert!(parse_tls_plan(&url_with_mode("sslmode=verify-full")).is_err());
    }

    #[test]
    fn allow_and_prefer_are_rejected() {
        assert!(parse_tls_plan(&url_with_mode("sslmode=allow")).is_err());
        assert!(parse_tls_plan(&url_with_mode("sslmode=prefer")).is_err());
    }

    #[test]
    fn unknown_sslmode_is_rejected() {
        assert!(parse_tls_plan(&url_with_mode("sslmode=banana")).is_err());
    }

    #[test]
    fn sslrootcert_system_is_rejected() {
        assert!(parse_tls_plan(&url_with_mode("sslmode=verify-full&sslrootcert=system")).is_err());
    }

    #[test]
    fn empty_sslrootcert_is_rejected() {
        assert!(parse_tls_plan(&url_with_mode("sslmode=verify-full&sslrootcert=")).is_err());
    }

    #[test]
    fn duplicate_params_are_rejected() {
        assert!(parse_tls_plan(&url_with_mode("sslmode=require&sslmode=disable")).is_err());
        assert!(
            parse_tls_plan(&url_with_mode(
                "sslmode=verify-ca&sslrootcert=/a&sslrootcert=/b"
            ))
            .is_err()
        );
    }

    #[test]
    fn non_url_dsn_is_rejected() {
        assert!(parse_tls_plan("host=db.example.com sslmode=require dbname=guardian").is_err());
    }

    #[test]
    fn unsupported_scheme_is_rejected() {
        assert!(parse_tls_plan("mysql://guardian:pw@db.example.com/guardian").is_err());
    }

    #[test]
    fn multi_host_is_rejected() {
        assert!(
            parse_tls_plan("postgres://guardian:pw@a.example.com,b.example.com/guardian").is_err()
        );
    }

    #[test]
    fn sync_url_normalizes_absent_to_disable() {
        let plan = parse_tls_plan(&url_with_mode("")).unwrap();
        let sync = normalized_sync_url(&url_with_mode(""), &plan).unwrap();
        assert!(sync.contains("sslmode=disable"));
        assert!(!sync.contains("sslrootcert"));
    }

    #[test]
    fn sync_url_keeps_verify_full_and_rootcert() {
        let raw = url_with_mode("sslmode=verify-full&sslrootcert=/etc/ca.pem");
        let plan = parse_tls_plan(&raw).unwrap();
        let sync = normalized_sync_url(&raw, &plan).unwrap();
        assert!(sync.contains("sslmode=verify-full"));
        assert!(
            sync.contains("sslrootcert=%2Fetc%2Fca.pem")
                || sync.contains("sslrootcert=/etc/ca.pem")
        );
    }

    #[test]
    fn async_url_forces_require_and_drops_rootcert() {
        let raw = url_with_mode("sslmode=verify-full&sslrootcert=/etc/ca.pem");
        let plan = parse_tls_plan(&raw).unwrap();
        let async_url = sanitized_async_url(&raw, &plan).unwrap();
        assert!(async_url.contains("sslmode=require"));
        assert!(!async_url.contains("sslrootcert"));
        assert!(!async_url.contains("verify-full"));
        assert!(async_url.contains("db.example.com"));
    }

    #[test]
    fn async_url_disable_stays_disable() {
        let raw = url_with_mode("sslmode=disable");
        let plan = parse_tls_plan(&raw).unwrap();
        let async_url = sanitized_async_url(&raw, &plan).unwrap();
        assert!(async_url.contains("sslmode=disable"));
    }

    #[test]
    fn both_stacks_agree_for_every_supported_mode() {
        let cases = [
            ("", false),
            ("sslmode=disable", false),
            ("sslmode=require", true),
            ("sslmode=require&sslrootcert=/etc/ca.pem", true),
            ("sslmode=verify-ca&sslrootcert=/etc/ca.pem", true),
            ("sslmode=verify-full&sslrootcert=/etc/ca.pem", true),
        ];
        for (query, tls_expected) in cases {
            let raw = url_with_mode(query);
            let plan = parse_tls_plan(&raw).unwrap();
            let sync = normalized_sync_url(&raw, &plan).unwrap();
            let async_url = sanitized_async_url(&raw, &plan).unwrap();

            let sync_tls = !sync.contains("sslmode=disable");
            let async_tls = !async_url.contains("sslmode=disable");
            assert_eq!(sync_tls, tls_expected, "sync TLS for {query:?}");
            assert_eq!(async_tls, tls_expected, "async TLS for {query:?}");

            let verifying = matches!(plan, TlsPlan::Verify { .. });
            assert_eq!(
                sync.contains("sslrootcert"),
                verifying,
                "sync trust anchor for {query:?}"
            );
            assert!(
                !async_url.contains("sslrootcert"),
                "async strips sslrootcert for {query:?}"
            );
            assert!(
                !async_url.contains("verify-"),
                "async forces require for {query:?}"
            );

            if !verifying {
                assert_eq!(
                    build_tls_client_config(&plan).unwrap().is_some(),
                    tls_expected,
                    "async verifier presence for {query:?}"
                );
            }
        }
    }

    #[test]
    fn preflight_error_does_not_leak_password() {
        let raw = "postgres://guardian:SUPERSECRET@db.example.com/guardian?sslmode=verify-full&sslrootcert=/nonexistent/ca.pem";
        let error = preflight_tls(raw).expect_err("missing CA bundle must fail");
        assert!(
            !error.contains("SUPERSECRET"),
            "error leaked password: {error}"
        );
    }

    #[tokio::test]
    async fn pool_connect_failure_error_is_password_free() {
        let raw = "postgres://guardian:SUPERSECRET@127.0.0.1:1/guardian?sslmode=require";
        let error = build_postgres_pool(raw, 1)
            .await
            .err()
            .expect("connection to a closed port must fail");
        assert!(
            !error.contains("SUPERSECRET"),
            "pool error leaked password: {error}"
        );
    }

    #[tokio::test]
    async fn migration_connect_failure_error_is_password_free() {
        let raw = "postgres://guardian:SUPERSECRET@127.0.0.1:1/guardian?sslmode=require";
        let sync_url = preflight_tls(raw).unwrap();
        let error = run_migrations(&sync_url)
            .await
            .expect_err("migration connection to a closed port must fail");
        assert!(
            !error.contains("SUPERSECRET"),
            "migration error leaked password: {error}"
        );
    }

    #[test]
    fn load_root_store_rejects_non_certificate_file() {
        let dir = std::env::temp_dir().join(format!("guardian_ca_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-cert.pem");
        std::fs::write(&path, b"this is not a certificate").unwrap();
        let result = load_root_store(path.to_str().unwrap());
        std::fs::remove_dir_all(&dir).ok();
        assert!(result.is_err());
    }

    #[test]
    fn load_root_store_rejects_missing_file() {
        assert!(load_root_store("/nonexistent/path/ca.pem").is_err());
    }

    fn test_now() -> UnixTime {
        UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_900_000_000))
    }

    fn gen_ca() -> (rcgen::Certificate, rcgen::KeyPair) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2050, 1, 1);
        let cert = params.self_signed(&key).unwrap();
        (cert, key)
    }

    fn gen_leaf(
        sans: Vec<rcgen::SanType>,
        common_name: Option<&str>,
        ca: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
    ) -> rcgen::Certificate {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = sans;
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2050, 1, 1);
        if let Some(cn) = common_name {
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, cn);
        }
        params.signed_by(&key, ca, ca_key).unwrap()
    }

    fn dns_san(name: &str) -> rcgen::SanType {
        rcgen::SanType::DnsName(name.try_into().unwrap())
    }

    fn roots_from(cas: &[&rcgen::Certificate]) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        for ca in cas {
            roots.add(ca.der().clone()).unwrap();
        }
        roots
    }

    fn full_verifier(roots: RootCertStore) -> Arc<WebPkiServerVerifier> {
        install_rustls_provider();
        WebPkiServerVerifier::builder(Arc::new(roots))
            .build()
            .unwrap()
    }

    fn server_name(name: &str) -> ServerName<'static> {
        ServerName::try_from(name.to_string()).unwrap()
    }

    #[test]
    fn verify_full_accepts_matching_dns_san() {
        let (ca, ca_key) = gen_ca();
        let leaf = gen_leaf(vec![dns_san("db.example.com")], None, &ca, &ca_key);
        let verifier = full_verifier(roots_from(&[&ca]));
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("db.example.com"),
                    &[],
                    test_now(),
                )
                .is_ok()
        );
    }

    #[test]
    fn verify_full_accepts_matching_ip_san() {
        let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5));
        let (ca, ca_key) = gen_ca();
        let leaf = gen_leaf(vec![rcgen::SanType::IpAddress(ip)], None, &ca, &ca_key);
        let verifier = full_verifier(roots_from(&[&ca]));
        assert!(
            verifier
                .verify_server_cert(leaf.der(), &[], &server_name("10.0.0.5"), &[], test_now())
                .is_ok()
        );
    }

    #[test]
    fn verify_full_rejects_hostname_mismatch() {
        let (ca, ca_key) = gen_ca();
        let leaf = gen_leaf(vec![dns_san("db.example.com")], None, &ca, &ca_key);
        let verifier = full_verifier(roots_from(&[&ca]));
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("evil.example.com"),
                    &[],
                    test_now(),
                )
                .is_err()
        );
    }

    #[test]
    fn verify_full_rejects_cn_only_cert() {
        let (ca, ca_key) = gen_ca();
        let leaf = gen_leaf(vec![], Some("db.example.com"), &ca, &ca_key);
        let verifier = full_verifier(roots_from(&[&ca]));
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("db.example.com"),
                    &[],
                    test_now(),
                )
                .is_err()
        );
    }

    #[test]
    fn verify_full_rejects_expired_certificate() {
        let (ca, ca_key) = gen_ca();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = vec![dns_san("db.example.com")];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2021, 1, 1);
        let leaf = params.signed_by(&key, &ca, &ca_key).unwrap();
        let verifier = full_verifier(roots_from(&[&ca]));
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("db.example.com"),
                    &[],
                    test_now(),
                )
                .is_err()
        );
    }

    #[test]
    fn verify_full_rejects_untrusted_issuer() {
        let (ca, ca_key) = gen_ca();
        let (other_ca, _) = gen_ca();
        let leaf = gen_leaf(vec![dns_san("db.example.com")], None, &ca, &ca_key);
        let verifier = full_verifier(roots_from(&[&other_ca]));
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("db.example.com"),
                    &[],
                    test_now(),
                )
                .is_err()
        );
    }

    #[test]
    fn verify_ca_tolerates_hostname_mismatch() {
        let (ca, ca_key) = gen_ca();
        let leaf = gen_leaf(vec![dns_san("db.example.com")], None, &ca, &ca_key);
        let verifier = ChainOnlyVerifier {
            inner: full_verifier(roots_from(&[&ca])),
        };
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("totally-different.example.com"),
                    &[],
                    test_now(),
                )
                .is_ok()
        );
    }

    #[test]
    fn verify_ca_still_rejects_untrusted_issuer() {
        let (ca, ca_key) = gen_ca();
        let (other_ca, _) = gen_ca();
        let leaf = gen_leaf(vec![dns_san("db.example.com")], None, &ca, &ca_key);
        let verifier = ChainOnlyVerifier {
            inner: full_verifier(roots_from(&[&other_ca])),
        };
        assert!(
            verifier
                .verify_server_cert(
                    leaf.der(),
                    &[],
                    &server_name("db.example.com"),
                    &[],
                    test_now(),
                )
                .is_err()
        );
    }

    #[test]
    fn combined_bundle_validates_certs_from_either_root() {
        let (ca_a, key_a) = gen_ca();
        let (ca_b, key_b) = gen_ca();
        let leaf_a = gen_leaf(vec![dns_san("a.example.com")], None, &ca_a, &key_a);
        let leaf_b = gen_leaf(vec![dns_san("b.example.com")], None, &ca_b, &key_b);
        let verifier = full_verifier(roots_from(&[&ca_a, &ca_b]));
        assert!(
            verifier
                .verify_server_cert(
                    leaf_a.der(),
                    &[],
                    &server_name("a.example.com"),
                    &[],
                    test_now(),
                )
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(
                    leaf_b.der(),
                    &[],
                    &server_name("b.example.com"),
                    &[],
                    test_now(),
                )
                .is_ok()
        );
    }

    #[test]
    fn load_root_store_accepts_multi_root_bundle() {
        let (ca_a, _) = gen_ca();
        let (ca_b, _) = gen_ca();
        let dir = std::env::temp_dir().join(format!("guardian_ca_multi_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("combined.pem");
        std::fs::write(&path, format!("{}{}", ca_a.pem(), ca_b.pem())).unwrap();
        let result = load_root_store(path.to_str().unwrap());
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(result.unwrap().len(), 2);
    }

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
            metadata: None,
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

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn pull_candidate_deltas_filters_in_the_store() {
        use crate::delta_object::DeltaStatus;
        use diesel::sql_types::Text;

        let url = crate::testing::pg::test_database_url().await;
        run_migrations(&url).await.expect("migrations apply");

        let service = PostgresService::new(&url, 4).await.expect("storage");
        let stamp = chrono::Utc::now().timestamp_micros();
        let account_id = format!("0xcand{stamp}");
        let now_at = chrono::Utc::now();
        let now = now_at.to_rfc3339();

        let mut conn = service.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), false)",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata row");
        drop(conn);

        let mut canonical = create_test_delta(&account_id, 1);
        canonical.status = DeltaStatus::canonical(now.clone());
        service.submit_delta(&canonical).await.expect("canonical");
        for nonce in [3u64, 2] {
            let mut candidate = create_test_delta(&account_id, nonce);
            candidate.status = DeltaStatus::candidate(now.clone());
            service.submit_delta(&candidate).await.expect("candidate");
        }
        let mut old_candidate = create_test_delta(&account_id, 4);
        old_candidate.status =
            DeltaStatus::candidate((now_at - chrono::TimeDelta::seconds(60)).to_rfc3339());
        service
            .submit_delta(&old_candidate)
            .await
            .expect("old candidate");

        let candidates = service
            .pull_candidate_deltas(&account_id)
            .await
            .expect("filtered read");
        assert_eq!(
            candidates.iter().map(|d| d.nonce).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "only candidate rows come back, nonce-ascending",
        );
        assert!(candidates.iter().all(|d| d.status.is_candidate()));

        let recent = service
            .pull_recent_candidate_deltas(now_at - chrono::TimeDelta::seconds(30), None, 10)
            .await
            .expect("recent filtered read");
        assert_eq!(
            recent
                .iter()
                .filter(|delta| delta.account_id == account_id)
                .map(|delta| delta.nonce)
                .collect::<Vec<_>>(),
            vec![2, 3],
            "the timestamp cutoff is applied in the store",
        );

        let first_page = service
            .pull_recent_candidate_deltas(now_at - chrono::TimeDelta::seconds(30), None, 1)
            .await
            .expect("first recent page");
        let cursor = crate::storage::RecentCandidateCursor {
            last_status_timestamp: now_at,
            last_account_id: account_id.clone(),
            last_nonce: first_page[0].nonce,
        };
        let second_page = service
            .pull_recent_candidate_deltas(now_at - chrono::TimeDelta::seconds(30), Some(&cursor), 1)
            .await
            .expect("second recent page");
        assert_eq!(second_page[0].nonce, 3, "cursor advances the SQL page");
    }

    /// Deep-history guard for the candidate-only read. The trait default
    /// for `pull_candidate_deltas` is `pull_deltas_after(0)` filtered in
    /// memory — a dropped Postgres override would still return correct
    /// results, so a small-scale test cannot catch it. This test seeds
    /// thousands of canonical/discarded rows with ~2 KiB payloads behind
    /// one candidate and requires the filtered read to be at least 5×
    /// faster than the full-history read (the real ratio is orders of
    /// magnitude; the margin absorbs timer jitter).
    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn pull_candidate_deltas_stays_flat_under_deep_history() {
        use crate::delta_object::DeltaStatus;
        use diesel::sql_types::{BigInt, Text};

        const CANONICAL_ROWS: i64 = 5_000;
        const DISCARDED_ROWS: i64 = 2_000;

        let url = crate::testing::pg::test_database_url().await;
        run_migrations(&url).await.expect("migrations apply");

        let service = PostgresService::new(&url, 4).await.expect("storage");
        let stamp = chrono::Utc::now().timestamp_micros();
        let account_id = format!("0xhist{stamp}");
        let now = chrono::Utc::now().to_rfc3339();

        let measurement_result: Result<_, String> = async {
            let mut conn = service
                .pool
                .get()
                .await
                .map_err(|error| error.to_string())?;
            diesel::sql_query(
                "INSERT INTO account_metadata \
                 (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
                 VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), false)",
            )
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .map_err(|error| error.to_string())?;
            for (status_kind, from_nonce, to_nonce) in [
                ("canonical", 1, CANONICAL_ROWS),
                (
                    "discarded",
                    CANONICAL_ROWS + 1,
                    CANONICAL_ROWS + DISCARDED_ROWS,
                ),
            ] {
                diesel::sql_query(
                    "INSERT INTO deltas \
                     (account_id, nonce, prev_commitment, new_commitment, delta_payload, \
                      ack_sig, status, status_kind, status_timestamp) \
                     SELECT $1, gs, '0x123', '0x456', \
                            jsonb_build_object('padding', repeat('x', 2048)), '0xsig', \
                            jsonb_build_object('status', $2::text, 'timestamp', $3::text), \
                            $2, now() \
                     FROM generate_series($4, $5) gs",
                )
                .bind::<Text, _>(&account_id)
                .bind::<Text, _>(status_kind)
                .bind::<Text, _>(&now)
                .bind::<BigInt, _>(from_nonce)
                .bind::<BigInt, _>(to_nonce)
                .execute(&mut conn)
                .await
                .map_err(|error| error.to_string())?;
            }
            drop(conn);

            let candidate_nonce = (CANONICAL_ROWS + DISCARDED_ROWS + 1) as u64;
            let mut candidate = create_test_delta(&account_id, candidate_nonce);
            candidate.status = DeltaStatus::candidate(now.clone());
            service.submit_delta(&candidate).await?;

            service.pull_candidate_deltas(&account_id).await?;
            service.pull_deltas_after(&account_id, 0).await?;

            let started = std::time::Instant::now();
            let candidates = service.pull_candidate_deltas(&account_id).await?;
            let filtered_elapsed = started.elapsed();

            let started = std::time::Instant::now();
            let full_history = service.pull_deltas_after(&account_id, 0).await?;
            let full_elapsed = started.elapsed();

            Ok((
                candidate_nonce,
                candidates,
                full_history,
                filtered_elapsed,
                full_elapsed,
            ))
        }
        .await;

        let cleanup_result: Result<(), String> = async {
            let mut conn = service
                .pool
                .get()
                .await
                .map_err(|error| error.to_string())?;
            diesel::sql_query("DELETE FROM deltas WHERE account_id = $1")
                .bind::<Text, _>(&account_id)
                .execute(&mut conn)
                .await
                .map_err(|error| error.to_string())?;
            diesel::sql_query("DELETE FROM account_metadata WHERE account_id = $1")
                .bind::<Text, _>(&account_id)
                .execute(&mut conn)
                .await
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        .await;
        cleanup_result.expect("remove deep-history test rows");

        let (candidate_nonce, candidates, full_history, filtered_elapsed, full_elapsed) =
            measurement_result.expect("measure candidate-only read under deep history");

        assert_eq!(
            candidates.iter().map(|d| d.nonce).collect::<Vec<_>>(),
            vec![candidate_nonce],
            "only the single candidate comes back from a deep history",
        );
        assert!(candidates[0].status.is_candidate());
        assert_eq!(
            full_history.len() as i64,
            CANONICAL_ROWS + DISCARDED_ROWS + 1,
            "control read must cover the full seeded history",
        );
        assert!(
            filtered_elapsed * 5 < full_elapsed,
            "candidate-only read must not scale with history depth: \
             filtered={filtered_elapsed:?} full={full_elapsed:?}",
        );
    }

    /// DB-side proof of the issue #345 SQL paths: the recoverable reads
    /// (retained rows plus cutoff-bounded client-abandoned discards),
    /// the exact-kind promotion gate, the kind-guarded discard, the
    /// submit-time supersede, and the retained-over-intent refusal in
    /// `update_candidate_status`. These behaviors live in Postgres
    /// predicates the filesystem tests cannot exercise.
    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn retain_and_reconcile_writes_are_kind_exact() {
        use crate::coordination::LeaderElector;
        use crate::coordination::postgres::PgLeaseElector;
        use crate::delta_object::{DeltaStatus, RetainReason};
        use crate::storage::{AbandonIntent, CandidatePromotion};
        use diesel::sql_types::Text;
        use std::time::Duration;

        let url = crate::testing::pg::test_database_url().await;
        run_migrations(&url).await.expect("migrations apply");

        let service = PostgresService::new(&url, 4).await.expect("storage");
        let metadata_store = crate::metadata::postgres::PostgresMetadataStore::new(&url, 2)
            .await
            .expect("metadata store");
        let stamp = chrono::Utc::now().timestamp_micros();
        let account_id = format!("0xretain{stamp}");
        let lease_name = format!("retain-fence-{stamp}");
        let now_dt = chrono::Utc::now();
        let now = now_dt.to_rfc3339();

        let mut conn = service.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), false)",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata row");
        drop(conn);

        let initial_state = create_test_state(&account_id);
        let initial_commitment = initial_state.commitment.clone();
        service
            .submit_state(&initial_state)
            .await
            .expect("insert initial state");

        // Seed one row per lifecycle in scope: canonical history (1),
        // a retained row (10), a recent client-abandoned discard (11),
        // and an abandoned discard past the cutoff (12).
        let canonical = create_test_delta(&account_id, 1);
        service.submit_delta(&canonical).await.expect("canonical");
        let mut retained = create_test_delta(&account_id, 10);
        retained.prev_commitment = initial_commitment.clone();
        retained.status = DeltaStatus::retained(now.clone(), RetainReason::RetryExhausted);
        service.submit_delta(&retained).await.expect("retained");
        let mut abandoned_recent = create_test_delta(&account_id, 11);
        abandoned_recent.status = DeltaStatus::discarded_client_abandoned(now.clone());
        service
            .submit_delta(&abandoned_recent)
            .await
            .expect("abandoned recent");
        let mut abandoned_old = create_test_delta(&account_id, 12);
        abandoned_old.status = DeltaStatus::discarded_client_abandoned(
            (now_dt - chrono::Duration::days(2)).to_rfc3339(),
        );
        service
            .submit_delta(&abandoned_old)
            .await
            .expect("abandoned old");

        // Recoverable reads: every retained row, abandoned rows only at
        // or after the cutoff, canonical history never.
        let cutoff = now_dt - chrono::Duration::days(1);
        let recoverable = service
            .pull_recoverable_deltas(&account_id, cutoff)
            .await
            .expect("recoverable read");
        assert_eq!(
            recoverable.iter().map(|d| d.nonce).collect::<Vec<_>>(),
            vec![10, 11],
            "retained plus recent-abandoned, nonce-ascending",
        );
        assert!(
            service
                .list_accounts_with_recoverable_deltas(cutoff)
                .await
                .expect("account scan")
                .contains(&account_id)
        );
        let strict = service
            .pull_recoverable_deltas(&account_id, now_dt + chrono::Duration::minutes(1))
            .await
            .expect("strict-cutoff read");
        assert_eq!(
            strict.iter().map(|d| d.nonce).collect::<Vec<_>>(),
            vec![10],
            "a future cutoff drops the abandoned arm but never retained rows",
        );

        let elector = PgLeaseElector::new(
            build_postgres_pool_lazy(&url, 2).unwrap(),
            &lease_name,
            "holder",
        );
        let lease = elector
            .try_acquire(Duration::from_secs(60))
            .await
            .expect("acquire")
            .expect("holder owns the lease");
        let fence = LeaseFence {
            lease_name: lease.name.clone(),
            holder_id: lease.holder_id.clone(),
            fence_token: lease.fence_token,
        };

        // Exact-kind promotion gate: a candidate-sourced promotion must
        // refuse the retained row (the wrong-row supersede race), and
        // the retained-sourced promotion commits atomically.
        let mut promoted_state = create_test_state(&account_id);
        promoted_state.commitment = "0xreconciled".to_string();
        let mut canonical_retained = retained.clone();
        canonical_retained.status = DeltaStatus::canonical(now.clone());
        canonical_retained.new_commitment = Some("0xreconciled".to_string());
        let wrong_source = service
            .promote_candidate(
                &metadata_store,
                CandidatePromotion {
                    state: promoted_state.clone(),
                    delta: canonical_retained.clone(),
                    new_auth: None,
                    now: now.clone(),
                    fence: Some(fence.clone()),
                    source: PromotableKind::Candidate,
                },
            )
            .await
            .expect("wrong-source promotion resolves");
        assert_eq!(wrong_source, PromoteWrite::NotCandidate);
        assert!(
            service
                .pull_delta(&account_id, 10)
                .await
                .expect("row survives")
                .status
                .is_retained(),
            "a refused promotion mutates nothing",
        );
        let promoted = service
            .promote_candidate(
                &metadata_store,
                CandidatePromotion {
                    state: promoted_state,
                    delta: canonical_retained,
                    new_auth: None,
                    now: now.clone(),
                    fence: Some(fence.clone()),
                    source: PromotableKind::Retained,
                },
            )
            .await
            .expect("retained-source promotion resolves");
        assert_eq!(promoted, PromoteWrite::Applied);
        assert!(
            service
                .pull_delta(&account_id, 10)
                .await
                .expect("row readable")
                .status
                .is_canonical()
        );
        assert_eq!(
            service
                .pull_state(&account_id)
                .await
                .expect("state readable")
                .commitment,
            "0xreconciled",
            "the reconcile promotion advances the stored base",
        );

        // Kind-guarded discard: a candidate-kind discard spares the
        // retained row; the retained-kind discard removes it.
        let mut retained_expiring = create_test_delta(&account_id, 13);
        retained_expiring.status = DeltaStatus::retained(now.clone(), RetainReason::Diverged);
        service
            .submit_delta(&retained_expiring)
            .await
            .expect("retained expiring");
        let wrong_kind = service
            .discard_candidate(
                &metadata_store,
                &account_id,
                13,
                DeltaStatusKind::Candidate,
                &now,
                Some(&fence),
            )
            .await
            .expect("wrong-kind discard resolves");
        assert_eq!(wrong_kind, CanonicalWrite::NotCandidate);
        assert!(service.pull_delta(&account_id, 13).await.is_ok());
        let right_kind = service
            .discard_candidate(
                &metadata_store,
                &account_id,
                13,
                DeltaStatusKind::Retained,
                &now,
                Some(&fence),
            )
            .await
            .expect("retained-kind discard resolves");
        assert_eq!(right_kind, CanonicalWrite::Applied);
        assert!(service.pull_delta(&account_id, 13).await.is_err());

        // Submit-time supersede: a fresh candidate replaces the abandoned
        // row at its nonce inside the submission transaction, while the
        // canonical row stays untouched.
        let mut resubmission = create_test_delta(&account_id, 11);
        resubmission.prev_commitment = "0xreconciled".to_string();
        resubmission.status = DeltaStatus::candidate(now.clone());
        let superseded = service
            .submit_candidate(&metadata_store, &resubmission, &now)
            .await
            .expect("resubmission resolves");
        assert_eq!(superseded, CandidateSubmission::Submitted);
        assert!(
            service
                .pull_delta(&account_id, 11)
                .await
                .expect("row readable")
                .status
                .is_candidate(),
            "the resubmission replaced the abandoned discard",
        );
        assert!(
            service
                .pull_delta(&account_id, 1)
                .await
                .expect("canonical readable")
                .status
                .is_canonical(),
            "settled history is never superseded",
        );

        // Retained-over-intent refusal: once an abandon intent is
        // recorded, the retained flip must refuse rather than wipe it.
        let intent = service
            .request_candidate_abandon(&account_id, 11, &now)
            .await
            .expect("intent resolves");
        assert_eq!(intent, AbandonIntent::Recorded);
        let refused = service
            .update_candidate_status(
                &account_id,
                11,
                DeltaStatus::retained(now.clone(), RetainReason::RetryExhausted),
                Some(&fence),
            )
            .await
            .expect("retained flip resolves");
        assert_eq!(refused, CanonicalWrite::NotCandidate);
        let row = service
            .pull_delta(&account_id, 11)
            .await
            .expect("row readable");
        assert!(row.status.is_candidate());
        assert!(
            row.status.abandon_requested_at().is_some(),
            "the intent survives the refused flip",
        );

        // Best-effort cleanup.
        let mut conn = service.pool.get().await.expect("conn");
        let _ = diesel::sql_query("DELETE FROM deltas WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await;
        let _ = diesel::sql_query("DELETE FROM states WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await;
        let _ = diesel::sql_query("DELETE FROM account_metadata WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await;
        let _ = diesel::sql_query("DELETE FROM worker_leases WHERE lease_name = $1")
            .bind::<Text, _>(&lease_name)
            .execute(&mut conn)
            .await;
    }

    /// End-to-end proof of the transactional fence: a superseded lease
    /// holder's retry, discard, and promotion are all refused with no row
    /// mutated; the current holder promotes atomically; and once canonical,
    /// the delta survives both a repeated promotion and a discard attempt.
    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn fenced_canonicalization_writes_reject_stale_owners() {
        use crate::coordination::LeaderElector;
        use crate::coordination::postgres::PgLeaseElector;
        use crate::delta_object::DeltaStatus;
        use diesel::sql_types::Text;
        use std::time::Duration;

        let url = crate::testing::pg::test_database_url().await;
        run_migrations(&url).await.expect("migrations apply");

        let service = PostgresService::new(&url, 4).await.expect("storage");
        let metadata_store = crate::metadata::postgres::PostgresMetadataStore::new(&url, 2)
            .await
            .expect("metadata store");
        let stamp = chrono::Utc::now().timestamp_micros();
        let account_id = format!("0xfence{stamp}");
        let lease_name = format!("canon-fence-{stamp}");
        let now = chrono::Utc::now().to_rfc3339();

        let mut conn = service.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), false)",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata row");
        drop(conn);

        let initial_state = create_test_state(&account_id);
        let initial_commitment = initial_state.commitment.clone();
        service
            .submit_state(&initial_state)
            .await
            .expect("insert initial state");

        let mut candidate = create_test_delta(&account_id, 1);
        candidate.prev_commitment = initial_commitment.clone();
        candidate.status = DeltaStatus::candidate(now.clone());
        let submitted = service
            .submit_candidate(&metadata_store, &candidate, &now)
            .await
            .expect("candidate insert + flag set commit together");
        assert_eq!(submitted, CandidateSubmission::Submitted);

        let racing_duplicate = service
            .submit_candidate(&metadata_store, &candidate, &now)
            .await
            .expect("duplicate submission resolves");
        assert_eq!(
            racing_duplicate,
            CandidateSubmission::Conflict,
            "a second submission that raced past the service gate must not commit",
        );
        let mut second_nonce = create_test_delta(&account_id, 2);
        second_nonce.prev_commitment = initial_commitment.clone();
        second_nonce.status = DeltaStatus::candidate(now.clone());
        assert_eq!(
            service
                .submit_candidate(&metadata_store, &second_nonce, &now)
                .await
                .expect("second-nonce submission resolves"),
            CandidateSubmission::Conflict,
            "one pending candidate per account, regardless of nonce",
        );
        let flag = |account: String| {
            let pool = service.pool.clone();
            async move {
                let mut conn = pool.get().await.expect("conn");
                account_metadata::table
                    .filter(account_metadata::account_id.eq(account))
                    .select(account_metadata::has_pending_candidate)
                    .first::<bool>(&mut conn)
                    .await
                    .expect("flag read")
            }
        };
        assert!(flag(account_id.clone()).await, "submit sets the flag");

        let elector_a =
            PgLeaseElector::new(build_postgres_pool_lazy(&url, 2).unwrap(), &lease_name, "a");
        let elector_b =
            PgLeaseElector::new(build_postgres_pool_lazy(&url, 2).unwrap(), &lease_name, "b");
        let lease_a = elector_a
            .try_acquire(Duration::from_secs(1))
            .await
            .expect("acquire a")
            .expect("a owns the lease");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let lease_b = elector_b
            .try_acquire(Duration::from_secs(60))
            .await
            .expect("acquire b")
            .expect("b steals the expired lease");
        let stale_fence = LeaseFence {
            lease_name: lease_a.name.clone(),
            holder_id: lease_a.holder_id.clone(),
            fence_token: lease_a.fence_token,
        };
        let current_fence = LeaseFence {
            lease_name: lease_b.name.clone(),
            holder_id: lease_b.holder_id.clone(),
            fence_token: lease_b.fence_token,
        };

        let stale_retry = service
            .update_candidate_status(
                &account_id,
                1,
                DeltaStatus::candidate_with_retry(now.clone(), 1),
                Some(&stale_fence),
            )
            .await
            .expect("stale retry resolves");
        assert_eq!(stale_retry, CanonicalWrite::StaleLease);
        assert_eq!(
            service.pull_delta(&account_id, 1).await.unwrap().status,
            candidate.status,
            "a refused retry mutates nothing",
        );

        let stale_discard = service
            .discard_candidate(
                &metadata_store,
                &account_id,
                1,
                DeltaStatusKind::Candidate,
                &now,
                Some(&stale_fence),
            )
            .await
            .expect("stale discard resolves");
        assert_eq!(stale_discard, CanonicalWrite::StaleLease);
        assert!(
            service.pull_delta(&account_id, 1).await.is_ok(),
            "a refused discard deletes nothing",
        );

        let mut canonical = candidate.clone();
        canonical.status = DeltaStatus::canonical(now.clone());
        let mut promoted_state = create_test_state(&account_id);
        promoted_state.commitment = "0xpromoted".to_string();
        let promotion = CandidatePromotion {
            state: promoted_state,
            delta: canonical,
            new_auth: None,
            now: now.clone(),
            fence: Some(current_fence.clone()),
            source: PromotableKind::Candidate,
        };

        let stale_promotion = service
            .promote_candidate(
                &metadata_store,
                CandidatePromotion {
                    fence: Some(stale_fence),
                    ..promotion.clone()
                },
            )
            .await
            .expect("stale promotion resolves");
        assert_eq!(stale_promotion, PromoteWrite::StaleLease);

        let unfenced = service
            .promote_candidate(
                &metadata_store,
                CandidatePromotion {
                    fence: None,
                    ..promotion.clone()
                },
            )
            .await;
        assert!(
            unfenced.is_err_and(|e| e.contains("lease fence")),
            "the Postgres backend refuses unfenced canonicalization writes",
        );

        let stale_base = service
            .promote_candidate(
                &metadata_store,
                CandidatePromotion {
                    delta: crate::delta_object::DeltaObject {
                        prev_commitment: "0xsome_other_base".to_string(),
                        ..promotion.delta.clone()
                    },
                    ..promotion.clone()
                },
            )
            .await
            .expect("stale-base promotion resolves");
        assert_eq!(
            stale_base,
            PromoteWrite::StaleBase,
            "a promotion whose base moved must be refused",
        );
        assert!(
            service
                .pull_delta(&account_id, 1)
                .await
                .unwrap()
                .status
                .is_candidate(),
            "the rolled-back promotion must not leave the delta canonical",
        );
        assert_eq!(
            service.pull_state(&account_id).await.unwrap().commitment,
            initial_commitment,
            "the rolled-back promotion must not advance the state",
        );

        let promoted = service
            .promote_candidate(&metadata_store, promotion.clone())
            .await
            .expect("current owner promotes");
        assert_eq!(promoted, PromoteWrite::Applied);
        assert!(
            service
                .pull_delta(&account_id, 1)
                .await
                .unwrap()
                .status
                .is_canonical(),
            "promotion flips the delta to canonical",
        );
        assert_eq!(
            service.pull_state(&account_id).await.unwrap().commitment,
            "0xpromoted",
            "promotion advances the state in the same commit",
        );
        assert!(
            !flag(account_id.clone()).await,
            "promotion releases the pending-candidate flag in the same commit",
        );

        let mut stale_state_candidate = create_test_delta(&account_id, 2);
        stale_state_candidate.prev_commitment = initial_commitment;
        stale_state_candidate.status = DeltaStatus::candidate(now.clone());
        assert_eq!(
            service
                .submit_candidate(&metadata_store, &stale_state_candidate, &now)
                .await
                .expect("stale-state submission resolves"),
            CandidateSubmission::CommitmentMismatch {
                expected: "0xpromoted".to_string(),
            },
            "the transaction rejects a candidate built before the promotion",
        );

        let repeat = service
            .promote_candidate(&metadata_store, promotion)
            .await
            .expect("repeat promotion resolves");
        assert_eq!(
            repeat,
            PromoteWrite::NotCandidate,
            "a promotion re-applied to a canonical delta is a no-op",
        );

        let discard_canonical = service
            .discard_candidate(
                &metadata_store,
                &account_id,
                1,
                DeltaStatusKind::Candidate,
                &now,
                Some(&current_fence),
            )
            .await
            .expect("discard of canonical resolves");
        assert_eq!(
            discard_canonical,
            CanonicalWrite::NotCandidate,
            "canonical lineage survives every discard attempt",
        );
        assert!(service.pull_delta(&account_id, 1).await.is_ok());

        elector_b.release(lease_b).await.expect("release");
        let mut conn = service.pool.get().await.expect("conn");
        diesel::sql_query("DELETE FROM deltas WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .expect("cleanup deltas");
        diesel::sql_query("DELETE FROM states WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .expect("cleanup states");
        diesel::sql_query("DELETE FROM account_metadata WHERE account_id = $1")
            .bind::<Text, _>(&account_id)
            .execute(&mut conn)
            .await
            .expect("cleanup metadata");
        diesel::sql_query("DELETE FROM worker_leases WHERE lease_name = $1")
            .bind::<Text, _>(&lease_name)
            .execute(&mut conn)
            .await
            .expect("cleanup lease");
    }

    /// The non-locking fence's core safety property. An in-flight promotion
    /// that validated its lease keeps the account-metadata row locked, so:
    /// (1) a leadership transfer (release + steal) does not wait on it — the
    /// fence read holds no lock; (2) the in-flight write still commits its real
    /// candidate→canonical mutation after the transfer; and (3) the new owner's
    /// competing promotion serializes behind the account lock and is then
    /// neutralized by the candidate conditional (NotCandidate), so the delta is
    /// promoted exactly once with no double-apply.
    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn lease_transfer_does_not_wait_for_a_validated_write_transaction() {
        use crate::coordination::LeaderElector;
        use crate::coordination::postgres::PgLeaseElector;
        use crate::delta_object::DeltaStatus;
        use diesel::sql_types::Text;
        use std::time::Duration;
        use tokio::sync::oneshot;

        let url = crate::testing::pg::test_database_url().await;
        run_migrations(&url).await.expect("migrations apply");

        let service = PostgresService::new(&url, 4).await.expect("storage");
        let metadata_store = crate::metadata::postgres::PostgresMetadataStore::new(&url, 2)
            .await
            .expect("metadata store");
        let stamp = chrono::Utc::now().timestamp_micros();
        let account_id = format!("0xoverlap{stamp}");
        let lease_name = format!("canon-overlap-{stamp}");
        let now = chrono::Utc::now().to_rfc3339();

        let mut conn = service.pool.get().await.expect("conn");
        diesel::sql_query(
            "INSERT INTO account_metadata \
             (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
             VALUES ($1, '{}'::jsonb, '{}'::jsonb, now(), now(), false)",
        )
        .bind::<Text, _>(&account_id)
        .execute(&mut conn)
        .await
        .expect("insert metadata row");
        drop(conn);

        let initial_state = create_test_state(&account_id);
        let initial_commitment = initial_state.commitment.clone();
        service
            .submit_state(&initial_state)
            .await
            .expect("insert initial state");
        let mut candidate = create_test_delta(&account_id, 1);
        candidate.prev_commitment = initial_commitment.clone();
        candidate.status = DeltaStatus::candidate(now.clone());
        service
            .submit_candidate(&metadata_store, &candidate, &now)
            .await
            .expect("candidate insert");

        let elector_a =
            PgLeaseElector::new(build_postgres_pool_lazy(&url, 2).unwrap(), &lease_name, "a");
        let elector_b =
            PgLeaseElector::new(build_postgres_pool_lazy(&url, 2).unwrap(), &lease_name, "b");
        let lease_a = elector_a
            .try_acquire(Duration::from_secs(60))
            .await
            .expect("acquire a")
            .expect("a owns the lease");
        let fence = LeaseFence {
            lease_name: lease_a.name.clone(),
            holder_id: lease_a.holder_id.clone(),
            fence_token: lease_a.fence_token,
        };

        let canonical_status = DeltaStatus::canonical(now.clone());
        let status_json = serde_json::to_value(&canonical_status).expect("serialize status");
        let (status_kind, status_timestamp) =
            derive_status_columns(&canonical_status).expect("status columns");
        let pool = service.pool.clone();
        let inflight_account_id = account_id.clone();
        let (validated_tx, validated_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();

        // The in-flight promotion: lock the account, validate the fence (still
        // current), park while holding the account lock, then commit the real
        // candidate→canonical flip after being resumed post-transfer.
        let write = tokio::spawn(async move {
            let mut conn = pool.get().await.expect("write connection");
            conn.transaction::<usize, diesel::result::Error, _>(|conn| {
                async move {
                    lock_account_metadata(conn, &inflight_account_id).await?;
                    assert!(lease_fence_is_current(conn, &fence).await?);
                    validated_tx.send(()).expect("signal validation");
                    resume_rx.await.expect("resume validated transaction");
                    let flipped = diesel::update(deltas::table)
                        .filter(deltas::account_id.eq(&inflight_account_id))
                        .filter(deltas::nonce.eq(1i64))
                        .filter(deltas::status_kind.eq("candidate"))
                        .set((
                            deltas::status.eq(&status_json),
                            deltas::status_kind.eq(status_kind),
                            deltas::status_timestamp.eq(status_timestamp),
                        ))
                        .execute(conn)
                        .await?;
                    Ok(flipped)
                }
                .scope_boxed()
            })
            .await
            .expect("validated transaction commits")
        });

        validated_rx.await.expect("write validates its lease");

        // (1) The transfer must not wait for the in-flight, account-locked write.
        tokio::time::timeout(Duration::from_secs(2), elector_a.release(lease_a))
            .await
            .expect("lease transfer must not wait for the validated transaction")
            .expect("release a");
        let lease_b = elector_b
            .try_acquire(Duration::from_secs(60))
            .await
            .expect("acquire b")
            .expect("b takes over while the prior transaction is open");
        let current_fence = LeaseFence {
            lease_name: lease_b.name.clone(),
            holder_id: lease_b.holder_id.clone(),
            fence_token: lease_b.fence_token,
        };

        // (3) The new owner's competing promotion serializes behind the account
        // lock the in-flight write holds; it can only run once that write commits.
        let mut canonical = candidate.clone();
        canonical.status = canonical_status.clone();
        let mut newowner_state = create_test_state(&account_id);
        newowner_state.commitment = "0xnewowner".to_string();
        let newowner_promotion = CandidatePromotion {
            state: newowner_state,
            delta: canonical,
            new_auth: None,
            now: now.clone(),
            fence: Some(current_fence),
            source: PromotableKind::Candidate,
        };
        let newowner_service = PostgresService::new(&url, 2)
            .await
            .expect("new-owner storage");
        let newowner_metadata = crate::metadata::postgres::PostgresMetadataStore::new(&url, 2)
            .await
            .expect("new-owner metadata store");
        let newowner = tokio::spawn(async move {
            newowner_service
                .promote_candidate(&newowner_metadata, newowner_promotion)
                .await
                .expect("new-owner promotion resolves")
        });

        resume_tx.send(()).expect("resume write");
        let flipped = write.await.expect("write task");
        assert_eq!(flipped, 1, "the in-flight write commits its real mutation");

        // (2)+(3): the delta is canonical, promoted exactly once; the new owner's
        // promotion found no candidate and left the state untouched.
        let newowner_outcome = newowner.await.expect("new-owner task");
        assert_eq!(
            newowner_outcome,
            PromoteWrite::NotCandidate,
            "the new owner cannot re-promote a delta the in-flight write already flipped",
        );
        assert!(
            service
                .pull_delta(&account_id, 1)
                .await
                .unwrap()
                .status
                .is_canonical(),
            "the delta is canonical exactly once",
        );

        elector_b.release(lease_b).await.expect("cleanup lease");
        let mut conn = service.pool.get().await.expect("conn");
        for stmt in [
            "DELETE FROM deltas WHERE account_id = $1",
            "DELETE FROM states WHERE account_id = $1",
            "DELETE FROM account_metadata WHERE account_id = $1",
        ] {
            diesel::sql_query(stmt)
                .bind::<Text, _>(&account_id)
                .execute(&mut conn)
                .await
                .expect("cleanup account rows");
        }
        diesel::sql_query("DELETE FROM worker_leases WHERE lease_name = $1")
            .bind::<Text, _>(&lease_name)
            .execute(&mut conn)
            .await
            .expect("cleanup lease row");
    }
}
