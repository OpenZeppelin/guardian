use tracing::Level;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

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
                // eprintln: parse runs from LoggingConfig::new before the
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
    pub fn new(level: Level) -> Self {
        Self {
            level,
            with_env_filter: true,
            format: LogFormat::from_env(),
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

        let use_ansi = match self.format {
            LogFormat::Json => false,
            _ => std::io::IsTerminal::is_terminal(&std::io::stdout()),
        };

        match self.format {
            LogFormat::Json => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .flatten_event(true)
                            .with_current_span(true)
                            .with_span_list(true),
                    )
                    .init();
            }
            LogFormat::Compact => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .compact()
                            .with_ansi(use_ansi),
                    )
                    .init();
            }
            LogFormat::Text => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(tracing_subscriber::fmt::layer().with_ansi(use_ansi))
                    .init();
            }
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self::new(Level::INFO)
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

    fn capture_json(filter: &str, events: impl FnOnce()) -> String {
        let writer = CapturedWriter::default();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(filter))
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_writer(writer.clone()),
            );
        let _registry_pin = crate::testing::log_capture::dispatcher_registry_pin();
        tracing::subscriber::with_default(subscriber, events);
        writer.contents()
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
        assert_eq!(
            LogFormat::parse("json " /* trimmed -> json */),
            LogFormat::Json
        );
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
    fn with_format_overrides_env() {
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
        let captured = capture_json("info", request_probe);
        let lines: Vec<&str> = captured.lines().collect();

        assert_eq!(lines.len(), 1, "unexpected log output: {captured}");
        let event: serde_json::Value =
            serde_json::from_str(lines[0]).expect("captured event should be JSON");
        assert_eq!(event["level"], "WARN");
        assert_eq!(event["message"], "request warning");
        assert_eq!(event["span"]["account_id"], "0xabc");
        assert_eq!(event["span"]["name"], "request_probe");
    }

    #[test]
    fn debug_filter_emits_request_trace_with_context() {
        let captured = capture_json("debug", request_probe);
        let events: Vec<serde_json::Value> = captured
            .lines()
            .map(|line| serde_json::from_str(line).expect("captured event should be JSON"))
            .collect();

        assert_eq!(events.len(), 2, "unexpected log output: {captured}");
        assert_eq!(events[0]["level"], "DEBUG");
        assert_eq!(events[0]["message"], "request trace");
        assert_eq!(events[0]["span"]["account_id"], "0xabc");
        assert_eq!(events[1]["level"], "WARN");
    }
}
