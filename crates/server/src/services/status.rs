//! Public, unauthenticated server status. Assembled entirely from
//! in-process build/network/startup data — no storage I/O, no account,
//! operator, or auth data is exposed.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Response body for `GET /status`. Safe to expose unauthenticated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct StatusResponse {
    /// Constant `"ok"`. A 200 response is itself the liveness signal.
    pub status: &'static str,
    /// `CARGO_PKG_VERSION` of `guardian-server`.
    pub version: &'static str,
    /// Short git SHA at build time (`"unknown"` when unavailable).
    pub git_commit: &'static str,
    /// Deployment environment / network label, e.g. `"testnet"`. Same
    /// value surfaced on `GET /dashboard/info`.
    pub environment: String,
    /// RFC 3339 wall-clock time the process started.
    pub started_at: String,
    /// Whole seconds since `started_at`; clamped to 0 on clock skew.
    pub uptime_seconds: u64,
}

/// Assemble the status response. Pure so it is unit-testable without an
/// `AppState`: the handler supplies `environment`, `started_at`, and
/// `now`.
pub fn build_status(
    environment: &str,
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> StatusResponse {
    let uptime_seconds = (now - started_at).num_seconds().max(0) as u64;
    StatusResponse {
        status: "ok",
        version: crate::build_info::VERSION,
        git_commit: crate::build_info::GIT_SHA,
        environment: environment.to_string(),
        started_at: started_at.to_rfc3339(),
        uptime_seconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn computes_uptime_and_carries_fields() {
        let started = at("2026-06-17T10:00:00Z");
        let now = at("2026-06-17T11:00:00Z");
        let resp = build_status("devnet", started, now);

        assert_eq!(resp.status, "ok");
        assert_eq!(resp.version, crate::build_info::VERSION);
        assert_eq!(resp.git_commit, crate::build_info::GIT_SHA);
        assert_eq!(resp.environment, "devnet");
        assert_eq!(resp.started_at, started.to_rfc3339());
        assert_eq!(resp.uptime_seconds, 3600);
    }

    #[test]
    fn negative_uptime_is_clamped_to_zero() {
        let started = at("2026-06-17T11:00:00Z");
        let now = at("2026-06-17T10:00:00Z");
        let resp = build_status("local", started, now);
        assert_eq!(resp.uptime_seconds, 0);
    }

    #[test]
    fn payload_has_no_sensitive_fields() {
        let resp = build_status(
            "devnet",
            at("2026-06-17T10:00:00Z"),
            at("2026-06-17T10:00:01Z"),
        );
        let json = serde_json::to_value(&resp).unwrap();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "environment",
                "git_commit",
                "started_at",
                "status",
                "uptime_seconds",
                "version"
            ]
        );
    }
}
