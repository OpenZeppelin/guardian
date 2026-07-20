use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::Word;
use tokio::sync::Mutex;

use crate::error::{GuardianError, Result};

/// Realm-specific data needed to match a submitted credential against a pending
/// challenge at verify time. Operator verification re-runs a Falcon signature
/// check over the signing digest; EVM verification recovers the signer from the
/// full original challenge fields.
#[derive(Clone, Debug)]
pub enum ChallengePayload {
    OperatorDigest(Word),
    EvmChallenge {
        address: String,
        nonce: String,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    },
}

impl ChallengePayload {
    /// JSONB representation persisted in `auth_challenges.payload`. `Word` is not
    /// directly serializable, so the operator digest is stored as canonical hex.
    pub fn to_value(&self) -> serde_json::Value {
        match self {
            ChallengePayload::OperatorDigest(word) => serde_json::json!({
                "kind": "operator_digest",
                "signing_digest": (*word).into_hex(),
            }),
            ChallengePayload::EvmChallenge {
                address,
                nonce,
                issued_at,
                expires_at,
            } => serde_json::json!({
                "kind": "evm_challenge",
                "address": address,
                "nonce": nonce,
                "issued_at": issued_at.to_rfc3339(),
                "expires_at": expires_at.to_rfc3339(),
            }),
        }
    }

    pub fn from_value(value: &serde_json::Value) -> Result<Self> {
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                GuardianError::StorageError("challenge payload missing kind".to_string())
            })?;
        match kind {
            "operator_digest" => {
                let hex = string_field(value, "signing_digest")?;
                let word = Word::from_hex(&hex).map_err(GuardianError::StorageError)?;
                Ok(ChallengePayload::OperatorDigest(word))
            }
            "evm_challenge" => Ok(ChallengePayload::EvmChallenge {
                address: string_field(value, "address")?,
                nonce: string_field(value, "nonce")?,
                issued_at: time_field(value, "issued_at")?,
                expires_at: time_field(value, "expires_at")?,
            }),
            other => Err(GuardianError::StorageError(format!(
                "unknown challenge payload kind: {other}"
            ))),
        }
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GuardianError::StorageError(format!("challenge payload missing {key}")))
}

fn time_field(value: &serde_json::Value, key: &str) -> Result<DateTime<Utc>> {
    let raw = string_field(value, key)?;
    DateTime::parse_from_rfc3339(&raw)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| {
            GuardianError::StorageError(format!("challenge payload {key} invalid: {error}"))
        })
}

#[derive(Clone, Debug)]
pub struct StoredChallenge {
    pub key: String,
    pub payload: ChallengePayload,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A realm-scoped store of pending login challenges grouped by principal
/// (operator commitment or EVM address). Verification matches a returned
/// credential against the active challenges in Rust, then claims the matched one
/// via [`ChallengeStore::consume`], which is single-use across replicas.
#[async_trait]
pub trait ChallengeStore: Send + Sync {
    async fn issue(
        &self,
        principal: &str,
        challenge: StoredChallenge,
        max_outstanding: usize,
        now: DateTime<Utc>,
    ) -> Result<()>;
    async fn active_for(&self, principal: &str, now: DateTime<Utc>)
    -> Result<Vec<StoredChallenge>>;
    async fn consume(&self, principal: &str, key: &str, now: DateTime<Utc>) -> Result<bool>;
    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<u64>;
}

#[derive(Clone, Default)]
pub struct InMemoryChallengeStore {
    challenges: Arc<Mutex<HashMap<String, Vec<StoredChallenge>>>>,
}

impl InMemoryChallengeStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ChallengeStore for InMemoryChallengeStore {
    async fn issue(
        &self,
        principal: &str,
        challenge: StoredChallenge,
        max_outstanding: usize,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut challenges = self.challenges.lock().await;
        let pending = challenges.entry(principal.to_string()).or_default();
        pending.retain(|challenge| challenge.expires_at > now);
        pending.push(challenge);
        if pending.len() > max_outstanding {
            pending.sort_by_key(|challenge| challenge.issued_at);
            let drain_len = pending.len() - max_outstanding;
            pending.drain(0..drain_len);
        }
        Ok(())
    }

