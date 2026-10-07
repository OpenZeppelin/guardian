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
    /// Miden account session: a wallet-authorized P-256 delegated signer
    /// acting for `signer_commitment`. The Guardian ACK-key commitment and
    /// network the grant named are re-checked on every request; the origin is
    /// kept only to tell grants apart.
    Miden {
        signer_commitment: String,
        origin: String,
        guardian_commitment: String,
        network: String,
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
    /// Insert only if no row exists for `key`, active, revoked or expired but
    /// not yet swept. Returns whether the session was inserted. Used where the
    /// key is chosen by the client (Miden session keys), so a revoked session
    /// can never be revived by registering the same key again.
    async fn insert_new(&self, key: SessionKey, session: StoredSession) -> Result<bool>;
    async fn get(&self, key: &SessionKey, now: DateTime<Utc>) -> Result<Option<StoredSession>>;
    /// Whether a row exists for `key` that `get` no longer returns: `Some(true)`
    /// when it was revoked, `Some(false)` when it only expired, `None` when there
    /// is no row (never registered, or swept). Lets callers report why a
    /// session stopped working.
    async fn inactive_reason(&self, key: &SessionKey) -> Result<Option<bool>>;
    /// Revoke a session (logout), returning the prior session if present for
    /// logout-side logging. The cross-replica contract: once revoked, `get` MUST
    /// reject it on every replica until its natural expiry. Both
    /// implementations keep the revoked entry until expiry.
    async fn revoke(&self, key: &SessionKey) -> Result<Option<StoredSession>>;
    /// Revoke every active session whose subject contains `filter` (JSON
    /// containment, as Postgres `@>`) and that was issued at or before
    /// `issued_at_or_before`. Returns how many were revoked.
    async fn revoke_by_subject(
        &self,
        filter: &serde_json::Value,
        issued_at_or_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<u64>;
    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<u64>;
}

/// `filter` is contained in `value`: every key of a filter object is present
/// with a contained value; other values must be equal.
fn json_contains(value: &serde_json::Value, filter: &serde_json::Value) -> bool {
    match (value, filter) {
        (serde_json::Value::Object(value), serde_json::Value::Object(filter)) => filter
            .iter()
            .all(|(key, inner)| value.get(key).is_some_and(|v| json_contains(v, inner))),
        _ => value == filter,
    }
}

/// In-memory entry; `revoked` mirrors the Postgres `revoked_at` column.
#[derive(Clone)]
struct InMemoryEntry {
    session: StoredSession,
    revoked: bool,
}

#[derive(Clone, Default)]
pub struct InMemorySessionStore {
    sessions: Arc<Mutex<HashMap<SessionKey, InMemoryEntry>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn insert(&self, key: SessionKey, session: StoredSession) -> Result<()> {
        let entry = InMemoryEntry {
            session,
            revoked: false,
        };
        self.sessions.lock().await.insert(key, entry);
        Ok(())
    }

    async fn insert_new(&self, key: SessionKey, session: StoredSession) -> Result<bool> {
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&key) {
            return Ok(false);
        }
        sessions.insert(
            key,
            InMemoryEntry {
                session,
                revoked: false,
            },
        );
        Ok(true)
    }

    async fn get(&self, key: &SessionKey, now: DateTime<Utc>) -> Result<Option<StoredSession>> {
        Ok(self
            .sessions
            .lock()
            .await
            .get(key)
            .filter(|entry| !entry.revoked && entry.session.expires_at > now)
            .map(|entry| entry.session.clone()))
    }

    async fn inactive_reason(&self, key: &SessionKey) -> Result<Option<bool>> {
        Ok(self
            .sessions
            .lock()
            .await
            .get(key)
            .map(|entry| entry.revoked))
    }

    async fn revoke(&self, key: &SessionKey) -> Result<Option<StoredSession>> {
        let mut sessions = self.sessions.lock().await;
        Ok(sessions
            .get_mut(key)
            .filter(|entry| !entry.revoked)
            .map(|entry| {
                entry.revoked = true;
                entry.session.clone()
            }))
    }

    async fn revoke_by_subject(
        &self,
        filter: &serde_json::Value,
        issued_at_or_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        let mut sessions = self.sessions.lock().await;
        let mut revoked = 0;
        for entry in sessions.values_mut() {
            if entry.revoked
                || entry.session.expires_at <= now
                || entry.session.issued_at > issued_at_or_before
            {
                continue;
            }
            let subject = serde_json::to_value(&entry.session.subject).map_err(|error| {
                crate::error::GuardianError::StorageError(format!(
                    "session subject encode: {error}"
                ))
            })?;
            if json_contains(&subject, filter) {
                entry.revoked = true;
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<u64> {
        let mut sessions = self.sessions.lock().await;
        let before = sessions.len();
        sessions.retain(|_, entry| entry.session.expires_at > now);
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
