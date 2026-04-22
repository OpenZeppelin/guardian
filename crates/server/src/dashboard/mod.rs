use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, Duration, Utc};
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::Signature;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{GuardianError, Result};
use crate::middleware::RateLimitConfig;
use crate::middleware::rate_limit::{RateLimitStore, RateLimitType};
use crate::state::AppState;

const OPEN_DASHBOARD_DOMAIN: &str = "*";
const DEFAULT_CANONICAL_DOMAIN: &str = OPEN_DASHBOARD_DOMAIN;
const DEFAULT_COOKIE_NAME: &str = "guardian_operator_session";
const DEFAULT_NONCE_TTL_SECS: i64 = 300;
const DEFAULT_SESSION_TTL_SECS: i64 = 8 * 60 * 60;
const MAX_SESSION_TTL_SECS: i64 = 24 * 60 * 60;
const DEFAULT_MAX_OUTSTANDING_CHALLENGES: usize = 8;
const DEFAULT_PUBKEY_RATE_BURST_PER_SEC: u32 = 5;
const DEFAULT_PUBKEY_RATE_PER_MIN: u32 = 30;
const ENV_ALLOWLIST_JSON: &str = "GUARDIAN_OPERATOR_ALLOWLIST_JSON";
const ENV_CANONICAL_DOMAIN: &str = "GUARDIAN_DASHBOARD_DOMAIN";
const ENV_ALLOW_INSECURE_HTTP: &str = "GUARDIAN_DASHBOARD_ALLOW_INSECURE_HTTP";
const ENV_COOKIE_NAME: &str = "GUARDIAN_OPERATOR_SESSION_COOKIE_NAME";
const ENV_NONCE_TTL_SECS: &str = "GUARDIAN_OPERATOR_NONCE_TTL_SECS";
const ENV_SESSION_TTL_SECS: &str = "GUARDIAN_OPERATOR_SESSION_TTL_SECS";
const ENV_MAX_OUTSTANDING_CHALLENGES: &str = "GUARDIAN_OPERATOR_MAX_OUTSTANDING_CHALLENGES";
const ENV_PUBKEY_RATE_BURST_PER_SEC: &str = "GUARDIAN_OPERATOR_PUBKEY_RATE_BURST_PER_SEC";
const ENV_PUBKEY_RATE_PER_MIN: &str = "GUARDIAN_OPERATOR_PUBKEY_RATE_PER_MIN";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedOperator {
    pub operator_id: String,
    pub commitment: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorChallengePayload {
    pub domain: String,
    pub commitment: String,
    pub nonce: String,
    pub expires_at: String,
}

impl OperatorChallengePayload {
    pub fn signing_digest(&self) -> std::result::Result<Word, String> {
        AuthRequestPayload::from_json_serializable(self).map(|payload| payload.to_word())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorChallenge {
    pub payload: OperatorChallengePayload,
    pub signing_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedOperatorSession {
    pub operator: AuthenticatedOperator,
    pub expires_at: String,
    pub cookie_header: String,
}

#[derive(Clone, Debug)]
pub struct DashboardConfig {
    canonical_domain: String,
    allow_insecure_http: bool,
    cookie_name: String,
    nonce_ttl: Duration,
    session_ttl: Duration,
    max_outstanding_challenges: usize,
    commitment_rate_limit: RateLimitConfig,
}

impl DashboardConfig {
    pub fn from_env() -> std::result::Result<Self, String> {
        let canonical_domain = env::var(ENV_CANONICAL_DOMAIN)
            .unwrap_or_else(|_| DEFAULT_CANONICAL_DOMAIN.to_string())
            .trim()
            .to_string();
        let canonical_domain = if canonical_domain.is_empty() {
            OPEN_DASHBOARD_DOMAIN.to_string()
        } else {
            canonical_domain
        };

        let allow_insecure_http = env_flag(ENV_ALLOW_INSECURE_HTTP, false);
        if allow_insecure_http && !is_local_or_open_domain(&canonical_domain) {
            return Err(format!(
                "{ENV_ALLOW_INSECURE_HTTP}=true is only allowed for local or open dashboard domains"
            ));
        }

        let cookie_name = env::var(ENV_COOKIE_NAME)
            .unwrap_or_else(|_| DEFAULT_COOKIE_NAME.to_string())
            .trim()
            .to_string();
        if cookie_name.is_empty() {
            return Err(format!("{ENV_COOKIE_NAME} must not be empty"));
        }

        let nonce_ttl_secs = env_i64(ENV_NONCE_TTL_SECS, DEFAULT_NONCE_TTL_SECS)?;
        if nonce_ttl_secs <= 0 {
            return Err(format!("{ENV_NONCE_TTL_SECS} must be greater than zero"));
        }

        let session_ttl_secs = env_i64(ENV_SESSION_TTL_SECS, DEFAULT_SESSION_TTL_SECS)?;
        if session_ttl_secs <= 0 {
            return Err(format!("{ENV_SESSION_TTL_SECS} must be greater than zero"));
        }
        if session_ttl_secs > MAX_SESSION_TTL_SECS {
            return Err(format!(
                "{ENV_SESSION_TTL_SECS} must be <= {MAX_SESSION_TTL_SECS}"
            ));
        }

        let max_outstanding_challenges = env_usize(
            ENV_MAX_OUTSTANDING_CHALLENGES,
            DEFAULT_MAX_OUTSTANDING_CHALLENGES,
        )?;
        if max_outstanding_challenges == 0 {
            return Err(format!(
                "{ENV_MAX_OUTSTANDING_CHALLENGES} must be greater than zero"
            ));
        }

        let commitment_rate_limit = RateLimitConfig {
            enabled: true,
            burst_per_sec: env_u32(
                ENV_PUBKEY_RATE_BURST_PER_SEC,
                DEFAULT_PUBKEY_RATE_BURST_PER_SEC,
            )?,
            per_min: env_u32(ENV_PUBKEY_RATE_PER_MIN, DEFAULT_PUBKEY_RATE_PER_MIN)?,
        };

        Ok(Self {
            canonical_domain,
            allow_insecure_http,
            cookie_name,
            nonce_ttl: Duration::seconds(nonce_ttl_secs),
            session_ttl: Duration::seconds(session_ttl_secs),
            max_outstanding_challenges,
            commitment_rate_limit,
        })
    }

    pub fn for_tests() -> Self {
        Self {
            canonical_domain: DEFAULT_CANONICAL_DOMAIN.to_string(),
            allow_insecure_http: false,
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

impl Default for DashboardConfig {
    fn default() -> Self {
        Self::for_tests()
    }
}

#[derive(Clone, Debug)]
pub struct DashboardState {
    config: DashboardConfig,
    allowlist: Arc<OperatorAllowlist>,
    challenges: Arc<Mutex<HashMap<String, Vec<PendingChallenge>>>>,
    sessions: Arc<Mutex<HashMap<String, OperatorSessionRecord>>>,
    commitment_rate_limits: RateLimitStore,
}

impl DashboardState {
    pub fn from_env() -> std::result::Result<Self, String> {
        let config = DashboardConfig::from_env()?;
        let allowlist_json = env::var(ENV_ALLOWLIST_JSON).unwrap_or_else(|_| "[]".to_string());
        let entries: Vec<OperatorAllowlistEntryInput> = serde_json::from_str(&allowlist_json)
            .map_err(|error| format!("Failed to parse {ENV_ALLOWLIST_JSON}: {error}"))?;
        Self::from_entries(entries, config)
    }

    pub fn for_tests(entries: Vec<(String, String)>) -> Self {
        let inputs = entries
            .into_iter()
            .map(|(operator_id, commitment)| OperatorAllowlistEntryInput {
                operator_id,
                commitment,
            })
            .collect();
        Self::from_entries(inputs, DashboardConfig::for_tests())
            .expect("dashboard test configuration should be valid")
    }

    pub fn cookie_name(&self) -> &str {
        &self.config.cookie_name
    }

    pub fn clear_cookie_header(&self) -> String {
        let expires = "Thu, 01 Jan 1970 00:00:00 GMT";
        let secure = if self.config.allow_insecure_http {
            ""
        } else {
            "; Secure"
        };

        format!(
            "{}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0; Expires={}{}",
            self.config.cookie_name, expires, secure
        )
    }

    pub async fn issue_challenge(
        &self,
        commitment: &str,
        now: DateTime<Utc>,
    ) -> Result<OperatorChallenge> {
        self.rate_limit_commitment("challenge", commitment)?;

        let correlation_id = correlation_id();
        let normalized_commitment =
            normalize_commitment(commitment).unwrap_or_else(|_| commitment.to_string());
        let payload = OperatorChallengePayload {
            domain: self.config.canonical_domain.clone(),
            commitment: normalized_commitment.clone(),
            nonce: random_hex::<32>(),
            expires_at: (now + self.config.nonce_ttl).to_rfc3339(),
        };
        let signing_digest = payload.signing_digest().map_err(|error| {
            GuardianError::ConfigurationError(format!("Failed to create challenge digest: {error}"))
        })?;

        if self.allowlist.lookup(&normalized_commitment).is_some() {
            let expires_at = now + self.config.nonce_ttl;
            let mut challenges = self.challenges.lock().await;
            let pending = challenges.entry(normalized_commitment.clone()).or_default();
            pending.retain(|challenge| challenge.expires_at > now);
            pending.push(PendingChallenge {
                signing_digest,
                issued_at: now,
                expires_at,
            });
            if pending.len() > self.config.max_outstanding_challenges {
                pending.sort_by_key(|challenge| challenge.issued_at);
                let drain_len = pending.len() - self.config.max_outstanding_challenges;
                pending.drain(0..drain_len);
            }

            tracing::info!(
                auth_event = "challenge_issued",
                correlation_id = %correlation_id,
                commitment = %normalized_commitment,
                "Operator challenge issued"
            );
        } else {
            tracing::info!(
                auth_event = "challenge_issued_decoy",
                correlation_id = %correlation_id,
                commitment = %normalized_commitment,
                "Operator challenge issued without allowlist match"
            );
        }

        Ok(OperatorChallenge {
            payload,
            signing_digest: signing_digest.into_hex(),
        })
    }

    pub async fn verify(
        &self,
        commitment: &str,
        signature_hex: &str,
        now: DateTime<Utc>,
    ) -> Result<IssuedOperatorSession> {
        self.rate_limit_commitment("verify", commitment)?;

        let correlation_id = correlation_id();
        let normalized_commitment = normalize_commitment(commitment).map_err(|_| {
            tracing::warn!(
                auth_event = "verify_failed",
                correlation_id = %correlation_id,
                "Operator verify rejected because the commitment was invalid"
            );
            GuardianError::AuthenticationFailed("Invalid operator credentials".to_string())
        })?;
        let operator = self
            .allowlist
            .lookup(&normalized_commitment)
            .cloned()
            .ok_or_else(|| {
                tracing::warn!(
                    auth_event = "verify_failed",
                    correlation_id = %correlation_id,
                    commitment = %normalized_commitment,
                    "Operator verify rejected because the commitment is not allowlisted"
                );
                GuardianError::AuthenticationFailed("Invalid operator credentials".to_string())
            })?;

        let signature = Signature::from_hex(signature_hex).map_err(|_| {
            tracing::warn!(
                auth_event = "verify_failed",
                correlation_id = %correlation_id,
                operator_id = %operator.operator_id,
                "Operator verify rejected because the signature was malformed"
            );
            GuardianError::AuthenticationFailed("Invalid operator credentials".to_string())
        })?;
        let public_key = signature.public_key();
        let signature_commitment = public_key.to_commitment().into_hex();
        if signature_commitment != normalized_commitment {
            tracing::warn!(
                auth_event = "verify_failed",
                correlation_id = %correlation_id,
                operator_id = %operator.operator_id,
                "Operator verify rejected because the signature commitment did not match the requested commitment"
            );
            return Err(GuardianError::AuthenticationFailed(
                "Invalid operator credentials".to_string(),
            ));
        }

        let mut challenges = self.challenges.lock().await;
        let pending = challenges.entry(normalized_commitment.clone()).or_default();
        pending.retain(|challenge| challenge.expires_at > now);

        let matched_index = pending
            .iter()
            .position(|challenge| public_key.verify(challenge.signing_digest, &signature));

        let Some(matched_index) = matched_index else {
            if pending.is_empty() {
                challenges.remove(&normalized_commitment);
            }
            tracing::warn!(
                auth_event = "verify_failed",
                correlation_id = %correlation_id,
                operator_id = %operator.operator_id,
                "Operator verify rejected because no active challenge matched the signature"
            );
            return Err(GuardianError::AuthenticationFailed(
                "Invalid operator credentials".to_string(),
            ));
        };

        pending.remove(matched_index);
        if pending.is_empty() {
            challenges.remove(&normalized_commitment);
        }
        drop(challenges);

        let issued_at = now;
        let expires_at = now + self.config.session_ttl;
        let operator_identity = AuthenticatedOperator {
            operator_id: operator.operator_id.clone(),
            commitment: operator.commitment.clone(),
        };
        let token = random_hex::<32>();
        let cookie_header = self.session_cookie_header(&token, issued_at, expires_at);

        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(
            token,
            OperatorSessionRecord {
                operator: operator_identity.clone(),
                issued_at,
                expires_at,
            },
        );

        tracing::info!(
            auth_event = "verify_success",
            correlation_id = %correlation_id,
            operator_id = %operator.operator_id,
            "Operator session created"
        );

        Ok(IssuedOperatorSession {
            operator: operator_identity,
            expires_at: expires_at.to_rfc3339(),
            cookie_header,
        })
    }

    pub async fn authenticate_session(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<AuthenticatedOperator> {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);

        let session = sessions.get(token).cloned().ok_or_else(|| {
            tracing::warn!(
                auth_event = "session_rejected",
                reason = "missing_or_expired",
                "Operator session rejected"
            );
            GuardianError::AuthenticationFailed("Invalid operator session".to_string())
        })?;

        if self
            .allowlist
            .lookup(&session.operator.commitment)
            .is_none()
        {
            sessions.remove(token);
            tracing::warn!(
                auth_event = "session_rejected",
                operator_id = %session.operator.operator_id,
                reason = "revoked",
                "Operator session rejected because the operator is no longer allowlisted"
            );
            return Err(GuardianError::AuthenticationFailed(
                "Invalid operator session".to_string(),
            ));
        }

        Ok(session.operator)
    }

    pub async fn logout(&self, token: Option<&str>, now: DateTime<Utc>) {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        if let Some(token) = token {
            if let Some(session) = sessions.remove(token) {
                tracing::info!(
                    auth_event = "logout",
                    operator_id = %session.operator.operator_id,
                    issued_at = %session.issued_at.to_rfc3339(),
                    "Operator session cleared"
                );
            }
        }
    }

    fn from_entries(
        entries: Vec<OperatorAllowlistEntryInput>,
        config: DashboardConfig,
    ) -> std::result::Result<Self, String> {
        let allowlist = OperatorAllowlist::from_entries(entries)?;
        tracing::info!(
            auth_event = "allowlist_loaded",
            operator_count = allowlist.len(),
            "Operator allowlist loaded"
        );
        Ok(Self {
            commitment_rate_limits: RateLimitStore::new(config.commitment_rate_limit.clone()),
            config,
            allowlist: Arc::new(allowlist),
            challenges: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn rate_limit_commitment(&self, endpoint: &str, commitment: &str) -> Result<()> {
        if !self.config.commitment_rate_limit.enabled {
            return Ok(());
        }

        let key = format!("endpoint:{endpoint}|commitment:{commitment}");
        if let Err(limit_type) = self.commitment_rate_limits.check_burst(&key) {
            return Err(rate_limit_error(limit_type));
        }
        if let Err(limit_type) = self.commitment_rate_limits.check_sustained(&key) {
            return Err(rate_limit_error(limit_type));
        }
        Ok(())
    }

    fn session_cookie_header(
        &self,
        token: &str,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> String {
        let max_age = (expires_at - issued_at).num_seconds().max(0);
        let secure = if self.config.allow_insecure_http {
            ""
        } else {
            "; Secure"
        };

        format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}; Expires={}{}",
            self.config.cookie_name,
            token,
            max_age,
            cookie_date(expires_at),
            secure
        )
    }
}

impl Default for DashboardState {
    fn default() -> Self {
        Self::for_tests(Vec::new())
    }
}

pub async fn require_dashboard_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let token =
        extract_cookie(request.headers(), state.dashboard.cookie_name()).ok_or_else(|| {
            GuardianError::AuthenticationFailed("Invalid operator session".to_string())
        })?;
    let operator = state
        .dashboard
        .authenticate_session(&token, state.clock.now())
        .await?;
    request.extensions_mut().insert(operator);
    Ok(next.run(request).await)
}

pub fn extract_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let raw_cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    raw_cookie.split(';').find_map(|item| {
        let (name, value) = item.trim().split_once('=')?;
        (name == cookie_name).then(|| value.to_string())
    })
}

#[derive(Clone, Debug)]
struct OperatorAllowlist {
    by_commitment: HashMap<String, AuthenticatedOperator>,
}

impl OperatorAllowlist {
    fn from_entries(
        entries: Vec<OperatorAllowlistEntryInput>,
    ) -> std::result::Result<Self, String> {
        let mut by_commitment = HashMap::with_capacity(entries.len());
        let mut operator_ids = HashSet::with_capacity(entries.len());
        let mut commitments = HashSet::with_capacity(entries.len());

        for entry in entries {
            if entry.operator_id.trim().is_empty() {
                return Err(
                    "Operator allowlist entries must have a non-empty operator_id".to_string(),
                );
            }

            let normalized_commitment = normalize_commitment(&entry.commitment)?;
            if !operator_ids.insert(entry.operator_id.clone()) {
                return Err(format!(
                    "Duplicate operator identifier in allowlist: {}",
                    entry.operator_id
                ));
            }
            if !commitments.insert(normalized_commitment.clone()) {
                return Err(format!(
                    "Duplicate operator commitment in allowlist: {}",
                    normalized_commitment
                ));
            }

            by_commitment.insert(
                normalized_commitment.clone(),
                AuthenticatedOperator {
                    operator_id: entry.operator_id,
                    commitment: normalized_commitment,
                },
            );
        }

        Ok(Self { by_commitment })
    }

    fn lookup(&self, commitment: &str) -> Option<&AuthenticatedOperator> {
        self.by_commitment.get(commitment)
    }

    fn len(&self) -> usize {
        self.by_commitment.len()
    }
}

#[derive(Clone, Debug)]
struct PendingChallenge {
    signing_digest: Word,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
struct OperatorSessionRecord {
    operator: AuthenticatedOperator,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize)]
struct OperatorAllowlistEntryInput {
    operator_id: String,
    commitment: String,
}

fn normalize_commitment(commitment: &str) -> std::result::Result<String, String> {
    Word::from_hex(commitment).map(|parsed| parsed.into_hex())
}

fn random_hex<const N: usize>() -> String {
    let bytes: [u8; N] = rand::random();
    hex::encode(bytes)
}

fn correlation_id() -> String {
    random_hex::<8>()
}

fn cookie_date(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn rate_limit_error(limit_type: RateLimitType) -> GuardianError {
    let retry_after_secs = match limit_type {
        RateLimitType::Burst => 1,
        RateLimitType::Sustained => 60,
    };
    GuardianError::RateLimitExceeded {
        retry_after_secs,
        scope: "operator_commitment".to_string(),
    }
}

fn env_flag(key: &str, default_value: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default_value)
}

fn env_i64(key: &str, default_value: i64) -> std::result::Result<i64, String> {
    env::var(key)
        .ok()
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|error| format!("Failed to parse {key}: {error}"))
        })
        .unwrap_or(Ok(default_value))
}

fn env_u32(key: &str, default_value: u32) -> std::result::Result<u32, String> {
    env::var(key)
        .ok()
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|error| format!("Failed to parse {key}: {error}"))
        })
        .unwrap_or(Ok(default_value))
}

fn env_usize(key: &str, default_value: usize) -> std::result::Result<usize, String> {
    env::var(key)
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| format!("Failed to parse {key}: {error}"))
        })
        .unwrap_or(Ok(default_value))
}

fn is_local_domain(domain: &str) -> bool {
    let lower = domain.trim().to_ascii_lowercase();
    lower == "localhost"
        || lower.starts_with("localhost:")
        || lower == "127.0.0.1"
        || lower.starts_with("127.0.0.1:")
        || lower.ends_with(".local")
}

fn is_open_domain(domain: &str) -> bool {
    domain.trim() == OPEN_DASHBOARD_DOMAIN
}

fn is_local_or_open_domain(domain: &str) -> bool {
    is_local_domain(domain) || is_open_domain(domain)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CANONICAL_DOMAIN, DashboardConfig, OPEN_DASHBOARD_DOMAIN, is_local_domain,
        is_local_or_open_domain, is_open_domain,
    };

    #[test]
    fn dashboard_config_defaults_to_open_domain() {
        let config = DashboardConfig::default();

        assert_eq!(config.canonical_domain, DEFAULT_CANONICAL_DOMAIN);
        assert_eq!(config.canonical_domain, OPEN_DASHBOARD_DOMAIN);
    }

    #[test]
    fn wildcard_domain_is_treated_as_open() {
        assert!(is_open_domain("*"));
        assert!(is_local_or_open_domain("*"));
        assert!(!is_local_domain("*"));
    }
}
