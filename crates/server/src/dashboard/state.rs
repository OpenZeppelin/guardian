use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::crypto::dsa::falcon512_poseidon2::Signature;
use tokio::sync::{Mutex, RwLock};

use super::allowlist::{
    AllowlistSource, OperatorAllowlist, OperatorAllowlistEntryInput, normalize_commitment,
};
use super::config::DashboardConfig;
use super::cursor::CursorSecret;
use super::types::{
    AuthenticatedOperator, IssuedOperatorSession, OperatorChallenge, OperatorChallengePayload,
    OperatorSessionRecord, PendingChallenge,
};
use super::util::{cookie_date, correlation_id, random_hex, rate_limit_error};
use crate::error::{GuardianError, Result};
use crate::middleware::rate_limit::RateLimitStore;

#[derive(Clone, Debug)]
pub struct DashboardState {
    config: DashboardConfig,
    allowlist_source: AllowlistSource,
    allowlist: Arc<RwLock<OperatorAllowlist>>,
    challenges: Arc<Mutex<HashMap<String, Vec<PendingChallenge>>>>,
    sessions: Arc<Mutex<HashMap<String, OperatorSessionRecord>>>,
    commitment_rate_limits: RateLimitStore,
    cursor_secret: CursorSecret,
    started_at: DateTime<Utc>,
}

impl DashboardState {
    pub async fn from_env() -> std::result::Result<Self, String> {
        let config = DashboardConfig::from_env()?;
        let allowlist_source = AllowlistSource::from_env().await?;
        let allowlist = allowlist_source.load().await?;
        Self::from_allowlist_source(allowlist_source, allowlist, config)
    }

    pub fn for_tests(entries: Vec<(String, String)>) -> Self {
        let inputs = entries
            .into_iter()
            .map(|(operator_id, commitment)| OperatorAllowlistEntryInput {
                operator_id,
                commitment,
            })
            .collect();
        let allowlist = OperatorAllowlist::from_entries(inputs)
            .expect("dashboard test configuration should be valid");
        Self::from_allowlist_source(
            AllowlistSource::Static,
            allowlist,
            DashboardConfig::for_tests(),
        )
        .expect("dashboard test configuration should be valid")
    }

    pub fn cookie_name(&self) -> &str {
        &self.config.cookie_name
    }

    pub fn clear_cookie_header(&self) -> String {
        let expires = "Thu, 01 Jan 1970 00:00:00 GMT";
        format!(
            "{}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0; Expires={}",
            self.config.cookie_name, expires
        )
    }

    pub async fn issue_challenge(
        &self,
        commitment: &str,
        now: DateTime<Utc>,
    ) -> Result<OperatorChallenge> {
        self.refresh_allowlist().await?;
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

        if self
            .lookup_allowlisted_operator(&normalized_commitment)
            .await
            .is_some()
        {
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
        self.refresh_allowlist().await?;
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
            .lookup_allowlisted_operator(&normalized_commitment)
            .await
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
        self.refresh_allowlist().await?;
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
            .lookup_allowlisted_operator(&session.operator.commitment)
            .await
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
        if let Some(token) = token
            && let Some(session) = sessions.remove(token)
        {
            tracing::info!(
                auth_event = "logout",
                operator_id = %session.operator.operator_id,
                issued_at = %session.issued_at.to_rfc3339(),
                "Operator session cleared"
            );
        }
    }

    fn from_allowlist_source(
        allowlist_source: AllowlistSource,
        allowlist: OperatorAllowlist,
        config: DashboardConfig,
    ) -> std::result::Result<Self, String> {
        tracing::info!(
            auth_event = "allowlist_loaded",
            operator_count = allowlist.len(),
            "Operator allowlist loaded"
        );
        let cursor_secret = match config.cursor_secret_hex() {
            Some(hex) => CursorSecret::from_hex(hex).map_err(|e| {
                format!(
                    "GUARDIAN_DASHBOARD_CURSOR_SECRET must be 32 hex-encoded bytes (64 chars): {e}"
                )
            })?,
            None => {
                if !cfg!(test) {
                    tracing::warn!(
                        "dashboard cursor secret not configured; generating ephemeral per-process \
                         secret. Multi-replica deployments must set \
                         GUARDIAN_DASHBOARD_CURSOR_SECRET to a stable shared 32-byte hex value."
                    );
                }
                CursorSecret::generate()
            }
        };
        Ok(Self {
            commitment_rate_limits: RateLimitStore::new(config.commitment_rate_limit.clone()),
            config,
            allowlist_source,
            allowlist: Arc::new(RwLock::new(allowlist)),
            challenges: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            cursor_secret,
            started_at: Utc::now(),
        })
    }

    /// Server-side signing secret for opaque pagination cursors. See
    /// `crate::dashboard::cursor`. Generated once per server startup.
    pub fn cursor_secret(&self) -> &CursorSecret {
        &self.cursor_secret
    }

    /// Filesystem-backed cross-account aggregate threshold from
    /// [`DashboardConfig`]. Used by feature `005-operator-dashboard-metrics`
    /// per FR-029.
    pub fn filesystem_aggregate_threshold(&self) -> usize {
        self.config.filesystem_aggregate_threshold()
    }

    /// Deployment environment identifier surfaced on
    /// `GET /dashboard/info` (e.g. `mainnet`, `testnet`).
    pub fn environment(&self) -> &str {
        self.config.environment()
    }

    /// Wall-clock time the dashboard state (and effectively the process)
    /// was initialized. Surfaced on `GET /dashboard/info` to identify
    /// the running binary instance.
    pub fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }

