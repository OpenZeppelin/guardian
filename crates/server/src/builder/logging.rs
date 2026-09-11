use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Builds the JSON formatting layer shared by [`LoggingConfig::init`] and the
/// tests, so the two cannot drift apart.
fn json_layer<S, W>(writer: W) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(REQUEST_SPAN_EVENTS)
        .with_writer(writer)
}

/// Request spans emit a single line when they close, carrying the span's
/// recorded fields plus `time.busy` / `time.idle`. This is the only
/// per-request line at the default `info` filter, and it is emitted on the
/// error path too, so the centralized 5xx log in `error.rs` (which runs after
/// the span has closed) has an adjacent line carrying the account context.
const REQUEST_SPAN_EVENTS: FmtSpan = FmtSpan::CLOSE;

/// Output encoding used by the server tracing subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable output with ANSI styling when stdout is a terminal.
    Text,
    /// Newline-delimited JSON with flattened event fields and span context.
    Json,
    /// Compact single-line human-readable output.
    Compact,
}

impl LogFormat {
    /// Parses a case-insensitive format name, defaulting unknown values to text.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "json" => Self::Json,
            "compact" => Self::Compact,
            "text" => Self::Text,
            "" => Self::Text,
            other => {
                // eprintln: parse runs from LoggingConfig::default before the
                // subscriber is installed, so tracing::warn! would be dropped.
                eprintln!(
                    "GUARDIAN_LOG_FORMAT has unrecognized value {raw:?} (normalized {other:?}); \
                     falling back to text (accepted: text, json, compact)"
                );
                Self::Text
            }
        }
    }

    /// Resolves the format from `GUARDIAN_LOG_FORMAT`.
    pub fn from_env() -> Self {
        match std::env::var("GUARDIAN_LOG_FORMAT") {
            Ok(v) => Self::parse(&v),
            Err(_) => Self::Text,
        }
    }

    /// Returns the canonical lowercase format name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Compact => "compact",
        }
    }
}

impl std::fmt::Display for LogFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    level: Level,
    with_env_filter: bool,
    format: LogFormat,
}

impl LoggingConfig {
    /// Builds a config with the default [`LogFormat::Text`] output. The
    /// environment is not consulted here; use [`LoggingConfig::default`] (or
    /// an explicit [`LoggingConfig::with_format`]) to honour
    /// `GUARDIAN_LOG_FORMAT`.
    pub fn new(level: Level) -> Self {
        Self {
            level,
            with_env_filter: true,
            format: LogFormat::Text,
        }
    }

    pub fn with_env_filter(mut self, enabled: bool) -> Self {
        self.with_env_filter = enabled;
        self
    }

    /// Overrides the configured output format.
    pub fn with_format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    /// Returns the configured output format.
    pub fn format(&self) -> LogFormat {
        self.format
    }

    pub fn init(&self) {
        let filter = if self.with_env_filter {
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(self.level.as_str()))
        } else {
            EnvFilter::new(self.level.as_str())
        };

