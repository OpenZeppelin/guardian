// Integration tests (enabled with `--features integration`)
#![cfg(feature = "integration")]

mod auth_grpc;
mod auth_http;
mod body_limit_http;
mod error_envelope_http;
mod history_grpc;
mod history_http;
mod lookup_grpc;
mod lookup_helpers;
mod lookup_http;
mod metrics_http;
mod miden_rpc_integration;
mod proposals_grpc;
mod proposals_http;
mod rate_limit_grpc;
mod rate_limit_http;
