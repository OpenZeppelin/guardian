//! Miden account sessions (issue #219); EVM sessions live in `evm::session`.
//!
//! A wallet signs a session grant once to authorize a client-held P-256
//! delegated signer; afterwards that key signs ordinary per-account requests
//! (`x-auth-format: session`). This module owns the session configuration,
//! the realm-scoped session store, and the delegated-signer key and signature
//! checks. Grant, request, logout and revoke-all flows live in
//! `services::{create_session, resolve_account, revoke_session,
//! revoke_all_sessions}`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use guardian_shared::session_grant::SESSION_PUBLIC_KEY_LEN;
use miden_protocol::Word;

use crate::coordination::{InMemorySessionStore, SessionStore, SessionSubject, StoredSession};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::MAX_TIMESTAMP_SKEW_SECS;

/// Env var capping how long after registration a session may expire.
pub const SESSION_MAX_TTL_ENV: &str = "GUARDIAN_SESSION_MAX_TTL_SECONDS";
/// Default and upper bound of the maximum lifetime: 8 hours, as for operator
/// and EVM sessions. Operators may only shorten it.
pub const DEFAULT_SESSION_MAX_TTL_SECONDS: u32 = 8 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    max_ttl_seconds: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_ttl_seconds: DEFAULT_SESSION_MAX_TTL_SECONDS,
        }
    }
}

impl SessionConfig {
    /// `max_ttl_seconds` must be longer than the request clock-skew window
    /// and at most [`DEFAULT_SESSION_MAX_TTL_SECONDS`].
    pub fn new(max_ttl_seconds: u32) -> Result<Self> {
        if u64::from(max_ttl_seconds) <= MAX_TIMESTAMP_SKEW_SECS
            || max_ttl_seconds > DEFAULT_SESSION_MAX_TTL_SECONDS
        {
            return Err(GuardianError::ConfigurationError(format!(
                "{SESSION_MAX_TTL_ENV} must be between {} and {DEFAULT_SESSION_MAX_TTL_SECONDS} seconds",
                MAX_TIMESTAMP_SKEW_SECS + 1
            )));
        }
        Ok(Self { max_ttl_seconds })
    }

    pub fn from_env() -> Result<Self> {
        let max_ttl_seconds = crate::config::positive_u32_from_env(
            SESSION_MAX_TTL_ENV,
            DEFAULT_SESSION_MAX_TTL_SECONDS,
        )
        .map_err(GuardianError::ConfigurationError)?;
        Self::new(max_ttl_seconds)
    }

    pub fn max_ttl_seconds(&self) -> u64 {
        self.max_ttl_seconds.into()
    }
}

/// What a registered grant binds, as stored with the session and re-checked
/// on every request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidenSession {
    /// Commitment of the wallet key that signed the grant.
    pub signer_commitment: String,
    /// The website that asked for the grant; empty outside a browser.
    /// Informational: never checked against requests.
    pub origin: String,
    /// This Guardian's ACK-key commitment for the wallet's scheme at grant time.
    pub guardian_commitment: String,
    /// The Miden network the grant names.
    pub network: String,
}

impl MidenSession {
    fn subject(&self) -> SessionSubject {
        SessionSubject::Miden {
            signer_commitment: self.signer_commitment.clone(),
            origin: self.origin.clone(),
            guardian_commitment: self.guardian_commitment.clone(),
            network: self.network.clone(),
        }
    }
}

pub struct MidenSessions {
    config: SessionConfig,
    store: Arc<dyn SessionStore>,
}

impl Default for MidenSessions {
    fn default() -> Self {
        Self::new(
            SessionConfig::default(),
            Arc::new(InMemorySessionStore::new()),
        )
    }
}