    async fn refresh_allowlist(&self) -> Result<()> {
        let Some(updated_allowlist) = self
            .allowlist_source
            .load_dynamic()
            .await
            .map_err(GuardianError::ConfigurationError)?
        else {
            return Ok(());
        };

        let mut allowlist = self.allowlist.write().await;
        if *allowlist != updated_allowlist {
            tracing::info!(
                auth_event = "allowlist_reloaded",
                operator_count = updated_allowlist.len(),
                source = %self.allowlist_source.label(),
                "Operator allowlist reloaded"
            );
            *allowlist = updated_allowlist;
        }
        Ok(())
    }

    async fn lookup_allowlisted_operator(&self, commitment: &str) -> Option<AuthenticatedOperator> {
        let allowlist = self.allowlist.read().await;
        allowlist.lookup(commitment).cloned()
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
        format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}; Expires={}",
            self.config.cookie_name,
            token,
            max_age,
            cookie_date(expires_at)
        )
    }
}

impl Default for DashboardState {
    fn default() -> Self {
        Self::for_tests(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::LazyLock;

    use chrono::{Duration, Utc};
    use guardian_shared::hex::FromHex;
    use miden_protocol::Word;
    use tokio::sync::Mutex as TokioMutex;
    use uuid::Uuid;

    use super::super::allowlist::{
        ENV_OPERATOR_PUBLIC_KEYS_FILE, ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID,
    };
    use super::super::config::{
        DEFAULT_CANONICAL_DOMAIN, DEFAULT_COOKIE_NAME, DEFAULT_MAX_OUTSTANDING_CHALLENGES,
        DEFAULT_NONCE_TTL_SECS, DEFAULT_PUBKEY_RATE_BURST_PER_SEC, DEFAULT_PUBKEY_RATE_PER_MIN,
        DEFAULT_SESSION_TTL_SECS, OPEN_DASHBOARD_DOMAIN,
    };
    use super::{DashboardConfig, DashboardState};
    use crate::testing::helpers::TestSigner;

    static ENV_LOCK: LazyLock<TokioMutex<()>> = LazyLock::new(|| TokioMutex::new(()));

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<str>) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value.as_ref()) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    fn operator_public_keys_json(public_keys: &[&str]) -> String {
        serde_json::to_string(public_keys).expect("operator public keys should serialize")
    }