        let use_ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());

        match self.format {
            LogFormat::Json => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(json_layer(std::io::stdout))
                    .init();
            }
            LogFormat::Compact => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .compact()
                            .with_ansi(use_ansi)
                            .with_span_events(REQUEST_SPAN_EVENTS),
                    )
                    .init();
            }
            LogFormat::Text => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .with_ansi(use_ansi)
                            .with_span_events(REQUEST_SPAN_EVENTS),
                    )
                    .init();
            }
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self::new(Level::INFO).with_format(LogFormat::from_env())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct CapturedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl CapturedWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.bytes.lock().expect("capture lock poisoned").clone())
                .expect("captured logs should be UTF-8")
        }
    }

    impl std::io::Write for CapturedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes
                .lock()
                .expect("capture lock poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedWriter {
        type Writer = CapturedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tracing::instrument(level = "info", fields(account_id = "0xabc"))]
    fn request_probe() {
        tracing::debug!("request trace");
        tracing::warn!("request warning");
    }

    #[tracing::instrument(level = "info", fields(account_id = "0xabc"))]
    fn silent_request_probe() {}

    #[tracing::instrument(level = "debug", fields(account_id = "0xabc"))]
    fn nested_probe() {}

    #[tracing::instrument(level = "info", fields(account_id = "0xabc"))]
    fn failing_request_probe() -> Result<(), &'static str> {
        Err("storage unavailable")
    }

    fn capture_json(filter: &str, events: impl FnOnce()) -> String {
        let writer = CapturedWriter::default();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(filter))
            .with(json_layer(writer.clone()));
        let _registry_pin = crate::testing::log_capture::dispatcher_registry_pin();
        tracing::subscriber::with_default(subscriber, events);
        writer.contents()
    }

    fn capture_events(filter: &str, events: impl FnOnce()) -> Vec<serde_json::Value> {
        capture_json(filter, events)
            .lines()
            .map(|line| serde_json::from_str(line).expect("captured event should be JSON"))
            .collect()
    }

    #[test]
    fn parse_accepts_known_values_case_insensitive() {
        assert_eq!(LogFormat::parse("json"), LogFormat::Json);
        assert_eq!(LogFormat::parse("JSON"), LogFormat::Json);
        assert_eq!(LogFormat::parse("Json"), LogFormat::Json);
        assert_eq!(LogFormat::parse("compact"), LogFormat::Compact);
        assert_eq!(LogFormat::parse("COMPACT"), LogFormat::Compact);
        assert_eq!(LogFormat::parse("text"), LogFormat::Text);
        assert_eq!(LogFormat::parse("TEXT"), LogFormat::Text);
        assert_eq!(LogFormat::parse(""), LogFormat::Text);
        assert_eq!(LogFormat::parse("   "), LogFormat::Text);
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(LogFormat::parse("json "), LogFormat::Json);
        assert_eq!(LogFormat::parse(" json "), LogFormat::Json);
        assert_eq!(LogFormat::parse(" json\n"), LogFormat::Json);
        assert_eq!(LogFormat::parse("\tcompact\t"), LogFormat::Compact);
        assert_eq!(LogFormat::parse(" text "), LogFormat::Text);
        assert_eq!(LogFormat::parse("  JSON  "), LogFormat::Json);
    }

    #[test]
    fn parse_falls_back_to_text_for_unknown() {
        assert_eq!(LogFormat::parse("xml"), LogFormat::Text);
        assert_eq!(LogFormat::parse("pretty"), LogFormat::Text);
        assert_eq!(LogFormat::parse(" jsonx "), LogFormat::Text);
    }

    #[test]
    fn as_str_roundtrips() {
        assert_eq!(LogFormat::Json.as_str(), "json");
        assert_eq!(LogFormat::Compact.as_str(), "compact");
        assert_eq!(LogFormat::Text.as_str(), "text");
        assert_eq!(LogFormat::parse(LogFormat::Json.as_str()), LogFormat::Json);
        assert_eq!(
            LogFormat::parse(LogFormat::Compact.as_str()),
            LogFormat::Compact
        );
        assert_eq!(LogFormat::parse(LogFormat::Text.as_str()), LogFormat::Text);
    }

    #[test]
    fn new_ignores_the_environment() {
        assert_eq!(LoggingConfig::new(Level::INFO).format(), LogFormat::Text);
    }

    #[test]
    fn with_format_replaces_the_configured_format() {
        let cfg = LoggingConfig::new(Level::INFO).with_format(LogFormat::Json);
        assert_eq!(cfg.format(), LogFormat::Json);
        let cfg2 = cfg.with_format(LogFormat::Compact);
        assert_eq!(cfg2.format(), LogFormat::Compact);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(LogFormat::Json.to_string(), "json");
        assert_eq!(LogFormat::Text.to_string(), "text");
    }

    #[test]
    fn info_filter_preserves_request_context_without_request_trace() {
        let events = capture_events("info", request_probe);

        assert_eq!(events.len(), 2, "unexpected log output: {events:?}");
        assert_eq!(events[0]["level"], "WARN");
        assert_eq!(events[0]["message"], "request warning");
        assert_eq!(events[0]["span"]["account_id"], "0xabc");
        assert_eq!(events[0]["span"]["name"], "request_probe");
        assert_eq!(events[1]["message"], "close");
    }

    #[test]
    fn info_filter_emits_one_close_line_per_request_span() {
        let events = capture_events("info", silent_request_probe);

        assert_eq!(events.len(), 1, "unexpected log output: {events:?}");
        assert_eq!(events[0]["level"], "INFO");
        assert_eq!(events[0]["message"], "close");
        assert_eq!(events[0]["span"]["account_id"], "0xabc");
        assert_eq!(events[0]["span"]["name"], "silent_request_probe");
        assert!(
            events[0].get("time.busy").is_some(),
            "close line should carry span timing: {events:?}"
        );
    }

    /// The centralized 5xx log lives in `GuardianError`'s `IntoResponse` and
    /// `tonic::Status` conversions, which run after the service span has
    /// closed, so that line carries no account fields of its own. The
    /// span-close line is what supplies them, and it must still be emitted at
    /// the default `info` filter when the request failed.
    #[test]
    fn failed_request_emits_the_context_line_before_the_error_line() {
        let events = capture_events("info", || {
            if let Err(detail) = failing_request_probe() {
                tracing::error!(
                    code = "GUARDIAN_STORAGE_ERROR",
                    detail,
                    "guardian error (HTTP 5xx)"
                );
            }
        });

        assert_eq!(events.len(), 2, "unexpected log output: {events:?}");
        assert_eq!(events[0]["message"], "close");
        assert_eq!(events[0]["span"]["account_id"], "0xabc");
        assert_eq!(events[0]["span"]["name"], "failing_request_probe");
        assert_eq!(events[1]["level"], "ERROR");
        assert_eq!(events[1]["message"], "guardian error (HTTP 5xx)");
        assert!(
            events[1].get("span").is_none(),
            "the 5xx line itself carries no account context: {events:?}"
        );
    }

    #[test]
    fn info_filter_suppresses_close_lines_for_debug_spans() {
        let events = capture_events("info", nested_probe);

        assert!(events.is_empty(), "unexpected log output: {events:?}");
    }

    #[test]
    fn debug_filter_emits_request_trace_with_context() {
        let events = capture_events("debug", request_probe);

        assert_eq!(events.len(), 3, "unexpected log output: {events:?}");
        assert_eq!(events[0]["level"], "DEBUG");
        assert_eq!(events[0]["message"], "request trace");
        assert_eq!(events[0]["span"]["account_id"], "0xabc");
        assert_eq!(events[1]["level"], "WARN");
        assert_eq!(events[2]["message"], "close");
    }
}
