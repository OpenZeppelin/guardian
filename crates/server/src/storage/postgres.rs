use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::schema::{delta_proposals, deltas, states};
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use crate::storage::{
    AccountDeltaCursor, AccountProposalCursor, DeltaStatusCounts, DeltaStatusKind,
    GlobalDeltaCursor, GlobalDeltaRow, GlobalProposalCursor, ProposalRecord, StorageType,
};
use async_trait::async_trait;
use diesel::ConnectionError;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::ManagerConfig;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
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

#[async_trait]
impl StorageBackend for PostgresService {
    fn kind(&self) -> StorageType {
        StorageType::Postgres
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
}
