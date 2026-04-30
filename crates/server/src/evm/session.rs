use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use tokio::sync::Mutex;

use crate::error::{GuardianError, Result};
use crate::metadata::network::normalize_evm_address;

const COOKIE_NAME: &str = "guardian_evm_session";
const COOKIE_DOMAIN_ENV: &str = "GUARDIAN_EVM_SESSION_COOKIE_DOMAIN";
const COOKIE_SAME_SITE_ENV: &str = "GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE";
const COOKIE_SECURE_ENV: &str = "GUARDIAN_EVM_SESSION_COOKIE_SECURE";
const CHALLENGE_TTL_SECS: i64 = 300;
const SESSION_TTL_SECS: i64 = 8 * 60 * 60;
const MAX_OUTSTANDING_CHALLENGES: usize = 8;

#[derive(Clone)]
pub struct EvmSessionState {
    challenges: Arc<Mutex<HashMap<String, Vec<PendingEvmChallenge>>>>,
    sessions: Arc<Mutex<HashMap<String, EvmSessionRecord>>>,
    cookie_config: EvmSessionCookieConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmChallenge {
    pub address: String,
    pub nonce: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEvmSession {
    pub address: String,
    pub expires_at: DateTime<Utc>,
    pub cookie_header: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedEvmSession {
    pub address: String,
}

#[derive(Clone)]
struct PendingEvmChallenge {
    challenge: EvmChallenge,
}

#[derive(Clone)]
struct EvmSessionRecord {
    address: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmSessionCookieConfig {
    domain: Option<String>,
    same_site: SameSite,
    secure: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SameSite {
    Strict,
    Lax,
    None,
}

impl Default for EvmSessionState {
    fn default() -> Self {
        Self {
            challenges: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            cookie_config: EvmSessionCookieConfig::default(),
        }
    }
}

impl EvmSessionState {
    pub fn from_env() -> std::result::Result<Self, String> {
        Ok(Self {
            cookie_config: EvmSessionCookieConfig::from_env()?,
            ..Self::default()
        })
    }

    pub fn cookie_name(&self) -> &'static str {
        COOKIE_NAME
    }

    pub fn clear_cookie_header(&self) -> String {
        let expires = Utc::now() - Duration::days(1);
        format!(
            "{COOKIE_NAME}=; {}; Max-Age=0; Expires={}",
            self.cookie_config.attributes(),
            cookie_date(expires)
        )
    }

    pub async fn issue_challenge(&self, address: &str, now: DateTime<Utc>) -> Result<EvmChallenge> {
        let address = normalize_evm_address(address).map_err(GuardianError::InvalidInput)?;
        let challenge = EvmChallenge {
            address: address.clone(),
            nonce: random_hex_32(),
            issued_at: now,
            expires_at: now + Duration::seconds(CHALLENGE_TTL_SECS),
        };

        let mut challenges = self.challenges.lock().await;
        let pending = challenges.entry(address).or_default();
        pending.retain(|challenge| challenge.challenge.expires_at > now);
        pending.push(PendingEvmChallenge {
            challenge: challenge.clone(),
        });
        if pending.len() > MAX_OUTSTANDING_CHALLENGES {
            let drain_len = pending.len() - MAX_OUTSTANDING_CHALLENGES;
            pending.drain(0..drain_len);
        }

        Ok(challenge)
    }

    pub async fn verify(
        &self,
        address: &str,
        nonce: &str,
        signature: &str,
        now: DateTime<Utc>,
    ) -> Result<VerifiedEvmSession> {
        let address = normalize_evm_address(address).map_err(GuardianError::InvalidInput)?;
        let signature = crate::evm::proposal::normalize_signature(signature)?;
        let mut challenges = self.challenges.lock().await;
        let pending = challenges.entry(address.clone()).or_default();
        pending.retain(|challenge| challenge.challenge.expires_at > now);

        let Some(index) = pending
            .iter()
            .position(|pending| pending.challenge.nonce.eq_ignore_ascii_case(nonce))
        else {
            return Err(GuardianError::AuthenticationFailed(
                "No active EVM challenge matched the nonce".to_string(),
            ));
        };

        let challenge = pending[index].challenge.clone();
        let recovered = crate::evm::contracts::recover_session_address(&challenge, &signature)?;
        if recovered != address {
            return Err(GuardianError::AuthenticationFailed(
                "EVM session signature does not match requested address".to_string(),
            ));
        }

        pending.remove(index);
        if pending.is_empty() {
            challenges.remove(&address);
        }
        drop(challenges);

        let token = random_hex_32();
        let expires_at = now + Duration::seconds(SESSION_TTL_SECS);
        let cookie_header = self.session_cookie_header(&token, expires_at);
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(
            token,
            EvmSessionRecord {
                address: address.clone(),
                expires_at,
            },
        );

        Ok(VerifiedEvmSession {
            address,
            expires_at,
            cookie_header,
        })
    }

    pub async fn authenticate(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<AuthenticatedEvmSession> {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        let session = sessions.get(token).cloned().ok_or_else(|| {
            GuardianError::AuthenticationFailed("Invalid EVM session".to_string())
        })?;
        Ok(AuthenticatedEvmSession {
            address: session.address,
        })
    }

    pub async fn logout(&self, token: Option<&str>, now: DateTime<Utc>) {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        if let Some(token) = token {
            sessions.remove(token);
        }
    }

    fn session_cookie_header(&self, token: &str, expires_at: DateTime<Utc>) -> String {
        format!(
            "{COOKIE_NAME}={token}; {}; Max-Age={SESSION_TTL_SECS}; Expires={}",
            self.cookie_config.attributes(),
            cookie_date(expires_at)
        )
    }
}

impl EvmSessionCookieConfig {
    pub fn from_env() -> std::result::Result<Self, String> {
        let domain = match std::env::var(COOKIE_DOMAIN_ENV) {
            Ok(value) => Some(parse_cookie_attribute_value(COOKIE_DOMAIN_ENV, &value)?),
            Err(_) => None,
        };
        let same_site = match std::env::var(COOKIE_SAME_SITE_ENV) {
            Ok(value) => SameSite::parse(&value)?,
            Err(_) => SameSite::Strict,
        };
        let secure = match std::env::var(COOKIE_SECURE_ENV) {
            Ok(value) => parse_bool(COOKIE_SECURE_ENV, &value)?,
            Err(_) => false,
        };

        Self::new(domain, same_site, secure)
    }

    fn new(
        domain: Option<String>,
        same_site: SameSite,
        secure: bool,
    ) -> std::result::Result<Self, String> {
        if matches!(same_site, SameSite::None) && !secure {
            return Err(format!(
                "{COOKIE_SAME_SITE_ENV}=None requires {COOKIE_SECURE_ENV}=true"
            ));
        }

        Ok(Self {
            domain,
            same_site,
            secure,
        })
    }

    fn attributes(&self) -> String {
        let mut attributes = vec![
            "HttpOnly".to_string(),
            format!("SameSite={}", self.same_site.as_cookie_value()),
            "Path=/".to_string(),
        ];
        if let Some(domain) = &self.domain {
            attributes.push(format!("Domain={domain}"));
        }
        if self.secure {
            attributes.push("Secure".to_string());
        }
        attributes.join("; ")
    }
}

impl Default for EvmSessionCookieConfig {
    fn default() -> Self {
        Self {
            domain: None,
            same_site: SameSite::Strict,
            secure: false,
        }
    }
}

impl SameSite {
    fn parse(value: &str) -> std::result::Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Self::Strict),
            "lax" => Ok(Self::Lax),
            "none" => Ok(Self::None),
            _ => Err(format!(
                "{COOKIE_SAME_SITE_ENV} must be Strict, Lax, or None"
            )),
        }
    }

    fn as_cookie_value(&self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
            Self::None => "None",
        }
    }
}

fn parse_cookie_attribute_value(name: &str, value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value
            .chars()
            .any(|character| character == ';' || character == ',' || character.is_whitespace())
    {
        return Err(format!("{name} contains an invalid cookie attribute value"));
    }
    Ok(value.to_string())
}

fn parse_bool(name: &str, value: &str) -> std::result::Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("{name} must be true or false")),
    }
}

