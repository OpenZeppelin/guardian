use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::Result;

pub type SessionKey = [u8; 32];

/// Realm-specific authenticated identity persisted with a session. Operator
/// permissions are intentionally absent: they are re-resolved from the live
/// allowlist on each request, so only the stable identity is stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "realm", rename_all = "snake_case")]
pub enum SessionSubject {
    Operator {
        operator_id: String,
        commitment: String,
    },
    Evm {
        address: String,
    },
}

#[derive(Clone, Debug)]
pub struct StoredSession {
    pub subject: SessionSubject,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A store of authenticated sessions keyed by the SHA-256 digest of the session
/// token. Each instance is bound to a single realm at construction (the Postgres
/// implementation scopes its rows by that realm; the in-memory implementation is
/// instance-scoped). Implementations expose only unexpired, unrevoked sessions;
/// reclamation of expired rows is the job of [`SessionStore::sweep_expired`].
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn insert(&self, key: SessionKey, session: StoredSession) -> Result<()>;
    async fn get(&self, key: &SessionKey, now: DateTime<Utc>) -> Result<Option<StoredSession>>;
    /// Revoke a session (logout), returning the prior session if present for
    /// logout-side logging. The cross-replica contract: once revoked, `get` MUST
    /// reject it on every replica until its natural expiry. The Postgres
    /// implementation marks `revoked_at` and keeps the row until expiry; the
    /// in-memory implementation removes it.
    async fn revoke(&self, key: &SessionKey) -> Result<Option<StoredSession>>;
    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<u64>;
}

#[derive(Clone, Default)]
pub struct InMemorySessionStore {
    sessions: Arc<Mutex<HashMap<SessionKey, StoredSession>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn insert(&self, key: SessionKey, session: StoredSession) -> Result<()> {
        self.sessions.lock().await.insert(key, session);
        Ok(())
    }

    async fn get(&self, key: &SessionKey, now: DateTime<Utc>) -> Result<Option<StoredSession>> {
        Ok(self
            .sessions
            .lock()
            .await
            .get(key)
            .filter(|session| session.expires_at > now)
            .cloned())
    }

    async fn revoke(&self, key: &SessionKey) -> Result<Option<StoredSession>> {
        Ok(self.sessions.lock().await.remove(key))
    }

    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<u64> {
        let mut sessions = self.sessions.lock().await;
        let before = sessions.len();
        sessions.retain(|_, session| session.expires_at > now);
        Ok((before - sessions.len()) as u64)
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use chrono::Duration;

    fn operator_session(now: DateTime<Utc>, ttl_secs: i64) -> StoredSession {
        StoredSession {
            subject: SessionSubject::Operator {
                operator_id: "op-1".to_string(),
                commitment: "0xabc".to_string(),
            },
            issued_at: now,
            expires_at: now + Duration::seconds(ttl_secs),
        }
    }

    #[tokio::test]
    async fn get_returns_unexpired_and_hides_expired() {
        let store = InMemorySessionStore::new();
        let now = Utc::now();
        store
            .insert([1u8; 32], operator_session(now, 60))
            .await
            .unwrap();

        assert!(store.get(&[1u8; 32], now).await.unwrap().is_some());
        assert!(
            store
                .get(&[1u8; 32], now + Duration::seconds(61))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn revoke_returns_record_then_absent() {
        let store = InMemorySessionStore::new();
        let now = Utc::now();
        store
            .insert([2u8; 32], operator_session(now, 60))
            .await
            .unwrap();

        assert!(store.revoke(&[2u8; 32]).await.unwrap().is_some());
        assert!(store.get(&[2u8; 32], now).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sweep_reclaims_only_expired() {
        let store = InMemorySessionStore::new();
        let now = Utc::now();
        store
            .insert([3u8; 32], operator_session(now, 10))
            .await
            .unwrap();
        store
            .insert([4u8; 32], operator_session(now, 600))
            .await
            .unwrap();

        let swept = store
            .sweep_expired(now + Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(swept, 1);
        assert!(
            store
                .get(&[4u8; 32], now + Duration::seconds(60))
                .await
                .unwrap()
                .is_some()
        );
    }
}
