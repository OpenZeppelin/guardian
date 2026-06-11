//! Metric name and label taxonomy for the Prometheus integration.
//!
//! Every metric the server emits is declared in [`REGISTRY`], which is
//! the single source of truth consumed by both the recorder (to attach
//! help text) and the cardinality unit tests (to enforce the label
//! allowlist). Naming follows the Prometheus conventions: `guardian_`
//! namespace, seconds as the base unit, `_total` suffix on counters.
//!
//! Cardinality policy: every label key must appear in
//! [`LABEL_ALLOWLIST`] and every label value must come from a bounded
//! set — route templates (never raw paths), gRPC method names from the
//! proto definition, static operation names, and small closed enums.
//! Account IDs, nonces, commitments, pubkeys, client IPs, and error
//! strings must never become label values.

// --- HTTP request path -------------------------------------------------

pub const HTTP_REQUESTS_TOTAL: &str = "guardian_http_requests_total";
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "guardian_http_request_duration_seconds";
pub const HTTP_REQUESTS_IN_FLIGHT: &str = "guardian_http_requests_in_flight";

// --- gRPC request path -------------------------------------------------

pub const GRPC_REQUESTS_TOTAL: &str = "guardian_grpc_requests_total";
pub const GRPC_REQUEST_DURATION_SECONDS: &str = "guardian_grpc_request_duration_seconds";

// --- Storage backend ---------------------------------------------------

pub const STORAGE_OPERATIONS_TOTAL: &str = "guardian_storage_operations_total";
pub const STORAGE_OPERATION_DURATION_SECONDS: &str = "guardian_storage_operation_duration_seconds";

// --- Canonicalization jobs ----------------------------------------------

pub const CANONICALIZATION_RUNS_TOTAL: &str = "guardian_canonicalization_runs_total";
pub const CANONICALIZATION_RUN_DURATION_SECONDS: &str =
    "guardian_canonicalization_run_duration_seconds";
pub const CANONICALIZATION_CANDIDATES_TOTAL: &str = "guardian_canonicalization_candidates_total";
pub const CANONICALIZATION_RETRIES_TOTAL: &str = "guardian_canonicalization_retries_total";

// --- Delta / proposal lifecycle ------------------------------------------

pub const DELTAS_SUBMITTED_TOTAL: &str = "guardian_deltas_submitted_total";
pub const PROPOSALS_TOTAL: &str = "guardian_proposals_total";

// --- Operator auth / rate limiting ---------------------------------------

pub const OPERATOR_AUTH_CHALLENGES_TOTAL: &str = "guardian_operator_auth_challenges_total";
pub const OPERATOR_AUTH_VERIFICATIONS_TOTAL: &str = "guardian_operator_auth_verifications_total";
pub const OPERATOR_SESSIONS_STARTED_TOTAL: &str = "guardian_operator_sessions_started_total";
pub const RATE_LIMIT_REJECTIONS_TOTAL: &str = "guardian_rate_limit_rejections_total";

// --- Slow aggregates (set by the background refresher) -------------------

pub const DELTAS_GAUGE: &str = "guardian_deltas";
pub const PROPOSALS_IN_FLIGHT: &str = "guardian_proposals_in_flight";
pub const ACCOUNTS_GAUGE: &str = "guardian_accounts";
pub const METRICS_REFRESH_TIMESTAMP_SECONDS: &str = "guardian_metrics_refresh_timestamp_seconds";
pub const METRICS_REFRESH_FAILURES_TOTAL: &str = "guardian_metrics_refresh_failures_total";

// --- Build identity ------------------------------------------------------

pub const BUILD_INFO: &str = "guardian_build_info";

// --- Label keys -----------------------------------------------------------

