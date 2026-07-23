use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::cipher::StorageCipher;
use super::envelope::RecordAad;
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::state_object::StateObject;
use crate::storage::{
    AbandonIntent, AccountDeltaCursor, AccountProposalCursor, CandidatePromotion,
    CandidateSubmission, CanonicalWrite, DeltaStatusCounts, DeltaStatusKind, GlobalDeltaCursor,
    GlobalDeltaRow, GlobalProposalCursor, LeaseFence, PromoteWrite, ProposalRecord, StorageBackend,
    StorageType,
};
use crate::utils::normalize_commitment_hex;

/// Canonical commitment form for proposal AAD, so the value bound at write time
/// matches what each read path reconstructs regardless of `0x` prefixing or the
/// backend's on-disk normalization.
fn canonical_commitment(commitment: &str) -> String {
    normalize_commitment_hex(commitment).unwrap_or_else(|_| commitment.to_string())
}

/// Wraps a concrete [`StorageBackend`], encrypting sensitive payloads on write
/// and decrypting them on read so callers above the storage boundary are
/// unchanged. Routing/index fields are left untouched.
pub(crate) struct EncryptedStorage {
    inner: Arc<dyn StorageBackend>,
    cipher: Arc<dyn StorageCipher>,
}

impl EncryptedStorage {
    pub(crate) fn new(inner: Arc<dyn StorageBackend>, cipher: Arc<dyn StorageCipher>) -> Self {
        Self { inner, cipher }
    }

    fn encrypt_state(&self, state: &StateObject) -> Result<StateObject, String> {
        let aad = RecordAad::State {
            account_id: &state.account_id,
        };
        let payload = self
            .cipher
            .encrypt(&aad, &state.state_json)
            .map_err(|e| e.to_string())?;
        Ok(StateObject {
            state_json: payload,
            ..state.clone()
        })
    }

    fn decrypt_state(&self, mut state: StateObject) -> Result<StateObject, String> {
        let payload = {
            let aad = RecordAad::State {
                account_id: &state.account_id,
            };
            self.cipher
                .decrypt(&aad, &state.state_json)
                .map_err(|e| e.to_string())?
        };
        state.state_json = payload;
        Ok(state)
    }

    fn encrypt_delta(&self, delta: &DeltaObject) -> Result<DeltaObject, String> {
        let aad = RecordAad::Delta {
            account_id: &delta.account_id,
            nonce: delta.nonce,
        };
        let payload = self
            .cipher
            .encrypt(&aad, &delta.delta_payload)
            .map_err(|e| e.to_string())?;
        Ok(DeltaObject {
            delta_payload: payload,
            ..delta.clone()
        })
    }

    fn decrypt_delta(&self, mut delta: DeltaObject) -> Result<DeltaObject, String> {
        let payload = {
            let aad = RecordAad::Delta {
                account_id: &delta.account_id,
                nonce: delta.nonce,
            };
            self.cipher
                .decrypt(&aad, &delta.delta_payload)
                .map_err(|e| e.to_string())?
        };
        delta.delta_payload = payload;
        Ok(delta)
    }

    fn encrypt_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<DeltaObject, String> {
        let commitment = canonical_commitment(commitment);
        let aad = RecordAad::Proposal {
            account_id: &proposal.account_id,
            commitment: &commitment,
        };
        let payload = self
            .cipher
            .encrypt(&aad, &proposal.delta_payload)
            .map_err(|e| e.to_string())?;
        Ok(DeltaObject {
            delta_payload: payload,
            ..proposal.clone()
        })
    }

    fn decrypt_proposal(
        &self,
        account_id: &str,
        commitment: &str,
        mut proposal: DeltaObject,
    ) -> Result<DeltaObject, String> {
        let commitment = canonical_commitment(commitment);
        let payload = {
            let aad = RecordAad::Proposal {
                account_id,
                commitment: &commitment,
            };
            self.cipher
                .decrypt(&aad, &proposal.delta_payload)
                .map_err(|e| e.to_string())?
        };
        proposal.delta_payload = payload;
        Ok(proposal)
    }

    fn decrypt_proposal_record(
        &self,
        mut record: ProposalRecord,
    ) -> Result<ProposalRecord, String> {
        record.proposal =
            self.decrypt_proposal(&record.account_id, &record.commitment, record.proposal)?;
        Ok(record)
    }
}

#[async_trait]
impl StorageBackend for EncryptedStorage {
    fn kind(&self) -> StorageType {
        self.inner.kind()
    }

