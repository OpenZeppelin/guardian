use crate::auth::Auth;
use crate::error::{ClientError, ClientResult};
use crate::keystore::Signer;
use crate::proto::guardian_client::GuardianClient as GuardianGrpcClient;
use crate::proto::{
    AbandonDeltaCandidateRequest, AbandonDeltaCandidateResponse, AuthConfig, ConfigureRequest,
    ConfigureResponse, GetAccountByKeyCommitmentRequest, GetAccountByKeyCommitmentResponse,
    GetDeltaProposalRequest, GetDeltaProposalResponse, GetDeltaProposalsRequest,
    GetDeltaProposalsResponse, GetDeltaRequest, GetDeltaResponse, GetDeltaSinceRequest,
    GetDeltaSinceResponse, GetPubkeyRequest, GetStateRequest, GetStateResponse,
    ProposalSignature as ProtoProposalSignature, PushDeltaProposalRequest,
    PushDeltaProposalResponse, PushDeltaRequest, PushDeltaResponse, SignDeltaProposalRequest,
    SignDeltaProposalResponse,
};
use chrono::Utc;
use guardian_shared::ProposalSignature as JsonProposalSignature;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::FromHex;
use guardian_shared::lookup_auth_message::LookupAuthMessage;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

/// Bounded retry policy for replay-classified rejections
/// (`authentication_replay`): the request was correctly signed but lost the
/// server's per-signer timestamp CAS to a concurrent request. Each attempt
/// re-mints the timestamp and re-signs the identical payload. Mirrors the TS
/// client's policy; terminal authentication failures are never retried.
const REPLAY_RETRY_LIMIT: u32 = 2;
const REPLAY_RETRY_BACKOFF: Duration = Duration::from_millis(50);

/// A client for interacting with Guardian servers.
///
/// `GuardianClient` provides methods for managing off-chain account state, including:
/// - Account configuration
/// - Delta (state change) management
/// - Multi-party proposal workflows
///
/// All methods that interact with account data require authentication via a configured signer.
pub struct GuardianClient {
    client: GuardianGrpcClient<Channel>,
    auth: Option<Auth>,
    signer: Option<Arc<dyn Signer>>,
    last_timestamp: AtomicI64,
}

impl GuardianClient {
    /// Creates a new client connected to the specified GUARDIAN server endpoint.
    ///
    /// # Arguments
    /// * `endpoint` - The gRPC endpoint URL (e.g., "http://localhost:50051")
    pub async fn connect(endpoint: impl Into<String>) -> ClientResult<Self> {
        let endpoint = endpoint.into();
        let client = GuardianGrpcClient::connect(endpoint).await?;
        Ok(Self {
            client,
            auth: None,
            signer: None,
            last_timestamp: AtomicI64::new(0),
        })
    }

    /// Monotonic timestamp for auth metadata: strictly increasing across
    /// calls within a single client instance so concurrent or rapid-fire
    /// requests never mint duplicate `x-timestamp` values. Mirrors the TS
    /// client's `nextTimestamp`.
    fn next_timestamp(&self) -> i64 {
        next_monotonic_timestamp(&self.last_timestamp, Utc::now().timestamp_millis())
    }