pub const LABEL_METHOD: &str = "method";
pub const LABEL_ROUTE: &str = "route";
pub const LABEL_STATUS: &str = "status";
pub const LABEL_SERVICE: &str = "service";
pub const LABEL_CODE: &str = "code";
pub const LABEL_OPERATION: &str = "operation";
pub const LABEL_OUTCOME: &str = "outcome";
pub const LABEL_KIND: &str = "kind";
pub const LABEL_EVENT: &str = "event";
pub const LABEL_LIMIT_TYPE: &str = "limit_type";
pub const LABEL_VERSION: &str = "version";
pub const LABEL_GIT_COMMIT: &str = "git_commit";
pub const LABEL_PROFILE: &str = "profile";

/// Every label key any guardian metric is allowed to carry. The
/// `REGISTRY` unit test enforces membership; adding a new label means
/// consciously extending this list and re-reviewing its cardinality.
pub const LABEL_ALLOWLIST: &[&str] = &[
    LABEL_METHOD,
    LABEL_ROUTE,
    LABEL_STATUS,
    LABEL_SERVICE,
    LABEL_CODE,
    LABEL_OPERATION,
    LABEL_OUTCOME,
    LABEL_KIND,
    LABEL_EVENT,
    LABEL_LIMIT_TYPE,
    LABEL_VERSION,
    LABEL_GIT_COMMIT,
    LABEL_PROFILE,
];

/// Metric type, mirroring the Prometheus instrument kinds in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

/// One declared metric: name, kind, allowed label keys, and help text.
pub struct MetricDef {
    pub name: &'static str,
    pub kind: MetricKind,
    pub labels: &'static [&'static str],
    pub help: &'static str,
}

