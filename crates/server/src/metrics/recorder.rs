//! Prometheus recorder construction.
//!
//! The recorder is the in-memory registry the `metrics` macros write
//! into; the [`PrometheusHandle`] renders its contents as text
//! exposition on scrape. Production installs one global recorder in
//! `ServerHandle::run()`. Tests must never install globally — use
//! `metrics::with_local_recorder(&build_recorder(), || ...)` instead,
//! because the global recorder is process-wide and set-once.

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusRecorder};

use super::names::{self, MetricKind, REGISTRY};
use crate::build_info;

/// Suffix shared by every latency histogram in the taxonomy; one
/// bucket rule covers them all (and any future `*_duration_seconds`).
const DURATION_SECONDS_SUFFIX: &str = "duration_seconds";

/// Explicit buckets from 1ms to 10s. Without explicit buckets the
/// exporter falls back to quantile summaries, which cannot be
/// aggregated across instances — histograms are required for the
/// multi-replica use-case in the feature spec.
const DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Build an uninstalled recorder. The caller decides whether to
/// install it globally (production) or scope it locally (tests).
pub fn build_recorder() -> PrometheusRecorder {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Suffix(DURATION_SECONDS_SUFFIX.to_string()),
            DURATION_BUCKETS,
        )
        .expect("static duration buckets are non-empty")
        .build_recorder()
}

/// Attach help text to every metric in the taxonomy. Must run while
/// the target recorder is installed (globally or via
/// `with_local_recorder`), since the describe macros write to the
/// active recorder.
pub fn describe_metrics() {
    for def in REGISTRY {
        match def.kind {
            MetricKind::Counter => metrics::describe_counter!(def.name, def.help),
            MetricKind::Gauge => metrics::describe_gauge!(def.name, def.help),
            MetricKind::Histogram => metrics::describe_histogram!(def.name, def.help),
        }
    }
}

/// Emit the constant `guardian_build_info{version,git_commit,profile} 1`
/// gauge so dashboards can correlate metric changes with deploys.
pub fn record_build_info() {
    metrics::gauge!(
        names::BUILD_INFO,
        names::LABEL_VERSION => build_info::VERSION,
        names::LABEL_GIT_COMMIT => build_info::GIT_SHA,
        names::LABEL_PROFILE => build_info::build_profile(),
    )
    .set(1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_renders_constant_gauge_with_identity_labels() {
        let recorder = build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, || {
            describe_metrics();
            record_build_info();
        });

        let rendered = handle.render();
        assert!(rendered.contains("guardian_build_info{"));
        assert!(rendered.contains(&format!("version=\"{}\"", build_info::VERSION)));
        assert!(rendered.contains("} 1"));
    }

    #[test]
    fn duration_histograms_render_buckets_not_summaries() {
        let recorder = build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, || {
            metrics::histogram!(names::HTTP_REQUEST_DURATION_SECONDS,
                names::LABEL_METHOD => "GET", names::LABEL_ROUTE => "/pubkey")
            .record(0.003);
        });

        let rendered = handle.render();
        assert!(
            rendered.contains("guardian_http_request_duration_seconds_bucket"),
            "expected histogram buckets in:\n{rendered}"
        );
        assert!(rendered.contains("le=\"0.005\""));
        assert!(rendered.contains("le=\"+Inf\""));
    }
}
