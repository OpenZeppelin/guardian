use diesel::sql_types::Text;
use diesel::{Connection, PgConnection, QueryableByName, RunQueryDsl};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;
use url::Url;

use crate::storage::postgres::run_migrations;

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
    Fatal(String),
}

async fn prepare() -> String {
    let url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .expect("DATABASE_URL must be set; run ./scripts/test-postgres.sh");

    let parsed = Url::parse(&url).expect("DATABASE_URL must be a postgres:// or postgresql:// URL");
    let declared = database_name(&parsed);
    assert!(
        is_test_database_name(declared),
        "{}",
        refusal_message(declared)
    );

    ensure_database_exists(&parsed, declared).await;
    reset_public_schema(&url).await;
    run_migrations(&url).await.expect("migrations apply");

    url
}

fn database_name(url: &Url) -> &str {
    url.path().trim_start_matches('/')
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

async fn ensure_database_exists(url: &Url, name: &str) {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        match probe(probe_url(url), name.to_string()).await {
            Probe::Ready => return,
            Probe::Missing => return create_database(url, name).await,
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

/// `docker compose up -d` returns before Postgres accepts connections, so a
/// cold server has to be waited out rather than reported as broken. Only an
/// undefined database means "create it"; auth and host-level failures are the
/// operator's problem and must surface as themselves.
async fn probe(url: String, name: String) -> Probe {
    tokio::task::spawn_blocking(move || match PgConnection::establish(&url) {
        Ok(_) => Probe::Ready,
        Err(error) => {
            let message = error.to_string();
            if message.contains(&format!("database \"{name}\" does not exist")) {
                Probe::Missing
            } else if message.contains("authentication failed") || message.contains("pg_hba.conf") {
                Probe::Fatal(message)
            } else {
                Probe::Retryable(message)
            }
        }
    })
    .await
    .expect("connection probe task")
}

async fn create_database(url: &Url, name: &str) {
    let mut maintenance = url.clone();
    maintenance.set_path("/postgres");
    let maintenance_url = maintenance.to_string();
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
    use super::{database_name, is_test_database_name};
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
        assert_eq!(database_name(&url), "guardian_test");
    }

    #[test]
    fn a_dbname_parameter_hides_the_real_target_from_the_path() {
        let url =
            Url::parse("postgres://u:p@localhost:5432/guardian_test?dbname=guardian").unwrap();
        assert_eq!(database_name(&url), "guardian_test");
        assert!(
            is_test_database_name(database_name(&url)),
            "the path alone cannot prove the target, which is why the reset re-checks \
             current_database() against the server"
        );
    }
}
