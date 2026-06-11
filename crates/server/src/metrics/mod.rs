//! Native Prometheus integration (issue #225).
//!
//! Architecture:
//! - a global `metrics` recorder installed once in `ServerHandle::run()`
//!   ([`recorder`]); instrumentation call sites use the `metrics`
//!   macros and are no-ops when the recorder is absent (metrics
//!   disabled, unit tests);
//! - a dedicated listener serving the text exposition ([`router`]),
//!   bound to `GUARDIAN_METRICS_ADDR` (loopback by default) and
//!   optionally guarded by a shared-secret bearer token;
//! - request-path layers for HTTP ([`http`]) and gRPC ([`grpc`]);
//! - a storage decorator ([`storage`]) timing every backend call;
//! - a background refresher ([`refresher`]) publishing slow aggregates
//!   as gauges so scrapes never query storage.
//!
//! The metric taxonomy and its cardinality rules live in [`names`].
//! Everything is disabled unless `GUARDIAN_METRICS_ENABLED=true`
//! ([`config`]).

pub mod config;
pub mod grpc;
pub mod http;
pub mod labels;
pub mod names;
pub mod recorder;
pub mod refresher;
pub mod router;
pub mod storage;

pub use config::MetricsConfig;
pub use grpc::MetricsGrpcLayer;
pub use http::track_http;
pub use recorder::{build_recorder, describe_metrics, record_build_info};
pub use refresher::start_refresher;
pub use router::metrics_router;
pub use storage::InstrumentedStorage;