    /// Configures scheme-aware authentication for authenticated GUARDIAN requests.
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Configures the signer used for authenticated GUARDIAN requests.
    pub fn with_signer(mut self, signer: Arc<dyn Signer>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Returns the hex-encoded public key of the configured auth or signer.
    pub fn auth_pubkey_hex(&self) -> Result<String, ClientError> {
        self.auth
            .as_ref()
            .map(|auth| auth.public_key_hex())
            .or_else(|| self.signer.as_ref().map(|signer| signer.public_key_hex()))
            .ok_or_else(|| {
                ClientError::InvalidResponse("GUARDIAN client has no signer configured".to_string())
            })
    }

    /// Returns the hex-encoded public key of the configured signer, if any.
    pub fn signer_pubkey_hex(&self) -> Result<String, ClientError> {
        self.auth_pubkey_hex()
    }

    fn add_auth_metadata(
        &self,
        request: &mut tonic::Request<impl prost::Message + std::fmt::Debug>,
        account_id: &AccountId,
    ) -> ClientResult<()> {
        let request_payload = AuthRequestPayload::from_protobuf_message(request.get_ref());
        let timestamp = self.next_timestamp();

        let (pubkey_hex, signature_hex) = if let Some(auth) = &self.auth {
            let pubkey_hex = auth.public_key_hex();
            let signature_hex = auth.sign_request_message(account_id, timestamp, request_payload);
            (pubkey_hex, signature_hex)
        } else if let Some(signer) = &self.signer {
            let pubkey_hex = signer.public_key_hex();
            let digest = AuthRequestMessage::new(*account_id, timestamp, request_payload).to_word();
            let signature_hex = signer.sign_word_hex(digest);
            (pubkey_hex, signature_hex)
        } else {
            return Ok(());
        };

        attach_auth_headers(request, &pubkey_hex, &signature_hex, timestamp)
    }

    /// Attach lookup-bound auth metadata to a `GetAccountByKeyCommitment`
    /// request. Account-less because lookup is the operation that discovers
    /// the account ID, and signs the dedicated `LookupAuthMessage` digest —
    /// domain-separated from `AuthRequestMessage` by construction.
    ///
    /// The server derives the public key from the signature itself (Falcon
    /// embeds it; ECDSA recovers it). The `x-pubkey` header is sent for
    /// API consistency but ignored by the lookup verification path.
    fn add_lookup_auth_metadata<T: prost::Message + std::fmt::Debug>(
        &self,
        request: &mut tonic::Request<T>,
        key_commitment: Word,
    ) -> ClientResult<()> {
        let timestamp = self.next_timestamp();
        let digest = LookupAuthMessage::new(timestamp, key_commitment).to_word();

        let (pubkey_hex, signature_hex) = if let Some(auth) = &self.auth {
            (auth.public_key_hex(), auth.sign_word_hex(digest))
        } else if let Some(signer) = &self.signer {
            (signer.public_key_hex(), signer.sign_word_hex(digest))
        } else {
            return Err(ClientError::InvalidResponse(
                "GUARDIAN client has no signer configured".to_string(),
            ));
        };

        attach_auth_headers(request, &pubkey_hex, &signature_hex, timestamp)
    }

    /// Send an authenticated request, retrying only replay-classified
    /// rejections (`authentication_replay`) up to [`REPLAY_RETRY_LIMIT`]
    /// times. Every attempt rebuilds the request from the identical message
    /// with a fresh monotonic timestamp, recomputed digest, and fresh
    /// signature. Terminal failures (invalid signature, unauthorized signer,
    /// clock outside the skew window) propagate immediately.
    async fn send_with_replay_retry<Req, Resp, F>(
        &mut self,
        account_id: &AccountId,
        message: Req,
        mut send: F,
    ) -> ClientResult<Resp>
    where
        Req: prost::Message + Clone + std::fmt::Debug,
        F: AsyncFnMut(
            &mut GuardianGrpcClient<Channel>,
            tonic::Request<Req>,
        ) -> Result<tonic::Response<Resp>, tonic::Status>,
    {
        let mut retries_left = REPLAY_RETRY_LIMIT;
        loop {
            let mut request = tonic::Request::new(message.clone());
            self.add_auth_metadata(&mut request, account_id)?;
            match send(&mut self.client, request).await {
                Ok(response) => return Ok(response.into_inner()),
                Err(status) => {
                    let error = ClientError::from(status);
                    if retries_left == 0 || !error.is_replay_rejection() {
                        return Err(error);
                    }
                    retries_left -= 1;
                    tokio::time::sleep(REPLAY_RETRY_BACKOFF).await;
                }
            }
        }
    }

    /// Configure a new account
    ///
    /// # Arguments
    pub async fn configure(
        &mut self,
        account_id: &AccountId,
        auth: AuthConfig,
        initial_state: impl serde::Serialize,
    ) -> ClientResult<ConfigureResponse> {
        let initial_state_json = serde_json::to_string(&initial_state)?;

        let message = ConfigureRequest {
            account_id: account_id.to_string(),
            auth: Some(auth),
            initial_state: initial_state_json,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.configure(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Pushes a delta (state change) to the GUARDIAN server.
    ///
    /// This makes the delta canonical and triggers the canonicalization workflow.
    pub async fn push_delta(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
        prev_commitment: impl Into<String>,
        delta_payload: impl serde::Serialize,
    ) -> ClientResult<PushDeltaResponse> {
        let delta_payload_json = serde_json::to_string(&delta_payload)?;

        let message = PushDeltaRequest {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: prev_commitment.into(),
            delta_payload: delta_payload_json,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.push_delta(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves a specific delta by nonce.
    pub async fn get_delta(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
    ) -> ClientResult<GetDeltaResponse> {
        let message = GetDeltaRequest {
            account_id: account_id.to_string(),
            nonce,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.get_delta(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves all deltas starting from a given nonce.
    pub async fn get_delta_since(
        &mut self,
        account_id: &AccountId,
        from_nonce: u64,
    ) -> ClientResult<GetDeltaSinceResponse> {
        let message = GetDeltaSinceRequest {
            account_id: account_id.to_string(),
            from_nonce,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.get_delta_since(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Retrieves the current state for an account.
    pub async fn get_state(&mut self, account_id: &AccountId) -> ClientResult<GetStateResponse> {
        let message = GetStateRequest {
            account_id: account_id.to_string(),
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.get_state(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Look up the set of account IDs whose authorization set contains the
    /// given public-key commitment. Mirror of HTTP `GET /state/lookup`.
    ///
    /// Authentication is by proof-of-possession: the configured signer (or
    /// `Auth`) must hold the private key behind `key_commitment`. The server
    /// rejects any caller whose pubkey does not derive to the queried
    /// commitment, so this method MUST be called with a signer whose
    /// `commitment_hex` matches `key_commitment`.
    ///
    /// Returns an empty list if no account authorizes the commitment; the
    /// server intentionally does NOT distinguish that case from "wrong key"
    /// at the protocol level.
    pub async fn lookup_account_by_key_commitment(
        &mut self,
        key_commitment: &str,
    ) -> ClientResult<GetAccountByKeyCommitmentResponse> {
        let key_commitment_word = Word::from_hex(key_commitment).map_err(|e| {
            ClientError::InvalidResponse(format!("Invalid key_commitment hex: {e}"))
        })?;

        let mut request = tonic::Request::new(GetAccountByKeyCommitmentRequest {
            key_commitment: key_commitment.to_string(),
        });

        self.add_lookup_auth_metadata(&mut request, key_commitment_word)?;

        let response = self.client.get_account_by_key_commitment(request).await?;
        Ok(response.into_inner())
    }

    /// Retrieves the GUARDIAN server's public key commitment (and optionally the raw public key).
    pub async fn get_pubkey(
        &mut self,
        scheme: Option<&str>,
    ) -> ClientResult<(String, Option<String>)> {
        let request = tonic::Request::new(GetPubkeyRequest {
            scheme: scheme.map(|s| s.to_string()),
        });
        let response = self.client.get_pubkey(request).await?;
        let inner = response.into_inner();
        Ok((inner.pubkey, inner.raw_pubkey))
    }

    /// Push a delta proposal
    pub async fn push_delta_proposal(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
        delta_payload: impl serde::Serialize,
    ) -> ClientResult<PushDeltaProposalResponse> {
        let delta_payload_json = serde_json::to_string(&delta_payload)?;

        let message = PushDeltaProposalRequest {
            account_id: account_id.to_string(),
            nonce,
            delta_payload: delta_payload_json,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.push_delta_proposal(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Get all delta proposals for an account
    pub async fn get_delta_proposals(
        &mut self,
        account_id: &AccountId,
    ) -> ClientResult<GetDeltaProposalsResponse> {
        let message = GetDeltaProposalsRequest {
            account_id: account_id.to_string(),
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.get_delta_proposals(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Get a specific delta proposal for an account by commitment.
    pub async fn get_delta_proposal(
        &mut self,
        account_id: &AccountId,
        commitment: impl Into<String>,
    ) -> ClientResult<GetDeltaProposalResponse> {
        let message = GetDeltaProposalRequest {
            account_id: account_id.to_string(),
            commitment: commitment.into(),
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.get_delta_proposal(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Sign a delta proposal
    pub async fn sign_delta_proposal(
        &mut self,
        account_id: &AccountId,
        commitment: impl Into<String>,
        signature: JsonProposalSignature,
    ) -> ClientResult<SignDeltaProposalResponse> {
        let proto_signature = Some(proto_signature_from_json(&signature));

        let message = SignDeltaProposalRequest {
            account_id: account_id.to_string(),
            commitment: commitment.into(),
            signature: proto_signature,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.sign_delta_proposal(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }

    /// Request abandonment of a pending canonicalization candidate whose
    /// transaction will never land on-chain (issue #319).
    ///
    /// The request records an abandon *intent*: the delta stays a
    /// candidate — the account stays locked — until the guardian's
    /// canonicalization worker confirms over the abandon quarantine that
    /// the transaction did not land, then discards the delta as
    /// `client_abandoned` and releases the account (typically well under
    /// a minute). Poll `get_delta` for the resolution; the returned
    /// `state` is `"pending"` or, when already resolved, `"abandoned"`.
    ///
    /// `nonce` pins the exact candidate (learned from the delta feed).
    /// The server refuses with `GUARDIAN_CANDIDATE_LANDED` when the
    /// transaction actually landed. Retries are idempotent and preserve
    /// the original request timestamp.
    pub async fn abandon_candidate(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
    ) -> ClientResult<AbandonDeltaCandidateResponse> {
        let message = AbandonDeltaCandidateRequest {
            account_id: account_id.to_string(),
            nonce,
        };

        let inner = self
            .send_with_replay_retry(account_id, message, async |client, request| {
                client.abandon_delta_candidate(request).await
            })
            .await?;

        if !inner.success {
            return Err(ClientError::ServerError(inner.message.clone()));
        }

        Ok(inner)
    }
}

/// `max(now_ms, last + 1)`: the wall clock when it is ahead, otherwise one
/// past the previously minted value. Shared by all auth paths of one client
/// instance; the caller supplies the clock so the arithmetic stays
/// deterministic under test.
fn next_monotonic_timestamp(last_timestamp: &AtomicI64, now_ms: i64) -> i64 {
    let mut previous = last_timestamp.load(Ordering::SeqCst);
    loop {
        let next = previous.saturating_add(1).max(now_ms);
        match last_timestamp.compare_exchange_weak(
            previous,
            next,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return next,
            Err(actual) => previous = actual,
        }
    }
}

fn attach_auth_headers<T: prost::Message>(
    request: &mut tonic::Request<T>,
    pubkey_hex: &str,
    signature_hex: &str,
    timestamp: i64,
) -> ClientResult<()> {
    let pubkey_metadata = MetadataValue::try_from(pubkey_hex)
        .map_err(|e| ClientError::InvalidResponse(format!("Invalid pubkey: {e}")))?;
    let signature_metadata = MetadataValue::try_from(signature_hex)
        .map_err(|e| ClientError::InvalidResponse(format!("Invalid signature: {e}")))?;
    let timestamp_metadata = MetadataValue::try_from(timestamp.to_string())
        .map_err(|e| ClientError::InvalidResponse(format!("Invalid timestamp: {e}")))?;

    let metadata = request.metadata_mut();
    metadata.insert("x-pubkey", pubkey_metadata);
    metadata.insert("x-signature", signature_metadata);
    metadata.insert("x-timestamp", timestamp_metadata);
    Ok(())
}

fn proto_signature_from_json(signature: &JsonProposalSignature) -> ProtoProposalSignature {
    match signature {
        JsonProposalSignature::Falcon { signature } => ProtoProposalSignature {
            scheme: "falcon".to_string(),
            signature: signature.clone(),
            public_key: None,
        },
        JsonProposalSignature::Ecdsa {
            signature,
            public_key,
        } => ProtoProposalSignature {
            scheme: "ecdsa".to_string(),
            signature: signature.clone(),
            public_key: public_key.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_timestamp_follows_the_clock_when_ahead() {
        let last = AtomicI64::new(0);
        assert_eq!(next_monotonic_timestamp(&last, 1_000), 1_000);
        assert_eq!(next_monotonic_timestamp(&last, 2_000), 2_000);
    }

    #[test]
    fn monotonic_timestamp_never_repeats_within_one_instance() {
        let last = AtomicI64::new(0);
        let first = next_monotonic_timestamp(&last, 1_000);
        let second = next_monotonic_timestamp(&last, 1_000);
        let third = next_monotonic_timestamp(&last, 1_000);
        assert_eq!((first, second, third), (1_000, 1_001, 1_002));
    }

    #[test]
    fn monotonic_timestamp_survives_a_clock_step_backwards() {
        let last = AtomicI64::new(0);
        assert_eq!(next_monotonic_timestamp(&last, 5_000), 5_000);
        assert_eq!(next_monotonic_timestamp(&last, 4_000), 5_001);
    }

    #[test]
    fn concurrent_timestamps_are_unique() {
        let last = Arc::new(AtomicI64::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let last = last.clone();
                std::thread::spawn(move || {
                    (0..100)
                        .map(|_| next_monotonic_timestamp(&last, 1_000))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut minted: Vec<i64> = handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect();
        let count = minted.len();
        minted.sort_unstable();
        minted.dedup();
        assert_eq!(
            minted.len(),
            count,
            "timestamps must be unique across threads"
        );
    }
}
