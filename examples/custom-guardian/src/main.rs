//! The Guardian server with policy module kinds compiled in: everything the official `server`
//! binary does ([`ServerBuilder::from_env`]), plus the `note-policy` kind, so a catalog entry
//! can name it with `kind = "note-policy"` and no endpoint. Register your own kinds the same
//! way. Walkthrough in `docs/guides/policy-modules/custom.md`.
//!
//! `GUARDIAN_EXAMPLE_HTTP_PORT` and `GUARDIAN_EXAMPLE_GRPC_PORT` (default 3000 and 50051) let it
//! run next to another server.

#[path = "../../../crates/guardian-module-sdk/examples/note_policy/mod.rs"]
mod note_policy;

use std::sync::Arc;

use server::builder::ServerBuilder;
use server::builder::env::{DEFAULT_GRPC_PORT, DEFAULT_HTTP_PORT};

fn port(var: &str, default: u16) -> u16 {
    std::env::var(var)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    ServerBuilder::from_env()
        .await
        .unwrap_or_else(|error| panic!("Failed to configure server: {error}"))
        .with_module_kind(note_policy::KIND, Arc::new(note_policy::NotePolicyFactory))
        .http(true, port("GUARDIAN_EXAMPLE_HTTP_PORT", DEFAULT_HTTP_PORT))
        .grpc(true, port("GUARDIAN_EXAMPLE_GRPC_PORT", DEFAULT_GRPC_PORT))
        .build()
        .await
        .unwrap_or_else(|error| panic!("Failed to build server: {error}"))
        .run()
        .await;
}
