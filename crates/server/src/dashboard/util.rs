use chrono::{DateTime, Utc};

use crate::error::GuardianError;
use crate::middleware::rate_limit::RateLimitType;

pub(crate) fn random_hex<const N: usize>() -> String {
    let bytes: [u8; N] = rand::random();
    hex::encode(bytes)
}

pub(crate) fn correlation_id() -> String {
    random_hex::<8>()
}

pub(crate) fn cookie_date(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub(crate) fn rate_limit_error(limit_type: RateLimitType) -> GuardianError {
    let retry_after_secs = match limit_type {
        RateLimitType::Burst => 1,
        RateLimitType::Sustained => 60,
    };
    GuardianError::RateLimitExceeded {
        retry_after_secs,
        scope: "operator_commitment".to_string(),
    }
}