    fn write_operator_public_keys_file(path: &std::path::Path, public_keys: &[&str]) {
        fs::write(path, operator_public_keys_json(public_keys))
            .expect("operator public keys file should be written");
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn dashboard_config_uses_opinionated_defaults() {
        let config = DashboardConfig::default();

        assert_eq!(config.canonical_domain, DEFAULT_CANONICAL_DOMAIN);
        assert_eq!(config.canonical_domain, OPEN_DASHBOARD_DOMAIN);
        assert_eq!(config.cookie_name, DEFAULT_COOKIE_NAME);
        assert_eq!(config.nonce_ttl, Duration::seconds(DEFAULT_NONCE_TTL_SECS));
        assert_eq!(
            config.session_ttl,
            Duration::seconds(DEFAULT_SESSION_TTL_SECS)
        );
        assert_eq!(
            config.max_outstanding_challenges,
            DEFAULT_MAX_OUTSTANDING_CHALLENGES
        );
        assert!(config.commitment_rate_limit.enabled);
        assert_eq!(
            config.commitment_rate_limit.burst_per_sec,
            DEFAULT_PUBKEY_RATE_BURST_PER_SEC
        );
        assert_eq!(
            config.commitment_rate_limit.per_min,
            DEFAULT_PUBKEY_RATE_PER_MIN
        );
    }

    #[tokio::test]
    async fn dashboard_state_loads_operator_public_keys_from_file() {
        let _env_lock = ENV_LOCK.lock().await;
        let operator_one = TestSigner::new();
        let operator_two = TestSigner::new();
        let path = std::env::temp_dir().join(format!(
            "guardian_operator_public_keys_{}.json",
            Uuid::new_v4()
        ));

        write_operator_public_keys_file(
            &path,
            &[&operator_one.pubkey_hex, &operator_two.pubkey_hex],
        );
        let _operator_public_keys_file =
            EnvVarGuard::set(ENV_OPERATOR_PUBLIC_KEYS_FILE, path.display().to_string());
        let _operator_public_keys_secret = EnvVarGuard::remove(ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID);

        let state = DashboardState::from_env()
            .await
            .expect("dashboard state should load");
        let now = Utc::now();
        let challenge_one = state
            .issue_challenge(&operator_one.commitment_hex, now)
            .await
            .expect("first challenge should succeed");
        let signature_one = operator_one
            .sign_word(Word::from_hex(&challenge_one.signing_digest).expect("digest should parse"));
        state
            .verify(&operator_one.commitment_hex, &signature_one, now)
            .await
            .expect("first verify should succeed");

        let challenge_two = state
            .issue_challenge(&operator_two.commitment_hex, now)
            .await
            .expect("second challenge should succeed");
        let signature_two = operator_two
            .sign_word(Word::from_hex(&challenge_two.signing_digest).expect("digest should parse"));
        let session_two = state
            .verify(&operator_two.commitment_hex, &signature_two, now)
            .await
            .expect("second verify should succeed");
        assert_eq!(
            session_two.operator.operator_id,
            operator_two.commitment_hex
        );

        fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn dashboard_state_rereads_operator_public_keys_file_in_process() {
        let _env_lock = ENV_LOCK.lock().await;
        let operator_one = TestSigner::new();
        let operator_two = TestSigner::new();
        let path = std::env::temp_dir().join(format!(
            "guardian_operator_public_keys_{}.json",
            Uuid::new_v4()
        ));

        write_operator_public_keys_file(&path, &[&operator_one.pubkey_hex]);
        let _operator_public_keys_file =
            EnvVarGuard::set(ENV_OPERATOR_PUBLIC_KEYS_FILE, path.display().to_string());
        let _operator_public_keys_secret = EnvVarGuard::remove(ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID);

        let state = DashboardState::from_env()
            .await
            .expect("dashboard state should load");
        let now = Utc::now();
        let challenge_one = state
            .issue_challenge(&operator_one.commitment_hex, now)
            .await
            .expect("first challenge should succeed");
        let signature_one = operator_one
            .sign_word(Word::from_hex(&challenge_one.signing_digest).expect("digest should parse"));
        state
            .verify(&operator_one.commitment_hex, &signature_one, now)
            .await
            .expect("first verify should succeed");

        let original_token = {
            let sessions = state.sessions.lock().await;
            sessions
                .keys()
                .next()
                .expect("a session token should exist")
                .clone()
        };

        write_operator_public_keys_file(&path, &[&operator_two.pubkey_hex]);

        let later = now + Duration::seconds(1);
        let challenge_two = state
            .issue_challenge(&operator_two.commitment_hex, later)
            .await
            .expect("updated env challenge should succeed");
        let signature_two = operator_two
            .sign_word(Word::from_hex(&challenge_two.signing_digest).expect("digest should parse"));
        let session_two = state
            .verify(&operator_two.commitment_hex, &signature_two, later)
            .await
            .expect("updated env verify should succeed");
        assert_eq!(
            session_two.operator.operator_id,
            operator_two.commitment_hex
        );

        assert!(
            state
                .authenticate_session(&original_token, later)
                .await
                .is_err(),
            "old session should be revoked after allowlist reload"
        );

        fs::remove_file(path).ok();
    }
}
