use chrono::Duration;

use crate::middleware::RateLimitConfig;

pub(crate) const OPEN_DASHBOARD_DOMAIN: &str = "*";
pub(crate) const DEFAULT_CANONICAL_DOMAIN: &str = OPEN_DASHBOARD_DOMAIN;
pub(crate) const DEFAULT_COOKIE_NAME: &str = "guardian_operator_session";
pub(crate) const DEFAULT_NONCE_TTL_SECS: i64 = 300;
pub(crate) const DEFAULT_SESSION_TTL_SECS: i64 = 8 * 60 * 60;
pub(crate) const DEFAULT_MAX_OUTSTANDING_CHALLENGES: usize = 8;
pub(crate) const DEFAULT_PUBKEY_RATE_BURST_PER_SEC: u32 = 5;
pub(crate) const DEFAULT_PUBKEY_RATE_PER_MIN: u32 = 30;

#[derive(Clone, Debug)]
pub struct DashboardConfig {
    pub(crate) canonical_domain: String,
    pub(crate) cookie_name: String,
    pub(crate) nonce_ttl: Duration,
    pub(crate) session_ttl: Duration,
    pub(crate) max_outstanding_challenges: usize,
    pub(crate) commitment_rate_limit: RateLimitConfig,
}

impl DashboardConfig {
    pub fn from_env() -> std::result::Result<Self, String> {
        Ok(Self::default())
    }

    pub fn for_tests() -> Self {
        Self::default()
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            canonical_domain: DEFAULT_CANONICAL_DOMAIN.to_string(),
            cookie_name: DEFAULT_COOKIE_NAME.to_string(),
            nonce_ttl: Duration::seconds(DEFAULT_NONCE_TTL_SECS),
            session_ttl: Duration::seconds(DEFAULT_SESSION_TTL_SECS),
            max_outstanding_challenges: DEFAULT_MAX_OUTSTANDING_CHALLENGES,
            commitment_rate_limit: RateLimitConfig {
                enabled: true,
                burst_per_sec: DEFAULT_PUBKEY_RATE_BURST_PER_SEC,
                per_min: DEFAULT_PUBKEY_RATE_PER_MIN,
            },
        }
    }
}