/// The full taxonomy. Order matches the exposition grouping; the
/// recorder describes each entry at startup and the tests in this
/// module enforce naming and label-cardinality rules over it.
pub const REGISTRY: &[MetricDef] = &[
    MetricDef {
        name: BUILD_INFO,
        kind: MetricKind::Gauge,
        labels: &[LABEL_VERSION, LABEL_GIT_COMMIT, LABEL_PROFILE],
        help: "Build identity of the running guardian-server (constant 1).",
    },
    MetricDef {
        name: HTTP_REQUESTS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_METHOD, LABEL_ROUTE, LABEL_STATUS],
        help: "HTTP requests served, by method, route template, and status code.",
    },
    MetricDef {
        name: HTTP_REQUEST_DURATION_SECONDS,
        kind: MetricKind::Histogram,
        labels: &[LABEL_METHOD, LABEL_ROUTE],
        help: "HTTP request latency in seconds, by method and route template.",
    },
    MetricDef {
        name: HTTP_REQUESTS_IN_FLIGHT,
        kind: MetricKind::Gauge,
        labels: &[],
        help: "HTTP requests currently being served.",
    },
    MetricDef {
        name: GRPC_REQUESTS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_SERVICE, LABEL_METHOD, LABEL_CODE],
        help: "gRPC requests served, by service, method, and status code.",
    },
    MetricDef {
        name: GRPC_REQUEST_DURATION_SECONDS,
        kind: MetricKind::Histogram,
        labels: &[LABEL_SERVICE, LABEL_METHOD],
        help: "gRPC request latency in seconds, by service and method.",
    },
    MetricDef {
        name: STORAGE_OPERATIONS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_OPERATION, LABEL_OUTCOME],
        help: "Storage backend operations, by operation name and outcome.",
    },
    MetricDef {
        name: STORAGE_OPERATION_DURATION_SECONDS,
        kind: MetricKind::Histogram,
        labels: &[LABEL_OPERATION],
        help: "Storage backend operation latency in seconds, by operation name.",
    },
    MetricDef {
        name: CANONICALIZATION_RUNS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_OUTCOME],
        help: "Canonicalization worker passes over all accounts, by outcome.",
    },
    MetricDef {
        name: CANONICALIZATION_RUN_DURATION_SECONDS,
        kind: MetricKind::Histogram,
        labels: &[],
        help: "Duration of one canonicalization pass over all accounts, in seconds.",
    },
    MetricDef {
        name: CANONICALIZATION_CANDIDATES_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_OUTCOME],
        help: "Candidate deltas processed by the canonicalization worker, by outcome \
               (canonicalized, retried, discarded, grace_deferred).",
    },
    MetricDef {
        name: CANONICALIZATION_RETRIES_TOTAL,
        kind: MetricKind::Counter,
        labels: &[],
        help: "Canonicalization verification retries consumed across all candidates.",
    },
    MetricDef {
        name: DELTAS_SUBMITTED_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_KIND],
        help: "Deltas accepted by the server, by submission kind (delta, proposal_commit).",
    },
    MetricDef {
        name: PROPOSALS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_EVENT],
        help: "Multisig proposal lifecycle events (created, signed, threshold_reached, \
               finalized).",
    },
    MetricDef {
        name: OPERATOR_AUTH_CHALLENGES_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_OUTCOME],
        help: "Operator dashboard login challenges issued, by outcome.",
    },
    MetricDef {
        name: OPERATOR_AUTH_VERIFICATIONS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_OUTCOME],
        help: "Operator dashboard login verification attempts, by outcome.",
    },
    MetricDef {
        name: OPERATOR_SESSIONS_STARTED_TOTAL,
        kind: MetricKind::Counter,
        labels: &[],
        help: "Operator dashboard sessions successfully started.",
    },
    MetricDef {
        name: RATE_LIMIT_REJECTIONS_TOTAL,
        kind: MetricKind::Counter,
        labels: &[LABEL_LIMIT_TYPE],
        help: "Requests rejected by the rate limiter, by limit type (burst, sustained).",
    },
    MetricDef {
        name: DELTAS_GAUGE,
        kind: MetricKind::Gauge,
        labels: &[LABEL_STATUS],
        help: "Persisted deltas by lifecycle status (candidate, canonical, discarded). \
               Refreshed asynchronously; see guardian_metrics_refresh_timestamp_seconds.",
    },
    MetricDef {
        name: PROPOSALS_IN_FLIGHT,
        kind: MetricKind::Gauge,
        labels: &[],
        help: "Pending multisig proposals awaiting cosigner signatures. Refreshed \
               asynchronously.",
    },
    MetricDef {
        name: ACCOUNTS_GAUGE,
        kind: MetricKind::Gauge,
        labels: &[],
        help: "Accounts configured on this guardian instance. Refreshed asynchronously.",
    },
    MetricDef {
        name: METRICS_REFRESH_TIMESTAMP_SECONDS,
        kind: MetricKind::Gauge,
        labels: &[],
        help: "Unix timestamp of the last successful slow-aggregate refresh. \
               time() - this = staleness.",
    },
    MetricDef {
        name: METRICS_REFRESH_FAILURES_TOTAL,
        kind: MetricKind::Counter,
        labels: &[],
        help: "Slow-aggregate refresh attempts that failed (gauges left stale).",
    },
];

/// Bound the `route` label to the axum route template. Templates like
/// `/dashboard/accounts/{account_id}` come from `MatchedPath` and are
/// inherently bounded by the route table; anything that did not match
/// a route (404 fallback) collapses into a single `unmatched` series
/// so attackers cannot mint time series by probing paths.
pub fn normalize_route(matched: Option<&str>) -> &str {
    matched.unwrap_or("unmatched")
}