    async fn submit_state(&self, state: &StateObject) -> Result<(), String> {
        self.inner.submit_state(&self.encrypt_state(state)?).await
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        self.inner.submit_delta(&self.encrypt_delta(delta)?).await
    }

    async fn pull_state(&self, account_id: &str) -> Result<StateObject, String> {
        let state = self.inner.pull_state(account_id).await?;
        self.decrypt_state(state)
    }

    async fn pull_states_batch(
        &self,
        account_ids: &[&str],
    ) -> Result<HashMap<String, StateObject>, String> {
        let states = self.inner.pull_states_batch(account_ids).await?;
        states
            .into_iter()
            .map(|(id, state)| self.decrypt_state(state).map(|state| (id, state)))
            .collect()
    }

    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String> {
        let delta = self.inner.pull_delta(account_id, nonce).await?;
        self.decrypt_delta(delta)
    }

    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        self.inner
            .pull_deltas_after(account_id, from_nonce)
            .await?
            .into_iter()
            .map(|delta| self.decrypt_delta(delta))
            .collect()
    }

    // Forwarded explicitly: the trait default would route through
    // `pull_deltas_after` and decrypt the full history just to keep the
    // candidates — the store-side filter must survive this layer.
    async fn pull_candidate_deltas(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        self.inner
            .pull_candidate_deltas(account_id)
            .await?
            .into_iter()
            .map(|delta| self.decrypt_delta(delta))
            .collect()
    }

    async fn pull_retained_deltas(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        self.inner
            .pull_retained_deltas(account_id)
            .await?
            .into_iter()
            .map(|delta| self.decrypt_delta(delta))
            .collect()
    }

    async fn list_accounts_with_retained_deltas(&self) -> Result<Vec<String>, String> {
        self.inner.list_accounts_with_retained_deltas().await
    }

    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        self.inner
            .submit_delta_proposal(commitment, &self.encrypt_proposal(commitment, proposal)?)
            .await
    }

    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String> {
        let proposal = self
            .inner
            .pull_delta_proposal(account_id, commitment)
            .await?;
        self.decrypt_proposal(account_id, commitment, proposal)
    }

    async fn pull_all_delta_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String> {
        self.inner
            .pull_all_delta_proposals(account_id)
            .await?
            .into_iter()
            .map(|record| self.decrypt_proposal_record(record))
            .collect()
    }

    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        self.inner
            .update_delta_proposal(commitment, &self.encrypt_proposal(commitment, proposal)?)
            .await
    }

    async fn delete_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<(), String> {
        self.inner
            .delete_delta_proposal(account_id, commitment)
            .await
    }

    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String> {
        self.inner.delete_delta(account_id, nonce).await
    }

    async fn request_candidate_abandon(
        &self,
        account_id: &str,
        nonce: u64,
        now: &str,
    ) -> Result<AbandonIntent, String> {
        self.inner
            .request_candidate_abandon(account_id, nonce, now)
            .await
    }

    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String> {
        self.inner
            .update_delta_status(account_id, nonce, status)
            .await
    }

    async fn submit_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        delta: &DeltaObject,
        now: &str,
    ) -> Result<CandidateSubmission, String> {
        self.inner
            .submit_candidate(metadata, &self.encrypt_delta(delta)?, now)
            .await
    }

    async fn promote_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        promotion: CandidatePromotion,
    ) -> Result<PromoteWrite, String> {
        let promotion = CandidatePromotion {
            state: self.encrypt_state(&promotion.state)?,
            delta: self.encrypt_delta(&promotion.delta)?,
            ..promotion
        };
        self.inner.promote_candidate(metadata, promotion).await
    }

    async fn discard_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        account_id: &str,
        nonce: u64,
        kind: DeltaStatusKind,
        now: &str,
        fence: Option<&LeaseFence>,
    ) -> Result<CanonicalWrite, String> {
        self.inner
            .discard_candidate(metadata, account_id, nonce, kind, now, fence)
            .await
    }

    async fn update_candidate_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
        fence: Option<&LeaseFence>,
    ) -> Result<CanonicalWrite, String> {
        self.inner
            .update_candidate_status(account_id, nonce, status, fence)
            .await
    }

    async fn list_account_deltas_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String> {
        self.inner
            .list_account_deltas_paged(account_id, limit, cursor)
            .await?
            .into_iter()
            .map(|delta| self.decrypt_delta(delta))
            .collect()
    }

    async fn list_account_proposals_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        self.inner
            .list_account_proposals_paged(account_id, limit, cursor)
            .await?
            .into_iter()
            .map(|record| self.decrypt_proposal_record(record))
            .collect()
    }

    async fn list_global_deltas_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalDeltaCursor>,
        status_filter: Option<Vec<DeltaStatusKind>>,
    ) -> Result<Vec<GlobalDeltaRow>, String> {
        self.inner
            .list_global_deltas_paged(limit, cursor, status_filter)
            .await?
            .into_iter()
            .map(|row| {
                Ok(GlobalDeltaRow {
                    account_id: row.account_id,
                    delta: self.decrypt_delta(row.delta)?,
                })
            })
            .collect()
    }

    async fn list_global_proposals_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        self.inner
            .list_global_proposals_paged(limit, cursor)
            .await?
            .into_iter()
            .map(|record| self.decrypt_proposal_record(record))
            .collect()
    }

    async fn count_deltas_by_status(&self) -> Result<DeltaStatusCounts, String> {
        self.inner.count_deltas_by_status().await
    }

    async fn count_in_flight_proposals(&self) -> Result<u64, String> {
        self.inner.count_in_flight_proposals().await
    }

    async fn latest_activity_timestamp(&self) -> Result<Option<DateTime<Utc>>, String> {
        self.inner.latest_activity_timestamp().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::encryption::cipher::Aes256GcmCipher;
    use crate::storage::encryption::key_provider::{InMemoryKeyProvider, StorageKeyProvider};
    use crate::storage::encryption::marker::{EncryptionMarker, MarkerStore, apply_startup_guard};
    use crate::storage::filesystem::FilesystemService;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use serde_json::json;

    fn provider_with(byte: u8, kid: &str) -> Arc<dyn StorageKeyProvider> {
        let key = BASE64.encode([byte; 32]);
        Arc::new(InMemoryKeyProvider::from_dev_key(&key, kid).unwrap())
    }

    fn provider() -> Arc<dyn StorageKeyProvider> {
        provider_with(3, "k1")
    }

    async fn fs_backend() -> (tempfile::TempDir, FilesystemService) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FilesystemService::new(dir.path().to_path_buf())
            .await
            .unwrap();
        (dir, fs)
    }

    fn state(account: &str, secret: &str) -> StateObject {
        StateObject {
            account_id: account.to_string(),
            state_json: json!({ "secret": secret }),
            ..Default::default()
        }
    }

    fn encrypted(inner: Arc<dyn StorageBackend>) -> EncryptedStorage {
        EncryptedStorage::new(inner, Arc::new(Aes256GcmCipher::new(provider())))
    }

    #[tokio::test]
    async fn state_and_delta_encrypted_at_rest_and_roundtrip() {
        let (_dir, fs) = fs_backend().await;
        let inner: Arc<dyn StorageBackend> = Arc::new(fs);
        let enc = encrypted(inner.clone());

        enc.submit_state(&state("acct1", "top-secret"))
            .await
            .unwrap();
        let raw = inner.pull_state("acct1").await.unwrap();
        assert!(
            raw.state_json.get("ct").is_some(),
            "payload must be an envelope"
        );
        assert!(!raw.state_json.to_string().contains("top-secret"));
        assert_eq!(raw.account_id, "acct1");
        assert_eq!(
            enc.pull_state("acct1").await.unwrap().state_json,
            json!({ "secret": "top-secret" })
        );

        let delta = DeltaObject {
            account_id: "acct1".to_string(),
            nonce: 1,
            delta_payload: json!({ "move": 7 }),
            ..Default::default()
        };
        enc.submit_delta(&delta).await.unwrap();
        let raw_delta = inner.pull_delta("acct1", 1).await.unwrap();
        assert!(raw_delta.delta_payload.get("ct").is_some());
        assert_eq!(
            enc.pull_delta("acct1", 1).await.unwrap().delta_payload,
            json!({ "move": 7 })
        );
    }

    #[tokio::test]
    async fn pull_candidate_deltas_forwards_the_filter_and_decrypts() {
        let (_dir, fs) = fs_backend().await;
        let inner: Arc<dyn StorageBackend> = Arc::new(fs);
        let enc = encrypted(inner.clone());

        let canonical = DeltaObject {
            account_id: "acct1".to_string(),
            nonce: 1,
            delta_payload: json!({ "move": 1 }),
            status: DeltaStatus::canonical("2024-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        let candidate = DeltaObject {
            account_id: "acct1".to_string(),
            nonce: 2,
            delta_payload: json!({ "move": 2 }),
            status: DeltaStatus::candidate("2024-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        enc.submit_delta(&canonical).await.unwrap();
        enc.submit_delta(&candidate).await.unwrap();

        let candidates = enc.pull_candidate_deltas("acct1").await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].nonce, 2);
        assert_eq!(candidates[0].delta_payload, json!({ "move": 2 }));
    }

    #[tokio::test]
    async fn proposal_encrypted_at_rest_and_roundtrip() {
        let (_dir, fs) = fs_backend().await;
        let inner: Arc<dyn StorageBackend> = Arc::new(fs);
        let enc = encrypted(inner.clone());

        let proposal = DeltaObject {
            account_id: "acct1".to_string(),
            nonce: 2,
            delta_payload: json!({ "proposed": true }),
            ..Default::default()
        };
        enc.submit_delta_proposal("0xABC123", &proposal)
            .await
            .unwrap();

        let raw = inner.pull_all_delta_proposals("acct1").await.unwrap();
        assert!(raw[0].proposal.delta_payload.get("ct").is_some());

        let decrypted = enc.pull_all_delta_proposals("acct1").await.unwrap();
        assert_eq!(decrypted.len(), 1);
        assert_eq!(
            decrypted[0].proposal.delta_payload,
            json!({ "proposed": true })
        );
    }

    #[tokio::test]
    async fn reads_match_plaintext_backend() {
        let (_d1, fs_plain) = fs_backend().await;
        let (_d2, fs_enc) = fs_backend().await;
        let plain: Arc<dyn StorageBackend> = Arc::new(fs_plain);
        let enc = encrypted(Arc::new(fs_enc));

        let s = state("acct1", "value");
        plain.submit_state(&s).await.unwrap();
        enc.submit_state(&s).await.unwrap();
        assert_eq!(
            plain.pull_state("acct1").await.unwrap().state_json,
            enc.pull_state("acct1").await.unwrap().state_json
        );
    }

    #[tokio::test]
    async fn marker_written_when_encrypting_empty_store_none_when_plaintext() {
        let (_dir, fs) = fs_backend().await;
        apply_startup_guard(&fs, None).await.unwrap();
        assert!(fs.read_encryption_marker().await.unwrap().is_none());

        let provider = provider();
        apply_startup_guard(&fs, Some(provider.as_ref()))
            .await
            .unwrap();
        assert!(fs.read_encryption_marker().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn encrypting_nonempty_unmarked_store_fails() {
        let (_dir, fs) = fs_backend().await;
        fs.submit_state(&state("acct1", "value")).await.unwrap();
        let provider = provider();
        assert!(
            apply_startup_guard(&fs, Some(provider.as_ref()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn plaintext_over_marked_store_fails() {
        let (_dir, fs) = fs_backend().await;
        let provider = provider();
        apply_startup_guard(&fs, Some(provider.as_ref()))
            .await
            .unwrap();
        assert!(apply_startup_guard(&fs, None).await.is_err());
    }

    #[tokio::test]
    async fn provider_missing_marker_kid_fails() {
        let (_dir, fs) = fs_backend().await;
        apply_startup_guard(&fs, Some(provider().as_ref()))
            .await
            .unwrap();
        let other = provider_with(9, "k2");
        assert!(
            apply_startup_guard(&fs, Some(other.as_ref()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn unsupported_marker_version_fails_startup() {
        let (_dir, fs) = fs_backend().await;
        let marker = EncryptionMarker {
            scheme_version: 99,
            init_kid: "k1".to_string(),
        };
        fs.write_encryption_marker(&marker).await.unwrap();
        assert!(
            apply_startup_guard(&fs, Some(provider().as_ref()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn aggregate_proposal_reads_fail_closed_on_corrupt_file() {
        let (dir, fs) = fs_backend().await;

        let proposal = DeltaObject {
            account_id: "acct1".to_string(),
            nonce: 1,
            delta_payload: json!({ "proposed": true }),
            ..Default::default()
        };
        fs.submit_delta_proposal("0xABC123", &proposal)
            .await
            .unwrap();

        let proposals_dir = dir.path().join("acct1").join("proposals");
        let mut entries = tokio::fs::read_dir(&proposals_dir).await.unwrap();
        let entry = entries.next_entry().await.unwrap().unwrap();
        tokio::fs::write(entry.path(), b"{ not valid json")
            .await
            .unwrap();

        assert!(fs.pull_all_delta_proposals("acct1").await.is_err());
        assert!(fs.pull_pending_proposals("acct1").await.is_err());
    }
}
