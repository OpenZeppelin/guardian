use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;

use crate::coordination::{
    ChallengePayload, ChallengeStore, InMemoryChallengeStore, InMemorySessionStore, SessionStore,
    SessionSubject, StoredChallenge, StoredSession,
};
use crate::error::{GuardianError, Result};
use crate::metadata::network::normalize_evm_address;
use crate::secret::session_digest;

const COOKIE_NAME: &str = "guardian_evm_session";
const CHALLENGE_TTL_SECS: i64 = 300;
const SESSION_TTL_SECS: i64 = 8 * 60 * 60;
const MAX_OUTSTANDING_CHALLENGES: usize = 8;

#[derive(Clone)]
pub struct EvmSessionState {
    session_store: Arc<dyn SessionStore>,
    challenge_store: Arc<dyn ChallengeStore>,
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

impl Default for EvmSessionState {
    fn default() -> Self {
        Self::new(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryChallengeStore::new()),
        )
    }
}

impl EvmSessionState {
    /// Build EVM session state over explicit, evm-realm coordination stores. The
    /// server builder passes shared (Postgres) stores on the Postgres backend;
    /// the default uses in-memory stores (single-process / dev).
    pub fn new(
        session_store: Arc<dyn SessionStore>,
        challenge_store: Arc<dyn ChallengeStore>,
    ) -> Self {
        Self {
            session_store,
            challenge_store,
        }
    }

    pub fn cookie_name(&self) -> &'static str {
        COOKIE_NAME
    }

    pub fn clear_cookie_header(&self) -> String {
        let expires = Utc::now() - Duration::days(1);
        format!(
            "{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0; Expires={}",
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

        let stored = StoredChallenge {
            key: challenge.nonce.clone(),
            payload: ChallengePayload::EvmChallenge {
                address: challenge.address.clone(),
                nonce: challenge.nonce.clone(),
                issued_at: challenge.issued_at,
                expires_at: challenge.expires_at,
            },
            issued_at: challenge.issued_at,
            expires_at: challenge.expires_at,
        };
        self.challenge_store
            .issue(&address, stored, MAX_OUTSTANDING_CHALLENGES, now)
            .await?;

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

        let active = self.challenge_store.active_for(&address, now).await?;
        let matched = active.iter().find_map(|stored| match &stored.payload {
            ChallengePayload::EvmChallenge {
                address: challenge_address,
                nonce: challenge_nonce,
                issued_at,
                expires_at,
            } if challenge_nonce.eq_ignore_ascii_case(nonce) => Some((
                stored.key.clone(),
                EvmChallenge {
                    address: challenge_address.clone(),
                    nonce: challenge_nonce.clone(),
                    issued_at: *issued_at,
                    expires_at: *expires_at,
                },
            )),
            _ => None,
        });

        let Some((key, challenge)) = matched else {
            return Err(GuardianError::AuthenticationFailed(
                "No active EVM challenge matched the nonce".to_string(),
            ));
        };

        let recovered = crate::evm::contracts::recover_session_address(&challenge, &signature)?;
        if recovered != address {
            return Err(GuardianError::AuthenticationFailed(
                "EVM session signature does not match requested address".to_string(),
            ));
        }

        if !self.challenge_store.consume(&address, &key, now).await? {
            return Err(GuardianError::AuthenticationFailed(
                "No active EVM challenge matched the nonce".to_string(),
            ));
        }

        let token = random_hex_32();
        let expires_at = now + Duration::seconds(SESSION_TTL_SECS);
        let cookie_header = self.session_cookie_header(&token, expires_at);
        let session_key = session_digest(&token);
        self.session_store
            .insert(
                session_key,
                StoredSession {
                    subject: SessionSubject::Evm {
                        address: address.clone(),
                    },
                    issued_at: now,
                    expires_at,
                },
            )
            .await?;

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
        let session = self
            .session_store
            .get(&session_digest(token), now)
            .await?
            .ok_or_else(|| {
                GuardianError::AuthenticationFailed("Invalid EVM session".to_string())
            })?;
        let SessionSubject::Evm { address } = session.subject else {
            return Err(GuardianError::AuthenticationFailed(
                "Invalid EVM session".to_string(),
            ));
        };
        Ok(AuthenticatedEvmSession { address })
    }

    pub async fn logout(&self, token: Option<&str>, _now: DateTime<Utc>) -> Result<()> {
        if let Some(token) = token {
            self.session_store.revoke(&session_digest(token)).await?;
        }
        Ok(())
    }

    /// Reclaim expired EVM sessions and challenges (housekeeping; expiry is also
    /// enforced on read).
    pub async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<()> {
        self.session_store.sweep_expired(now).await?;
        self.challenge_store.sweep_expired(now).await?;
        Ok(())
    }

    fn session_cookie_header(&self, token: &str, expires_at: DateTime<Utc>) -> String {
        format!(
            "{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_TTL_SECS}; Expires={}",
            cookie_date(expires_at)
        )
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
    async fn challenge_is_single_use_via_consume() {
        let state = EvmSessionState::default();
        let now = Utc::now();
        let challenge = state
            .issue_challenge("0x1111111111111111111111111111111111111111", now)
            .await
            .expect("challenge");

        let active = state
            .challenge_store
            .active_for(&challenge.address, now)
            .await
            .expect("active challenges");
        assert_eq!(active.len(), 1);

        assert!(
            state
                .challenge_store
                .consume(&challenge.address, &challenge.nonce, now)
                .await
                .expect("consume")
        );
        assert!(
            !state
                .challenge_store
                .consume(&challenge.address, &challenge.nonce, now)
                .await
                .expect("replay consume")
        );
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
}