    async fn active_for(
        &self,
        principal: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<StoredChallenge>> {
        let challenges = self.challenges.lock().await;
        Ok(challenges
            .get(principal)
            .map(|pending| {
                pending
                    .iter()
                    .filter(|challenge| challenge.expires_at > now)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn consume(&self, principal: &str, key: &str, now: DateTime<Utc>) -> Result<bool> {
        let mut challenges = self.challenges.lock().await;
        let Some(pending) = challenges.get_mut(principal) else {
            return Ok(false);
        };
        let matched = pending
            .iter()
            .position(|challenge| challenge.key == key && challenge.expires_at > now);
        let Some(index) = matched else {
            return Ok(false);
        };
        pending.remove(index);
        if pending.is_empty() {
            challenges.remove(principal);
        }
        Ok(true)
    }

    async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<u64> {
        let mut challenges = self.challenges.lock().await;
        let before: usize = challenges.values().map(Vec::len).sum();
        for pending in challenges.values_mut() {
            pending.retain(|challenge| challenge.expires_at > now);
        }
        challenges.retain(|_, pending| !pending.is_empty());
        let after: usize = challenges.values().map(Vec::len).sum();
        Ok((before - after) as u64)
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use chrono::Duration;

    fn challenge(key: &str, now: DateTime<Utc>, ttl_secs: i64) -> StoredChallenge {
        StoredChallenge {
            key: key.to_string(),
            payload: ChallengePayload::EvmChallenge {
                address: "0x1".to_string(),
                nonce: key.to_string(),
                issued_at: now,
                expires_at: now + Duration::seconds(ttl_secs),
            },
            issued_at: now,
            expires_at: now + Duration::seconds(ttl_secs),
        }
    }

    #[tokio::test]
    async fn consume_is_single_use() {
        let store = InMemoryChallengeStore::new();
        let now = Utc::now();
        store
            .issue("0xp", challenge("k1", now, 60), 8, now)
            .await
            .unwrap();

        assert!(store.consume("0xp", "k1", now).await.unwrap());
        assert!(!store.consume("0xp", "k1", now).await.unwrap());
    }

    #[tokio::test]
    async fn active_for_hides_expired() {
        let store = InMemoryChallengeStore::new();
        let now = Utc::now();
        store
            .issue("0xp", challenge("k1", now, 10), 8, now)
            .await
            .unwrap();

        assert_eq!(store.active_for("0xp", now).await.unwrap().len(), 1);
        assert!(
            store
                .active_for("0xp", now + Duration::seconds(11))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn issue_caps_outstanding_dropping_oldest() {
        let store = InMemoryChallengeStore::new();
        let now = Utc::now();
        for i in 0..5 {
            let issued = now + Duration::seconds(i);
            let mut c = challenge(&format!("k{i}"), now, 600);
            c.issued_at = issued;
            store.issue("0xp", c, 3, now).await.unwrap();
        }
        let active = store.active_for("0xp", now).await.unwrap();
        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|c| c.key != "k0" && c.key != "k1"));
    }

    #[tokio::test]
    async fn consume_unknown_principal_is_false() {
        let store = InMemoryChallengeStore::new();
        assert!(!store.consume("0xnope", "k1", Utc::now()).await.unwrap());
    }

    #[test]
    fn evm_payload_round_trips_through_json() {
        let payload = ChallengePayload::EvmChallenge {
            address: "0xabc".to_string(),
            nonce: "0xdead".to_string(),
            issued_at: "2026-06-23T00:00:00+00:00".parse().unwrap(),
            expires_at: "2026-06-23T00:05:00+00:00".parse().unwrap(),
        };
        let value = payload.to_value();
        let restored = ChallengePayload::from_value(&value).unwrap();
        match restored {
            ChallengePayload::EvmChallenge { address, nonce, .. } => {
                assert_eq!(address, "0xabc");
                assert_eq!(nonce, "0xdead");
            }
            _ => panic!("expected evm challenge"),
        }
    }

    #[test]
    fn unknown_payload_kind_is_rejected() {
        let value = serde_json::json!({ "kind": "nope" });
        assert!(ChallengePayload::from_value(&value).is_err());
    }
}