impl MidenSessions {
    pub fn new(config: SessionConfig, store: Arc<dyn SessionStore>) -> Self {
        Self { config, store }
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Stores a verified grant and returns the session expiry. `issued_at`
    /// and `expires_at` are the grant's Unix seconds.
    ///
    /// Revoke-all compares against the stored issue time, which is the
    /// grant's `issued_at` capped at `now`: a grant dated in the future (a
    /// fast client clock, or a page that chose it) cannot outlive a wallet
    /// revoke-all signed after it was registered.
    ///
    /// Re-submitting the grant of a live session succeeds and returns the
    /// same expiry, so a client that lost the first response can retry. A key
    /// already bound to a different grant, or whose session was revoked or
    /// has expired but is not swept yet, is refused: a revoked session can
    /// never be revived and a key can never be re-bound to another signer.
    pub(crate) async fn register(
        &self,
        session_public_key: &[u8; SESSION_PUBLIC_KEY_LEN],
        session: MidenSession,
        issued_at: u64,
        expires_at: u64,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        let to_time = |seconds: u64| {
            DateTime::<Utc>::from_timestamp(seconds as i64, 0)
                .ok_or_else(|| GuardianError::InvalidInput("Invalid session time".to_string()))
        };
        let expires_at = to_time(expires_at)?;
        let key = store_key(session_public_key);
        let stored = StoredSession {
            subject: session.subject(),
            issued_at: to_time(issued_at)?.min(now),
            expires_at,
        };
        if self.store.insert_new(key, stored.clone()).await? {
            return Ok(expires_at);
        }
        match self.store.get(&key, now).await? {
            Some(existing)
                if existing.subject == stored.subject && existing.expires_at == expires_at =>
            {
                Ok(expires_at)
            }
            Some(_) => Err(GuardianError::AuthenticationFailed(
                "Session key is already registered with a different grant".to_string(),
            )),
            None => Err(GuardianError::AuthenticationFailed(
                "Session key was revoked or has expired; start a session with a new key"
                    .to_string(),
            )),
        }
    }

    /// The active session for a session public key, or why there is none:
    /// `SessionRevoked`, `SessionExpired`, or `AuthenticationFailed` for a key
    /// that was never registered (or whose expired record was swept).
    pub(crate) async fn find(
        &self,
        session_public_key: &[u8; SESSION_PUBLIC_KEY_LEN],
        now: DateTime<Utc>,
    ) -> Result<MidenSession> {
        let key = store_key(session_public_key);
        if let Some(session) = self.store.get(&key, now).await? {
            return match session.subject {
                SessionSubject::Miden {
                    signer_commitment,
                    origin,
                    guardian_commitment,
                    network,
                } => Ok(MidenSession {
                    signer_commitment,
                    origin,
                    guardian_commitment,
                    network,
                }),
                _ => Err(unknown_session()),
            };
        }
        Err(match self.store.inactive_reason(&key).await? {
            Some(true) => GuardianError::SessionRevoked,
            Some(false) => GuardianError::SessionExpired,
            None => unknown_session(),
        })
    }

    /// Revokes the session for a key. Returns the signer commitment of the
    /// session that was active, or `None` when there was none.
    pub(crate) async fn revoke(
        &self,
        session_public_key: &[u8; SESSION_PUBLIC_KEY_LEN],
    ) -> Result<Option<String>> {
        Ok(self
            .store
            .revoke(&store_key(session_public_key))
            .await?
            .and_then(|session| match session.subject {
                SessionSubject::Miden {
                    signer_commitment, ..
                } => Some(signer_commitment),
                _ => None,
            }))
    }

    /// Revokes every active session of a signer issued at or before
    /// `issued_at_or_before`. Returns how many there were. Sessions granted
    /// later survive, so replaying an old revoke-all cannot end them.
    pub(crate) async fn revoke_all(
        &self,
        signer_commitment: &str,
        issued_at_or_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        self.store
            .revoke_by_subject(
                &serde_json::json!({ "realm": "miden", "signer_commitment": signer_commitment }),
                issued_at_or_before,
                now,
            )
            .await
    }

    /// Reclaim expired sessions (housekeeping; expiry is also enforced on read).
    pub async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<()> {
        self.store.sweep_expired(now).await?;
        Ok(())
    }
}

fn unknown_session() -> GuardianError {
    GuardianError::AuthenticationFailed("Unknown session key".to_string())
}

/// Prefix of a delegated signer's replay-floor key. It contains no `:`, the
/// separator the filesystem store uses between account and signer.
pub(crate) const SESSION_FLOOR_PREFIX: &str = "session-";

/// The replay-floor key of a delegated signer: the prefix and the hex SHA-256
/// of its public key, so logs never carry the key itself. Session requests
/// advance their own floor per (account, delegated key), never the wallet's
/// per (account, signer), so a stolen session cannot lock the wallet out.
pub(crate) fn replay_floor_key(session_public_key: &[u8; SESSION_PUBLIC_KEY_LEN]) -> String {
    format!(
        "{SESSION_FLOOR_PREFIX}{}",
        hex::encode(store_key(session_public_key))
    )
}

/// Whether a replay-state signer key is a delegated signer's floor.
pub(crate) fn is_replay_floor_key(key: &str) -> bool {
    key.strip_prefix(SESSION_FLOOR_PREFIX)
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Last request timestamp (ms) a still-live session can have advanced its
/// floor to before `now_ms`: older floors belong to ended sessions and can be
/// deleted. A session's requests are stamped within the skew window of its
/// lifetime, which is at most `max_ttl_seconds` after its grant.
pub(crate) fn stale_floor_cutoff_ms(now_ms: i64, max_ttl_seconds: u64) -> i64 {
    let window_ms = (max_ttl_seconds + 2 * MAX_TIMESTAMP_SKEW_SECS) as i64 * 1000;
    now_ms.saturating_sub(window_ms)
}

fn decode_public_key(value: &str) -> std::result::Result<[u8; SESSION_PUBLIC_KEY_LEN], String> {
    let bytes = hex::decode(value.trim_start_matches("0x"))
        .map_err(|_| "Invalid session public key hex".to_string())?;
    guardian_shared::session_grant::parse_session_public_key(&bytes)
}

/// Parses a hex session public key, rejecting points off the curve.
pub(crate) fn parse_public_key_hex(value: &str) -> Result<[u8; SESSION_PUBLIC_KEY_LEN]> {
    decode_public_key(value).map_err(GuardianError::InvalidInput)
}

/// Parses the session public key carried in request credentials (`x-pubkey`).
pub(crate) fn credential_public_key(value: &str) -> Result<[u8; SESSION_PUBLIC_KEY_LEN]> {
    decode_public_key(value).map_err(GuardianError::AuthenticationFailed)
}

/// Verifies a session-key signature from request credentials.
pub(crate) fn verify_signature(
    public_key: &[u8; SESSION_PUBLIC_KEY_LEN],
    signature_hex: &str,
    message: Word,
) -> Result<()> {
    let signature = hex::decode(signature_hex.trim_start_matches("0x")).map_err(|_| {
        GuardianError::AuthenticationFailed("Invalid session signature hex".to_string())
    })?;
    guardian_shared::session_key::verify(public_key, message, &signature)
        .map_err(GuardianError::AuthenticationFailed)
}

/// The session record's key: SHA-256 of the 33-byte compressed public key.
fn store_key(public_key: &[u8; SESSION_PUBLIC_KEY_LEN]) -> crate::coordination::SessionKey {
    use sha2::{Digest, Sha256};
    Sha256::digest(public_key).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use guardian_shared::session_key::SessionKey;

    fn session(signer: &str) -> MidenSession {
        MidenSession {
            signer_commitment: signer.to_string(),
            origin: "https://multisig.example".to_string(),
            guardian_commitment: "0xguardian".to_string(),
            network: "devnet".to_string(),
        }
    }

    fn key(seed: u8) -> [u8; SESSION_PUBLIC_KEY_LEN] {
        SessionKey::from_bytes(&[seed; 32]).unwrap().public_key()
    }

    fn now_and_secs() -> (DateTime<Utc>, u64) {
        let now = Utc::now();
        (now, now.timestamp() as u64)
    }

    #[test]
    fn config_bounds_the_maximum_lifetime() {
        assert_eq!(SessionConfig::new(3600).unwrap().max_ttl_seconds(), 3600);
        assert_eq!(
            SessionConfig::default().max_ttl_seconds(),
            u64::from(DEFAULT_SESSION_MAX_TTL_SECONDS)
        );
        assert!(SessionConfig::new(0).is_err());
        assert!(
            SessionConfig::new(MAX_TIMESTAMP_SKEW_SECS as u32).is_err(),
            "must outlive the skew window"
        );
        assert!(SessionConfig::new(MAX_TIMESTAMP_SKEW_SECS as u32 + 1).is_ok());
        assert!(SessionConfig::new(DEFAULT_SESSION_MAX_TTL_SECONDS + 1).is_err());
    }

    #[test]
    fn config_reads_env() {
        let _lock = crate::testing::env_lock::ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var(SESSION_MAX_TTL_ENV, "600");
        }
        let config = SessionConfig::from_env();
        unsafe {
            std::env::set_var(SESSION_MAX_TTL_ENV, "abc");
        }
        let invalid = SessionConfig::from_env();
        unsafe {
            std::env::remove_var(SESSION_MAX_TTL_ENV);
        }
        let default = SessionConfig::from_env();

        assert_eq!(config.unwrap().max_ttl_seconds(), 600);
        assert!(invalid.is_err());
        assert_eq!(default.unwrap(), SessionConfig::default());
    }

    #[tokio::test]
    async fn register_find_and_revoke() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();

        sessions
            .register(&key(1), session("0xabc"), secs, secs + 600, now)
            .await
            .unwrap();
        assert_eq!(sessions.find(&key(1), now).await.unwrap(), session("0xabc"));

        assert_eq!(
            sessions.revoke(&key(1)).await.unwrap().as_deref(),
            Some("0xabc"),
            "logout reports the signer it ended"
        );
        assert!(matches!(
            sessions.find(&key(1), now).await,
            Err(GuardianError::SessionRevoked)
        ));
        assert_eq!(sessions.revoke(&key(1)).await.unwrap(), None);
        assert!(matches!(
            sessions.find(&key(9), now).await,
            Err(GuardianError::AuthenticationFailed(_))
        ));
    }