/// Bound the `method` label to the standard HTTP method set. The
/// metrics middleware runs for every incoming request, including ones
/// with exotic methods, so unknown methods collapse into `other`.
pub fn normalize_method(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "GET",
        axum::http::Method::POST => "POST",
        axum::http::Method::PUT => "PUT",
        axum::http::Method::DELETE => "DELETE",
        axum::http::Method::PATCH => "PATCH",
        axum::http::Method::HEAD => "HEAD",
        axum::http::Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// Split a gRPC request path (`/package.Service/Method`) into bounded
/// `(service, method)` label values. Paths that do not have the
/// expected two-segment shape collapse into `("unknown", "unknown")`.
pub fn normalize_grpc_method(path: &str) -> (&str, &str) {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    match (parts.next(), parts.next()) {
        (Some(service), Some(method)) if !service.is_empty() && !method.is_empty() => {
            (service, method)
        }
        _ => ("unknown", "unknown"),
    }
}

/// Canonical lowercase label value for a numeric gRPC status code.
/// The set is closed (17 codes); anything else is `unknown`.
pub fn grpc_code_label(code: i32) -> &'static str {
    match code {
        0 => "ok",
        1 => "cancelled",
        2 => "unknown",
        3 => "invalid_argument",
        4 => "deadline_exceeded",
        5 => "not_found",
        6 => "already_exists",
        7 => "permission_denied",
        8 => "resource_exhausted",
        9 => "failed_precondition",
        10 => "aborted",
        11 => "out_of_range",
        12 => "unimplemented",
        13 => "internal",
        14 => "unavailable",
        15 => "data_loss",
        16 => "unauthenticated",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn normalize_route_passes_templates_through() {
        assert_eq!(
            normalize_route(Some("/dashboard/accounts/{account_id}/deltas/{nonce}")),
            "/dashboard/accounts/{account_id}/deltas/{nonce}"
        );
        assert_eq!(normalize_route(Some("/delta")), "/delta");
    }

    #[test]
    fn normalize_route_collapses_unmatched() {
        assert_eq!(normalize_route(None), "unmatched");
    }

    #[test]
    fn normalize_method_bounds_unknown_methods() {
        assert_eq!(normalize_method(&axum::http::Method::GET), "GET");
        assert_eq!(normalize_method(&axum::http::Method::POST), "POST");
        let exotic = axum::http::Method::from_bytes(b"PROPFIND").unwrap();
        assert_eq!(normalize_method(&exotic), "other");
    }

    #[test]
    fn normalize_grpc_method_splits_service_and_method() {
        assert_eq!(
            normalize_grpc_method("/guardian.Guardian/PushDelta"),
            ("guardian.Guardian", "PushDelta")
        );
    }

    #[test]
    fn normalize_grpc_method_collapses_unknown_shapes() {
        assert_eq!(normalize_grpc_method(""), ("unknown", "unknown"));
        assert_eq!(normalize_grpc_method("/"), ("unknown", "unknown"));
        assert_eq!(normalize_grpc_method("/no-method"), ("unknown", "unknown"));
        assert_eq!(normalize_grpc_method("/svc/"), ("unknown", "unknown"));
    }

    #[test]
    fn grpc_code_label_covers_closed_set() {
        assert_eq!(grpc_code_label(0), "ok");
        assert_eq!(grpc_code_label(16), "unauthenticated");
        assert_eq!(grpc_code_label(17), "unknown");
        assert_eq!(grpc_code_label(-1), "unknown");
    }

    /// Label allowlist enforcement: every declared metric follows the
    /// naming conventions and only carries allowlisted label keys.
    #[test]
    fn registry_enforces_naming_and_label_allowlist() {
        let allowlist: HashSet<&str> = LABEL_ALLOWLIST.iter().copied().collect();
        let mut seen = HashSet::new();

        for def in REGISTRY {
            assert!(
                def.name.starts_with("guardian_"),
                "{} must be namespaced under guardian_",
                def.name
            );
            assert!(
                def.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{} must be lower snake_case",
                def.name
            );
            assert!(seen.insert(def.name), "{} declared twice", def.name);
            assert!(!def.help.is_empty(), "{} needs help text", def.name);

            match def.kind {
                MetricKind::Counter => assert!(
                    def.name.ends_with("_total"),
                    "counter {} must end in _total",
                    def.name
                ),
                MetricKind::Histogram => assert!(
                    def.name.ends_with("_seconds"),
                    "histogram {} must use seconds as its base unit",
                    def.name
                ),
                MetricKind::Gauge => assert!(
                    !def.name.ends_with("_total"),
                    "gauge {} must not use the counter suffix",
                    def.name
                ),
            }

            for label in def.labels {
                assert!(
                    allowlist.contains(label),
                    "label `{}` on {} is not in LABEL_ALLOWLIST",
                    label,
                    def.name
                );
            }
        }
    }
}
