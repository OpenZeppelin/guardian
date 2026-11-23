//! `ServerBuilder::from_env` configures a server from the environment, as the official binary
//! and every custom build start. One test per file: it edits the process environment. A
//! Postgres build needs `DATABASE_URL`, so the test configures the filesystem backend only.

#![cfg(not(feature = "postgres"))]

use server::builder::ServerBuilder;

fn set(name: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: this test binary runs a single test, so no other thread reads the environment.
    unsafe { std::env::set_var(name, value) };
}

#[tokio::test]
async fn from_env_configures_a_filesystem_server_and_names_an_invalid_setting() {
    let dir = tempfile::tempdir().expect("temporary directory");
    for (name, sub) in [
        ("GUARDIAN_STORAGE_PATH", "storage"),
        ("GUARDIAN_METADATA_PATH", "metadata"),
        ("GUARDIAN_KEYSTORE_PATH", "keystore"),
    ] {
        set(name, dir.path().join(sub));
    }
    // SAFETY: as in `set`.
    unsafe { std::env::remove_var("DATABASE_URL") };
    set("GUARDIAN_ENV", "dev");
    set("GUARDIAN_NETWORK_TYPE", "local");

    if let Err(error) = ServerBuilder::from_env().await {
        panic!("a filesystem server configures from the environment: {error}");
    }

    set("GUARDIAN_NETWORK_TYPE", "nowhere");
    let error = match ServerBuilder::from_env().await {
        Ok(_) => panic!("an unknown network type must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("GUARDIAN_NETWORK_TYPE"), "{error}");
    assert!(error.contains("nowhere"), "{error}");
}
