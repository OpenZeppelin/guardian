use diesel::sql_types::Text;
use diesel::{Connection, PgConnection, QueryableByName, RunQueryDsl};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;
use tokio_postgres::error::SqlState;
use url::Url;

use crate::storage::postgres::{
    TestPostgresConnectionError, connect_test_postgres_client, run_migrations,
};

static PREPARED: OnceCell<String> = OnceCell::const_new();

const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT_SECONDS: &str = "5";
const RESET_LOCK_TIMEOUT: &str = "10s";

/// Return the connection URL for the Postgres-backed tests, resetting the
/// database to an empty schema on first use so no run inherits state from an
/// earlier one.
pub async fn test_database_url() -> String {
    PREPARED.get_or_init(prepare).await.clone()
}

#[derive(QueryableByName)]
struct ConnectedDatabase {
    #[diesel(sql_type = Text)]
    current_database: String,
}

enum Probe {
    Ready,
    Missing,
    Retryable(String),
    Configuration(String),
    Fatal(String),
}

async fn prepare() -> String {
    let url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .expect("DATABASE_URL must be set; run ./scripts/test-postgres.sh");

    let parsed = Url::parse(&url).expect("DATABASE_URL must be a postgres:// or postgresql:// URL");
    let declared = database_name(&parsed).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        is_test_database_name(&declared),
        "{}",
        refusal_message(&declared)
    );

    ensure_database_exists(&parsed, &declared).await;
    reset_public_schema(&url).await;
    run_migrations(&url).await.expect("migrations apply");

    url
}

fn database_name(url: &Url) -> Result<String, String> {
    let names: Vec<String> = url
        .query_pairs()
        .filter(|(key, _)| key == "dbname")
        .map(|(_, value)| value.into_owned())
        .collect();
    match names.as_slice() {
        [] => Ok(url.path().trim_start_matches('/').to_string()),
        [name] => Ok(name.clone()),
        _ => Err("DATABASE_URL must not contain duplicate 'dbname' parameters".to_string()),
    }
}

fn is_test_database_name(name: &str) -> bool {
    name.ends_with("_test")
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn refusal_message(name: &str) -> String {
    format!(
        "refusing to reset database {name:?}: the Postgres-backed test suite drops and recreates \
         the public schema, and will only do so on a database whose name ends in \"_test\" and \
         contains only letters, digits, and underscores"
    )
}

/// libpq accepts the database as a `dbname` query parameter as well as a path
/// segment, and the query parameter wins, so the name in the URL is not proof
/// of which database a connection landed on. Ask the server instead.
fn assert_connected_to_test_database(conn: &mut PgConnection) {
    let connected = diesel::sql_query("SELECT current_database()")
        .get_result::<ConnectedDatabase>(conn)
        .expect("read current database")
        .current_database;
    assert!(
        is_test_database_name(&connected),
        "{}",
        refusal_message(&connected)
    );
}

fn probe_url(url: &Url) -> String {
    let mut probe = url.clone();
    probe
        .query_pairs_mut()
        .append_pair("connect_timeout", CONNECT_TIMEOUT_SECONDS);
    probe.to_string()
}

fn maintenance_url(url: &Url) -> Url {
    let mut maintenance = url.clone();
    maintenance.set_path("/postgres");
    let preserved: Vec<(String, String)> = maintenance
        .query_pairs()
        .filter(|(key, _)| key != "dbname")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    {
        let mut pairs = maintenance.query_pairs_mut();
        pairs.clear();
        pairs.extend_pairs(preserved);
    }
    maintenance
}

async fn ensure_database_exists(url: &Url, name: &str) {
    let maintenance = maintenance_url(url);
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        match probe(probe_url(&maintenance), name).await {
            Probe::Ready => return,
            Probe::Missing => return create_database(url, name).await,
            Probe::Configuration(error) => {
                panic!("invalid DATABASE_URL configuration: {error}")
            }
            Probe::Fatal(error) => panic!("cannot reach {}: {error}", endpoint(url)),
            Probe::Retryable(error) if Instant::now() >= deadline => panic!(
                "Postgres at {} did not accept connections within {}s: {error}",
                endpoint(url),
                READINESS_TIMEOUT.as_secs()
            ),
            Probe::Retryable(_) => tokio::time::sleep(PROBE_INTERVAL).await,
        }
    }
}

fn is_retryable_postgres_error(error: &tokio_postgres::Error) -> bool {
    let Some(database_error) = error.as_db_error() else {
        return true;
    };
    matches!(
        *database_error.code(),
        SqlState::ADMIN_SHUTDOWN
            | SqlState::CRASH_SHUTDOWN
            | SqlState::CANNOT_CONNECT_NOW
            | SqlState::TOO_MANY_CONNECTIONS
    )
}

fn classify_postgres_error(error: tokio_postgres::Error) -> Probe {
    let message = error.to_string();
    if is_retryable_postgres_error(&error) {
        Probe::Retryable(message)
    } else {
        Probe::Fatal(message)
    }
}

