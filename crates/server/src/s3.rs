use aws_sdk_s3::Client as S3Client;
use aws_sdk_sts::Client as StsClient;
use serde::{Deserialize, Serialize};
use std::env;

/// S3 configuration loaded from environment variables
#[derive(Clone, Debug)]
pub struct S3Config {
    pub app_bucket_prefix: String,
    pub read_bucket_prefix: String,
}

impl S3Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self, String> {
        let app_bucket_prefix =
            env::var("PSM_APP_BUCKET_PREFIX").unwrap_or_else(|_| "psm-app".to_string());

        let read_bucket_prefix =
            env::var("PSM_READ_BUCKET_PREFIX").unwrap_or_else(|_| "psm-read".to_string());

        Ok(Self {
            app_bucket_prefix,
            read_bucket_prefix,
        })
    }
}

/// Account state object stored in S3
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountState {
    pub account_id: String,
    pub state_json: serde_json::Value,
    pub commitment: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Delta object stored in S3
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeltaObject {
    pub account_id: String,
    pub nonce: u64,
    pub prev_commitment: String,
    pub delta_hash: String,
    pub delta_payload: serde_json::Value,
    pub ack_sig: String,
    pub publisher_pubkey: String,
    pub publisher_sig: String,
    pub candidate_at: String,
    pub canonical_at: Option<String>,
    pub discarded_at: Option<String>,
}

/// S3 Service for managing states and deltas
pub struct S3Service {
    s3_client: S3Client,
    sts_client: StsClient,
    config: S3Config,
}

impl S3Service {
    /// Create a new S3Service with AWS configuration
    pub async fn new(config: S3Config) -> Result<Self, String> {
        let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let s3_client = S3Client::new(&aws_config);
        let sts_client = StsClient::new(&aws_config);

        Ok(Self {
            s3_client,
            sts_client,
            config,
        })
    }

    /// Validate AWS credentials by calling STS GetCallerIdentity
    pub async fn validate_credentials(&self) -> Result<String, String> {
        let identity = self
            .sts_client
            .get_caller_identity()
            .send()
            .await
            .map_err(|e| format!("Failed to validate credentials: {}", e))?;

        let account_id = identity
            .account()
            .ok_or_else(|| "No account ID returned".to_string())?;

        Ok(account_id.to_string())
    }

    /// Submit an account state to S3
    pub async fn submit_state(&self, state: &AccountState) -> Result<(), String> {
        let key = format!("{}/state.json", state.account_id);
        let body = serde_json::to_string(state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;

        self.s3_client
            .put_object()
            .bucket(&self.config.app_bucket_prefix)
            .key(&key)
            .body(body.into_bytes().into())
            .content_type("application/json")
            .send()
            .await
            .map_err(|e| format!("Failed to upload state: {}", e))?;

        Ok(())
    }

    /// Submit a delta to S3
    pub async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        let key = format!("{}/deltas/{}.json", delta.account_id, delta.nonce);
        let body = serde_json::to_string(delta)
            .map_err(|e| format!("Failed to serialize delta: {}", e))?;

        self.s3_client
            .put_object()
            .bucket(&self.config.app_bucket_prefix)
            .key(&key)
            .body(body.into_bytes().into())
            .content_type("application/json")
            .send()
            .await
            .map_err(|e| format!("Failed to upload delta: {}", e))?;

        Ok(())
    }

    /// Pull account state from S3
    pub async fn pull_state(&self, account_id: &str) -> Result<AccountState, String> {
        let key = format!("{}/state.json", account_id);

        let response = self
            .s3_client
            .get_object()
            .bucket(&self.config.app_bucket_prefix)
            .key(&key)
            .send()
            .await
            .map_err(|e| format!("Failed to get state: {}", e))?;

        let bytes = response
            .body
            .collect()
            .await
            .map_err(|e| format!("Failed to read state body: {}", e))?
            .into_bytes();

        let state: AccountState = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Failed to deserialize state: {}", e))?;

        Ok(state)
    }

    /// Pull a specific delta from S3
    pub async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String> {
        let key = format!("{}/deltas/{}.json", account_id, nonce);

        let response = self
            .s3_client
            .get_object()
            .bucket(&self.config.app_bucket_prefix)
            .key(&key)
            .send()
            .await
            .map_err(|e| format!("Failed to get delta: {}", e))?;

        let bytes = response
            .body
            .collect()
            .await
            .map_err(|e| format!("Failed to read delta body: {}", e))?
            .into_bytes();

        let delta: DeltaObject = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Failed to deserialize delta: {}", e))?;

        Ok(delta)
    }

    /// List all deltas for an account
    pub async fn list_deltas(&self, account_id: &str) -> Result<Vec<String>, String> {
        let prefix = format!("{}/deltas/", account_id);

        let response = self
            .s3_client
            .list_objects_v2()
            .bucket(&self.config.app_bucket_prefix)
            .prefix(&prefix)
            .send()
            .await
            .map_err(|e| format!("Failed to list deltas: {}", e))?;

        let keys = response
            .contents()
            .iter()
            .filter_map(|obj| obj.key().map(String::from))
            .collect();

        Ok(keys)
    }
}