    #[tokio::test]
    async fn resubmission_keeps_the_recorded_issue_time() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();
        sessions
            .register(&key(4), session("0xabc"), secs, secs + 600, now)
            .await
            .unwrap();

        // Re-signed a minute later: the same session, still recorded as
        // issued at `secs`, so a revoke-all signed in between still ends it.
        let later = now + chrono::Duration::seconds(60);
        sessions
            .register(&key(4), session("0xabc"), secs + 60, secs + 600, later)
            .await
            .unwrap();
        let between = DateTime::<Utc>::from_timestamp(secs as i64 + 30, 0).unwrap();
        assert_eq!(
            sessions.revoke_all("0xabc", between, later).await.unwrap(),
            1
        );
        assert!(matches!(
            sessions.find(&key(4), later).await,
            Err(GuardianError::SessionRevoked)
        ));
    }

    #[tokio::test]
    async fn resubmitting_a_live_grant_is_idempotent() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();

        let first = sessions
            .register(&key(2), session("0xabc"), secs, secs + 600, now)
            .await
            .unwrap();
        let again = sessions
            .register(&key(2), session("0xabc"), secs, secs + 600, now)
            .await
            .unwrap();
        assert_eq!(first, again);

        // Same signer, origin, Guardian and expiry: the same session, even if
        // the wallet re-signed it with another `issued_at`.
        assert_eq!(
            sessions
                .register(&key(2), session("0xabc"), secs + 1, secs + 600, now)
                .await
                .unwrap(),
            first
        );
        for (signer, issued, expires) in [("0xabc", secs, secs + 601), ("0xdef", secs, secs + 600)]
        {
            assert!(
                sessions
                    .register(&key(2), session(signer), issued, expires, now)
                    .await
                    .is_err(),
                "a different grant for the same key is refused: {signer} {issued} {expires}"
            );
        }
        let mut other_origin = session("0xabc");
        other_origin.origin = "https://phishing.example".to_string();
        assert!(
            sessions
                .register(&key(2), other_origin, secs, secs + 600, now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_revoked_session_key_cannot_be_registered_again() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();

        sessions
            .register(&key(3), session("0xabc"), secs, secs + 600, now)
            .await
            .unwrap();
        sessions.revoke(&key(3)).await.unwrap();
        assert!(
            sessions
                .register(&key(3), session("0xabc"), secs, secs + 600, now)
                .await
                .is_err(),
            "a revoked session must not be revived"
        );
    }

    #[tokio::test]
    async fn expired_session_reports_expiry() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();
        sessions
            .register(&key(4), session("0xabc"), secs, secs + 60, now)
            .await
            .unwrap();

        let later = now + Duration::seconds(61);
        assert!(matches!(
            sessions.find(&key(4), later).await,
            Err(GuardianError::SessionExpired)
        ));
    }

    #[tokio::test]
    async fn revoke_all_ends_sessions_issued_up_to_the_cutoff() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();
        // (key seed, signer, issued_at)
        let grants = [
            (5u8, "0xabc", secs - 10),
            (6, "0xabc", secs),
            (7, "0xabc", secs + 10),
            (8, "0xdef", secs - 10),
        ];
        for (seed, signer, issued) in grants {
            sessions
                .register(&key(seed), session(signer), issued, secs + 600, now)
                .await
                .unwrap();
        }

        let cutoff = DateTime::<Utc>::from_timestamp(secs as i64, 0).unwrap();
        assert_eq!(sessions.revoke_all("0xabc", cutoff, now).await.unwrap(), 2);
        for seed in [5, 6] {
            assert!(matches!(
                sessions.find(&key(seed), now).await,
                Err(GuardianError::SessionRevoked)
            ));
        }
        assert!(
            sessions.find(&key(7), now).await.is_ok(),
            "granted after the cutoff: a replayed revoke-all cannot end it"
        );
        assert!(sessions.find(&key(8), now).await.is_ok(), "another signer");
        assert_eq!(
            sessions.revoke_all("0xabc", cutoff, now).await.unwrap(),
            0,
            "idempotent"
        );
    }

    #[tokio::test]
    async fn a_grant_dated_in_the_future_cannot_outlive_revoke_all() {
        let sessions = MidenSessions::default();
        let (now, secs) = now_and_secs();
        sessions
            .register(&key(10), session("0xabc"), secs + 250, secs + 600, now)
            .await
            .unwrap();

        assert_eq!(
            sessions
                .register(&key(10), session("0xabc"), secs + 250, secs + 600, now)
                .await
                .unwrap()
                .timestamp() as u64,
            secs + 600,
            "re-submission stays idempotent"
        );
        assert_eq!(sessions.revoke_all("0xabc", now, now).await.unwrap(), 1);
    }

    #[test]
    fn stale_floor_cutoff_outlasts_every_live_session() {
        let hour_ms = 3_600_000;
        assert_eq!(
            stale_floor_cutoff_ms(10 * hour_ms, 3600),
            10 * hour_ms - hour_ms - 600_000
        );
        assert_eq!(
            stale_floor_cutoff_ms(i64::MIN, 28_800),
            i64::MIN,
            "saturates"
        );
    }

    #[test]
    fn session_and_wallet_replay_floors_never_collide() {
        let floor = replay_floor_key(&key(1));
        assert!(is_replay_floor_key(&floor));
        assert!(
            !floor.contains(':'),
            "the filesystem store splits keys on ':'"
        );
        assert!(
            !floor.contains(&hex::encode(key(1))),
            "logs never carry the session public key"
        );
        assert_ne!(floor, replay_floor_key(&key(2)));
        for wallet_floor in [format!("0x{}", "ab".repeat(32)), "legacy".to_string()] {
            assert!(!is_replay_floor_key(&wallet_floor));
        }
    }

    #[test]
    fn credential_failures_are_authentication_failures() {
        let key = SessionKey::from_bytes(&[4; 32]).unwrap();
        let message = Word::from([1u32, 2, 3, 4]);
        let public_key = credential_public_key(&key.public_key_hex()).unwrap();

        assert!(verify_signature(&public_key, &key.sign_hex(message), message).is_ok());
        assert!(matches!(
            credential_public_key("0xzz"),
            Err(GuardianError::AuthenticationFailed(_))
        ));
        for signature in [key.sign_hex(Word::default()), "0xzz".to_string()] {
            assert!(matches!(
                verify_signature(&public_key, &signature, message),
                Err(GuardianError::AuthenticationFailed(_))
            ));
        }
    }
}
