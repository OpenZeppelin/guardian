// Integration tests (enabled with `--features integration`)
#![cfg(feature = "integration")]

mod auth_grpc;
mod auth_http;
mod lookup_grpc;
mod lookup_helpers;
mod lookup_http;
mod miden_rpc_integration;
mod proposals_grpc;
mod proposals_http;