fn random_hex_32() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("0x{}", hex::encode(bytes))
}

fn cookie_date(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn challenge_is_single_use_after_manual_removal() {
        let state = EvmSessionState::default();
        let now = Utc::now();
        let challenge = state
            .issue_challenge("0x1111111111111111111111111111111111111111", now)
            .await
            .expect("challenge");

        let mut challenges = state.challenges.lock().await;
        let pending = challenges
            .get_mut(&challenge.address)
            .expect("pending challenge");
        assert_eq!(pending.len(), 1);
        pending.remove(0);
        assert!(pending.is_empty());
    }

    #[test]
    fn default_cookie_header_preserves_strict_host_only_cookie() {
        let state = EvmSessionState::default();
        let expires_at = Utc::now() + Duration::seconds(SESSION_TTL_SECS);

        let cookie = state.session_cookie_header("token", expires_at);

        assert!(cookie.contains("guardian_evm_session=token"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Domain="));
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn cross_subdomain_cookie_header_includes_configured_attributes() {
        let state = EvmSessionState {
            cookie_config: EvmSessionCookieConfig::new(
                Some(".openzeppelin.com".to_string()),
                SameSite::None,
                true,
            )
            .expect("cookie config"),
            ..EvmSessionState::default()
        };
        let expires_at = Utc::now() + Duration::seconds(SESSION_TTL_SECS);

        let cookie = state.session_cookie_header("token", expires_at);
        let clear_cookie = state.clear_cookie_header();

        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=None"));
        assert!(cookie.contains("Domain=.openzeppelin.com"));
        assert!(cookie.contains("Secure"));
        assert!(clear_cookie.contains("Domain=.openzeppelin.com"));
        assert!(clear_cookie.contains("Secure"));
    }

    #[test]
    fn same_site_none_requires_secure_cookie() {
        let error = EvmSessionCookieConfig::new(None, SameSite::None, false)
            .expect_err("SameSite=None without Secure should fail");

        assert!(error.contains(COOKIE_SECURE_ENV));
    }
}