/// `docker compose up -d` returns before Postgres accepts connections, so a
/// cold server has to be waited out rather than reported as broken. The probe
/// uses the maintenance database so a missing target can be detected without
/// parsing connection error text.
async fn probe(url: String, name: &str) -> Probe {
    let client = match connect_test_postgres_client(&url).await {
        Ok(client) => client,
        Err(TestPostgresConnectionError::Configuration(error)) => {
            return Probe::Configuration(error);
        }
        Err(TestPostgresConnectionError::Connection(error)) => {
            return classify_postgres_error(error);
        }
    };
    match client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            &[&name],
        )
        .await
    {
        Ok(row) if row.get::<_, bool>(0) => Probe::Ready,
        Ok(_) => Probe::Missing,
        Err(error) => classify_postgres_error(error),
    }
}

async fn create_database(url: &Url, name: &str) {
    let maintenance_url = maintenance_url(url).to_string();
    let name = name.to_string();
    let hint = format!(
        "createdb -h {} -U {} {name}",
        url.host_str().unwrap_or("localhost"),
        url.username()
    );

    tokio::task::spawn_blocking(move || {
        let mut conn = PgConnection::establish(&maintenance_url).unwrap_or_else(|error| {
            panic!(
                "database {name:?} does not exist and the maintenance database could not be \
                 opened to create it ({error}); create it manually with: {hint}"
            )
        });
        diesel::sql_query(format!("CREATE DATABASE \"{name}\""))
            .execute(&mut conn)
            .unwrap_or_else(|error| {
                panic!(
                    "failed to create database {name:?} ({error}); create it manually with: {hint}"
                )
            });
    })
    .await
    .expect("database creation task");
}

async fn reset_public_schema(url: &str) {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let mut conn = PgConnection::establish(&url).unwrap_or_else(|error| {
            panic!("DATABASE_URL must point at a reachable Postgres: {error}")
        });
        assert_connected_to_test_database(&mut conn);

        // An idle-in-transaction backend from an interrupted run would
        // otherwise block the drop indefinitely.
        diesel::sql_query(format!("SET lock_timeout = '{RESET_LOCK_TIMEOUT}'"))
            .execute(&mut conn)
            .expect("set lock timeout");
        // IF EXISTS: a run killed between the drop and the create leaves no
        // public schema, and the next run must still be able to reset.
        diesel::sql_query("DROP SCHEMA IF EXISTS public CASCADE")
            .execute(&mut conn)
            .expect("drop public schema");
        diesel::sql_query("CREATE SCHEMA public")
            .execute(&mut conn)
            .expect("create public schema");
    })
    .await
    .expect("schema reset task");
}

fn endpoint(url: &Url) -> String {
    format!(
        "{}:{}",
        url.host_str().unwrap_or("localhost"),
        url.port().unwrap_or(5432)
    )
}

#[cfg(test)]
mod tests {
    use super::{database_name, ensure_database_exists, is_test_database_name, maintenance_url};
    use url::Url;

    #[test]
    fn only_names_ending_in_test_may_be_reset() {
        assert!(is_test_database_name("guardian_test"));
        assert!(is_test_database_name("guardian_365_test"));

        assert!(!is_test_database_name("guardian"));
        assert!(!is_test_database_name("guardian_test_backup"));
        assert!(!is_test_database_name("testing"));
        assert!(!is_test_database_name(""));
    }

    #[test]
    fn names_needing_quoting_are_rejected() {
        assert!(!is_test_database_name(
            "guardian\"; DROP DATABASE guardian; --_test"
        ));
        assert!(!is_test_database_name("guardian test"));
        assert!(!is_test_database_name("guardian-test"));
    }

    #[test]
    fn declared_name_comes_from_the_url_path() {
        let url = Url::parse("postgres://guardian:guardian@localhost:5432/guardian_test").unwrap();
        assert_eq!(database_name(&url).unwrap(), "guardian_test");
    }

    #[test]
    fn dbname_parameter_overrides_the_url_path() {
        let url =
            Url::parse("postgres://u:p@localhost:5432/guardian_test?dbname=other_test").unwrap();
        assert_eq!(database_name(&url).unwrap(), "other_test");
    }

    #[test]
    fn maintenance_url_removes_dbname_and_preserves_other_parameters() {
        let url = Url::parse(
            "postgres://u:p@localhost:5432/guardian_test?dbname=other_test&sslmode=require",
        )
        .unwrap();

        assert_eq!(
            maintenance_url(&url).as_str(),
            "postgres://u:p@localhost:5432/postgres?sslmode=require"
        );
    }

    #[test]
    fn duplicate_dbname_parameters_are_rejected() {
        let url = Url::parse(
            "postgres://u:p@localhost:5432/guardian_test?dbname=one_test&dbname=two_test",
        )
        .unwrap();

        assert!(database_name(&url).is_err());
    }

    #[tokio::test]
    #[should_panic(expected = "invalid DATABASE_URL configuration: sslmode 'allow'/'prefer'")]
    async fn invalid_connection_options_are_reported_as_configuration_errors() {
        let url =
            Url::parse("postgres://guardian:guardian@localhost:5432/guardian_test?sslmode=prefer")
                .unwrap();

        ensure_database_exists(&url, "guardian_test").await;
    }
}
