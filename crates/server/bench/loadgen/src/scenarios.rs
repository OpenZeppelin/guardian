use crate::config::{AuthScheme, RunConfig, Scenario, Transport};
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use chrono::Utc;
use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_protocol::Word;
use miden_protocol::account::delta::{AccountStorageDelta, AccountVaultDelta};
use miden_protocol::account::{AccountDelta, AccountId};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, SecretKey as EcdsaSecretKey,
};
use miden_protocol::crypto::dsa::falcon512_rpo::{
    PublicKey as FalconPublicKey, SecretKey as FalconSecretKey,
};
use miden_protocol::transaction::{InputNotes, OutputNotes, TransactionSummary};
use miden_protocol::utils::{Deserializable, Serializable};
use miden_protocol::{Felt, ZERO};
use private_state_manager_client::{
    Auth, AuthConfig, ClientError, EcdsaSigner, FalconRpoSigner, MidenEcdsaAuth,
    MidenFalconRpoAuth, PsmClient, ToJson, auth_config::AuthType,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct AccountSeed {
    pub account_id: AccountId,
    pub initial_state: Value,
    pub commitment: String,
    pub next_nonce: u64,
    pub cosigner_commitments: Vec<String>,
}

pub struct UserContext {
    user_id: usize,
    auth_scheme: AuthScheme,
    signer_commitment: String,
    psm_commitment: Word,
    signers_per_account: usize,
    auth: Option<Auth>,
    last_timestamp_ms: i64,
    pub client: LoadClient,
    pub accounts: Vec<AccountSeed>,
}

pub enum LoadClient {
    Grpc(PsmClient),
    Http(HttpClient),
}

pub struct HttpClient {
    client: reqwest::Client,
    endpoint: String,
}

#[derive(Debug, Deserialize)]
struct HttpPubkeyResponse {
    commitment: String,
}

#[derive(Debug, Deserialize)]
struct HttpPushDeltaResponse {
    #[serde(default)]
    new_commitment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HttpDeltaStatus {
    status: String,
}

#[derive(Debug, Deserialize)]
struct HttpGetDeltaResponse {
    #[serde(default)]
    status: Option<HttpDeltaStatus>,
    #[serde(default)]
    canonical_at: Option<String>,
    #[serde(default)]
    discarded_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencySummary {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunMetrics {
    pub total_ops: u64,
    pub success_ops: u64,
    pub failed_ops: u64,
    pub run_duration_ms: f64,
    pub ops_per_sec: f64,
    pub success_ops_per_sec: f64,
    pub latency: LatencySummary,
    pub error_samples: Vec<String>,
}

#[derive(Debug)]
struct WorkerMetrics {
    total_ops: u64,
    success_ops: u64,
    failed_ops: u64,
    latencies_ms: Vec<f64>,
    error_samples: Vec<String>,
}

enum DeltaStatusCheck {
    Pending,
    Terminal,
    NotFound,
}

impl UserContext {
    async fn configure_account(
        &mut self,
        account_id: &AccountId,
        cosigner_commitments: Vec<String>,
        initial_state: Value,
    ) -> Result<()> {
        match &mut self.client {
            LoadClient::Grpc(client) => client
                .configure(
                    account_id,
                    build_auth_config(cosigner_commitments, self.auth_scheme),
                    initial_state,
                )
                .await
                .map(|_| ())
                .map_err(Into::into),
            LoadClient::Http(_) => {
                let payload = serde_json::json!({
                    "account_id": account_id.to_string(),
                    "auth": build_http_auth(cosigner_commitments, self.auth_scheme),
                    "initial_state": initial_state,
                });
                let response = self
                    .http_request_with_auth(Method::POST, "/configure", account_id, Some(payload))
                    .await?;
                ensure_http_success(response, "configure").await?;
                Ok(())
            }
        }
    }

    async fn read_state(&mut self, account_id: &AccountId) -> Result<()> {
        match &mut self.client {
            LoadClient::Grpc(client) => client
                .get_state(account_id)
                .await
                .map(|_| ())
                .map_err(Into::into),
            LoadClient::Http(_) => {
                let path = format!("/state?account_id={account_id}");
                let response = self
                    .http_request_with_auth(Method::GET, &path, account_id, None)
                    .await?;
                ensure_http_success(response, "get_state").await?;
                Ok(())
            }
        }
    }

    async fn push_state(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
        prev_commitment: String,
        delta_payload: Value,
    ) -> Result<Option<String>> {
        match &mut self.client {
            LoadClient::Grpc(client) => {
                let response = client
                    .push_delta(account_id, nonce, prev_commitment, delta_payload)
                    .await?;
                let pushed_delta = response
                    .delta
                    .ok_or_else(|| anyhow!("push_delta returned no delta for nonce {nonce}"))?;
                if pushed_delta.new_commitment.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(pushed_delta.new_commitment))
                }
            }
            LoadClient::Http(_) => {
                let payload = serde_json::json!({
                    "account_id": account_id.to_string(),
                    "nonce": nonce,
                    "prev_commitment": prev_commitment,
                    "delta_payload": delta_payload,
                });
                let response = self
                    .http_request_with_auth(Method::POST, "/delta", account_id, Some(payload))
                    .await?;
                let json = parse_http_json(response, "push_delta").await?;
                let parsed: HttpPushDeltaResponse =
                    serde_json::from_value(json).context("failed to parse push_delta response")?;
                Ok(parsed.new_commitment.filter(|value| !value.is_empty()))
            }
        }
    }

    async fn get_delta_status(
        &mut self,
        account_id: &AccountId,
        nonce: u64,
    ) -> Result<DeltaStatusCheck> {
        match &mut self.client {
            LoadClient::Grpc(client) => match client.get_delta(account_id, nonce).await {
                Ok(response) => {
                    let delta = response
                        .delta
                        .ok_or_else(|| anyhow!("get_delta returned no delta for nonce {nonce}"))?;
                    if delta.canonical_at.is_some() || delta.discarded_at.is_some() {
                        Ok(DeltaStatusCheck::Terminal)
                    } else {
                        Ok(DeltaStatusCheck::Pending)
                    }
                }
                Err(error) => {
                    if is_delta_not_found(&error) {
                        Ok(DeltaStatusCheck::NotFound)
                    } else {
                        Err(error.into())
                    }
                }
            },
            LoadClient::Http(_) => {
                let path = format!("/delta?account_id={account_id}&nonce={nonce}");
                let response = self
                    .http_request_with_auth(Method::GET, &path, account_id, None)
                    .await?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(DeltaStatusCheck::NotFound);
                }
                let json = parse_http_json(response, "get_delta").await?;
                let parsed: HttpGetDeltaResponse =
                    serde_json::from_value(json).context("failed to parse get_delta response")?;
                if parsed.canonical_at.is_some() || parsed.discarded_at.is_some() {
                    return Ok(DeltaStatusCheck::Terminal);
                }
                if let Some(status) = parsed.status {
                    let status_lower = status.status.to_ascii_lowercase();
                    if status_lower == "canonical" || status_lower == "discarded" {
                        return Ok(DeltaStatusCheck::Terminal);
                    }
                }
                Ok(DeltaStatusCheck::Pending)
            }
        }
    }

    async fn http_request_with_auth(
        &mut self,
        method: Method,
        path: &str,
        account_id: &AccountId,
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let (http_client, endpoint) = match &self.client {
            LoadClient::Http(client) => (client.client.clone(), client.endpoint.clone()),
            LoadClient::Grpc(_) => {
                return Err(anyhow!(
                    "attempted HTTP request while gRPC transport is active"
                ));
            }
        };

        let timestamp = self.next_timestamp_ms();
        let auth = self
            .auth
            .as_ref()
            .ok_or_else(|| anyhow!("HTTP transport requires signer auth"))?;
        let signature = auth.sign_account_id_with_timestamp(account_id, timestamp);
        let pubkey = auth.public_key_hex();
        let url = format!("{endpoint}{path}");

        let mut request = http_client
            .request(method.clone(), url)
            .header("content-type", "application/json")
            .header("x-pubkey", pubkey)
            .header("x-signature", signature)
            .header("x-timestamp", timestamp.to_string());
        if let Some(payload) = body {
            request = request.json(&payload);
        }

        request
            .send()
            .await
            .with_context(|| format!("failed to send HTTP {method} request to {path}"))
    }

    fn next_timestamp_ms(&mut self) -> i64 {
        let now = Utc::now().timestamp_millis();
        let next = if now > self.last_timestamp_ms {
            now
        } else {
            self.last_timestamp_ms + 1
        };
        self.last_timestamp_ms = next;
        next
    }
}

fn build_http_auth(cosigner_commitments: Vec<String>, auth_scheme: AuthScheme) -> Value {
    match auth_scheme {
        AuthScheme::Falcon => serde_json::json!({
            "MidenFalconRpo": {
                "cosigner_commitments": cosigner_commitments
            }
        }),
        AuthScheme::Ecdsa => serde_json::json!({
            "MidenEcdsa": {
                "cosigner_commitments": cosigner_commitments
            }
        }),
    }
}

async fn parse_http_json(response: reqwest::Response, operation: &str) -> Result<Value> {
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read HTTP body for {operation}"))?;
    if !status.is_success() {
        return Err(anyhow!("{operation} failed with status {status}: {body}"));
    }
    if body.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&body)
            .with_context(|| format!("failed to parse JSON response for {operation}: {body}"))
    }
}

async fn ensure_http_success(response: reqwest::Response, operation: &str) -> Result<()> {
    let _ = parse_http_json(response, operation).await?;
    Ok(())
}

async fn fetch_psm_commitment(config: &RunConfig) -> Result<String> {
    match config.transport {
        Transport::Grpc => {
            let mut probe_client = PsmClient::connect(config.psm_endpoint.clone())
                .await
                .with_context(|| format!("failed to connect to {}", config.psm_endpoint))?;
            let (psm_commitment_hex, _) = probe_client
                .get_pubkey(None)
                .await
                .context("failed to read PSM commitment via get_pubkey")?;
            Ok(psm_commitment_hex)
        }
        Transport::Http => {
            let endpoint = config.psm_http_endpoint.trim_end_matches('/');
            let url = format!("{endpoint}/pubkey");
            let response = reqwest::Client::new()
                .get(url)
                .send()
                .await
                .context("failed to read PSM commitment via HTTP /pubkey")?;
            let json = parse_http_json(response, "get_pubkey").await?;
            let parsed: HttpPubkeyResponse =
                serde_json::from_value(json).context("failed to parse /pubkey response")?;
            Ok(parsed.commitment)
        }
    }
}

async fn build_client(config: &RunConfig, auth: Auth) -> Result<(LoadClient, Option<Auth>)> {
    match config.transport {
        Transport::Grpc => {
            let client = PsmClient::connect(config.psm_endpoint.clone())
                .await
                .with_context(|| format!("failed to connect to {}", config.psm_endpoint))?
                .with_auth(auth);
            Ok((LoadClient::Grpc(client), None))
        }
        Transport::Http => Ok((
            LoadClient::Http(HttpClient {
                client: reqwest::Client::new(),
                endpoint: config.psm_http_endpoint.trim_end_matches('/').to_string(),
            }),
            Some(auth),
        )),
    }
}

pub async fn seed_accounts(config: &RunConfig) -> Result<Vec<UserContext>> {
    let psm_commitment_hex = fetch_psm_commitment(config).await?;
    let psm_commitment_word = word_from_hex(&psm_commitment_hex)?;

    let users = usize::try_from(config.users).context("users value is too large")?;
    let accounts = usize::try_from(config.accounts).context("accounts value is too large")?;
    let signers_per_account =
        usize::try_from(config.signers_per_account).context("signers-per-account is too large")?;

    let mut user_contexts = Vec::with_capacity(users);
    for user_id in 0..users {
        let (auth, commitment) = match config.auth_scheme {
            AuthScheme::Falcon => {
                let signer = FalconRpoSigner::new(FalconSecretKey::new());
                let commitment =
                    commitment_hex_from_pubkey_hex(&signer.public_key_hex(), config.auth_scheme)?;
                (Auth::FalconRpoSigner(signer), commitment)
            }
            AuthScheme::Ecdsa => {
                let signer = EcdsaSigner::new(EcdsaSecretKey::new());
                let commitment =
                    commitment_hex_from_pubkey_hex(&signer.public_key_hex(), config.auth_scheme)?;
                (Auth::EcdsaSigner(signer), commitment)
            }
        };
        let (client, user_auth) = build_client(config, auth).await?;

        user_contexts.push(UserContext {
            user_id,
            auth_scheme: config.auth_scheme,
            signer_commitment: commitment,
            psm_commitment: psm_commitment_word,
            signers_per_account,
            auth: user_auth,
            last_timestamp_ms: 0,
            client,
            accounts: Vec::new(),
        });
    }

    for account_index in 0..accounts {
        let owner_idx = account_index % users;
        let owner = &mut user_contexts[owner_idx];
        let seed = rand::random::<[u8; 32]>();
        let account_seed = create_account_seed(
            &owner.signer_commitment,
            owner.signers_per_account,
            owner.psm_commitment,
            owner.auth_scheme,
            seed,
        )
        .with_context(|| format!("failed to build account for account index {account_index}"))?;

        owner
            .configure_account(
                &account_seed.account_id,
                account_seed.cosigner_commitments.clone(),
                account_seed.initial_state.clone(),
            )
            .await
            .with_context(|| format!("failed to configure account {}", account_seed.account_id))?;

        owner.accounts.push(account_seed);
    }

    Ok(user_contexts)
}

pub async fn run_scenario(
    config: &RunConfig,
    user_contexts: Vec<UserContext>,
) -> Result<RunMetrics> {
    let run_start = Instant::now();

    let mut workers = JoinSet::new();
    for mut user in user_contexts {
        let cfg = config.clone();
        workers.spawn(async move { run_worker(&cfg, &mut user).await });
    }

    let mut total_ops = 0_u64;
    let mut success_ops = 0_u64;
    let mut failed_ops = 0_u64;
    let mut latencies_ms = Vec::new();
    let mut error_samples = Vec::new();

    while let Some(join_result) = workers.join_next().await {
        let worker = join_result.map_err(|e| anyhow!("worker task failed: {e}"))?;
        total_ops += worker.total_ops;
        success_ops += worker.success_ops;
        failed_ops += worker.failed_ops;
        latencies_ms.extend(worker.latencies_ms);

        if error_samples.len() < 25 {
            let remaining = 25_usize.saturating_sub(error_samples.len());
            error_samples.extend(worker.error_samples.into_iter().take(remaining));
        }
    }

    let run_duration_ms = run_start.elapsed().as_secs_f64() * 1_000.0;
    let run_seconds = (run_duration_ms / 1_000.0).max(0.001);

    let latency = latency_summary(&mut latencies_ms);

    Ok(RunMetrics {
        total_ops,
        success_ops,
        failed_ops,
        run_duration_ms,
        ops_per_sec: total_ops as f64 / run_seconds,
        success_ops_per_sec: success_ops as f64 / run_seconds,
        latency,
        error_samples,
    })
}

async fn run_worker(config: &RunConfig, user: &mut UserContext) -> WorkerMetrics {
    let mut total_ops = 0_u64;
    let mut success_ops = 0_u64;
    let mut failed_ops = 0_u64;
    let mut latencies_ms = Vec::new();
    let mut error_samples = Vec::new();

    let account_count = user.accounts.len();
    let worker_ops = scenario_op_count(config, account_count);
    for op_index in 0..worker_ops {
        total_ops += 1;

        let start = Instant::now();
        let result = match config.scenario {
            Scenario::StateRead => run_read_operation(user, op_index, account_count).await,
            Scenario::StateWrite => run_write_operation(user).await,
            Scenario::Mixed => {
                if should_write(config.mixed_write_percent, op_index, user.user_id) {
                    run_write_operation(user).await
                } else {
                    run_read_operation(user, op_index, account_count).await
                }
            }
            Scenario::StateSync => {
                if should_push_state(config.state_sync_reads_per_push, op_index) {
                    run_push_state_operation(user, op_index, account_count).await
                } else {
                    run_read_operation(user, op_index, account_count).await
                }
            }
            Scenario::Canonicalization => {
                run_canonicalization_operation(config, user, op_index, account_count).await
            }
        };

        let latency_ms = start.elapsed().as_secs_f64() * 1_000.0;

        match result {
            Ok(()) => {
                success_ops += 1;
                latencies_ms.push(latency_ms);
            }
            Err(error) => {
                failed_ops += 1;
                if error_samples.len() < 5 {
                    error_samples.push(error.to_string());
                }
            }
        }
    }

    WorkerMetrics {
        total_ops,
        success_ops,
        failed_ops,
        latencies_ms,
        error_samples,
    }
}

fn scenario_op_count(config: &RunConfig, account_count: usize) -> usize {
    match config.scenario {
        Scenario::Canonicalization => account_count,
        Scenario::StateRead | Scenario::StateWrite | Scenario::Mixed | Scenario::StateSync => {
            usize::try_from(config.ops_per_user).unwrap_or(0)
        }
    }
}

fn should_write(write_percent: u32, op_index: usize, user_id: usize) -> bool {
    if write_percent == 0 {
        return false;
    }
    if write_percent == 100 {
        return true;
    }

    let op = u32::try_from(op_index).unwrap_or(u32::MAX);
    let user = u32::try_from(user_id).unwrap_or(u32::MAX);
    let value = op.wrapping_mul(37).wrapping_add(user.wrapping_mul(17)) % 100;

    value < write_percent
}

fn should_push_state(reads_per_push: u32, op_index: usize) -> bool {
    let cycle = usize::try_from(reads_per_push.saturating_add(1)).unwrap_or(usize::MAX);
    cycle > 0 && op_index % cycle == usize::try_from(reads_per_push).unwrap_or(0)
}

async fn run_read_operation(
    user: &mut UserContext,
    op_index: usize,
    account_count: usize,
) -> Result<()> {
    let account_id = user.accounts[op_index % account_count].account_id.clone();
    user.read_state(&account_id).await
}

async fn run_write_operation(user: &mut UserContext) -> Result<()> {
    let seed = rand::random::<[u8; 32]>();
    let account_seed = create_account_seed(
        &user.signer_commitment,
        user.signers_per_account,
        user.psm_commitment,
        user.auth_scheme,
        seed,
    )?;

    user.configure_account(
        &account_seed.account_id,
        account_seed.cosigner_commitments,
        account_seed.initial_state,
    )
    .await
}

async fn run_push_state_operation(
    user: &mut UserContext,
    op_index: usize,
    account_count: usize,
) -> Result<()> {
    let account_index = op_index % account_count;
    let (account_id, nonce, prev_commitment, delta_payload) = {
        let account = &mut user.accounts[account_index];
        let nonce = account.next_nonce;
        let payload = create_delta_payload(&account.account_id, nonce)?;
        (
            account.account_id.clone(),
            nonce,
            account.commitment.clone(),
            payload,
        )
    };

    let new_commitment = user
        .push_state(&account_id, nonce, prev_commitment, delta_payload)
        .await?;

    if let Some(account) = user.accounts.get_mut(account_index) {
        account.next_nonce = nonce + 1;
        if let Some(commitment) = new_commitment {
            account.commitment = commitment;
        }
    }

    Ok(())
}

async fn run_canonicalization_operation(
    config: &RunConfig,
    user: &mut UserContext,
    op_index: usize,
    account_count: usize,
) -> Result<()> {
    let account_index = op_index % account_count;
    let (account_id, nonce, prev_commitment, delta_payload) = {
        let account = &mut user.accounts[account_index];
        let nonce = account.next_nonce;
        let payload = create_delta_payload(&account.account_id, nonce)?;
        (
            account.account_id.clone(),
            nonce,
            account.commitment.clone(),
            payload,
        )
    };

    let new_commitment = user
        .push_state(&account_id, nonce, prev_commitment, delta_payload)
        .await?;

    if let Some(account) = user.accounts.get_mut(account_index) {
        account.next_nonce = nonce + 1;
        if let Some(commitment) = new_commitment {
            account.commitment = commitment;
        }
    }

    wait_for_delta_terminal_status(config, user, &account_id, nonce).await
}

async fn wait_for_delta_terminal_status(
    config: &RunConfig,
    user: &mut UserContext,
    account_id: &AccountId,
    nonce: u64,
) -> Result<()> {
    let poll_interval = Duration::from_millis(config.canonicalization_poll_interval_ms);
    let timeout = Duration::from_secs(config.canonicalization_timeout_secs);
    let start = Instant::now();

    loop {
        match user.get_delta_status(account_id, nonce).await? {
            DeltaStatusCheck::Terminal | DeltaStatusCheck::NotFound => return Ok(()),
            DeltaStatusCheck::Pending => {}
        }

        if start.elapsed() >= timeout {
            return Err(anyhow!(
                "timed out waiting canonicalization for account {} nonce {}",
                account_id,
                nonce
            ));
        }

        sleep(poll_interval).await;
    }
}

fn is_delta_not_found(error: &ClientError) -> bool {
    match error {
        ClientError::ServerError(message) => message.to_ascii_lowercase().contains("not found"),
        _ => false,
    }
}

fn build_auth_config(cosigner_commitments: Vec<String>, auth_scheme: AuthScheme) -> AuthConfig {
    AuthConfig {
        auth_type: Some(match auth_scheme {
            AuthScheme::Falcon => AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                cosigner_commitments,
            }),
            AuthScheme::Ecdsa => AuthType::MidenEcdsa(MidenEcdsaAuth {
                cosigner_commitments,
            }),
        }),
    }
}

fn random_commitment_hex(auth_scheme: AuthScheme) -> String {
    match auth_scheme {
        AuthScheme::Falcon => {
            let secret = FalconSecretKey::new();
            let pubkey = secret.public_key();
            format!("0x{}", hex::encode(pubkey.to_commitment().to_bytes()))
        }
        AuthScheme::Ecdsa => {
            let secret = EcdsaSecretKey::new();
            let pubkey = secret.public_key();
            format!("0x{}", hex::encode(pubkey.to_commitment().to_bytes()))
        }
    }
}

fn create_account_seed(
    owner_signer_commitment: &str,
    signers_per_account: usize,
    psm_commitment: Word,
    auth_scheme: AuthScheme,
    seed: [u8; 32],
) -> Result<AccountSeed> {
    let signer_commitments =
        build_signer_commitments(owner_signer_commitment, signers_per_account, auth_scheme);

    let signer_words = signer_commitments
        .iter()
        .map(|commitment| word_from_hex(commitment))
        .collect::<Result<Vec<_>>>()?;

    let threshold = if signers_per_account == 1 { 1 } else { 2 };
    let account = MultisigPsmBuilder::new(MultisigPsmConfig::new(
        threshold,
        signer_words,
        psm_commitment,
    ))
    .with_seed(seed)
    .build()?;

    let account_id = account.id();
    let commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));
    let account_data = base64::engine::general_purpose::STANDARD.encode(account.to_bytes());
    let initial_state = serde_json::json!({
        "data": account_data,
        "account_id": account_id.to_string(),
    });

    Ok(AccountSeed {
        account_id,
        initial_state,
        commitment,
        next_nonce: 1,
        cosigner_commitments: signer_commitments,
    })
}

fn create_delta_payload(account_id: &AccountId, nonce: u64) -> Result<Value> {
    let account_delta = AccountDelta::new(
        account_id.to_owned(),
        AccountStorageDelta::default(),
        AccountVaultDelta::default(),
        Felt::new(nonce),
    )
    .map_err(|e| anyhow!("failed to build account delta: {e}"))?;
    let tx_summary = TransactionSummary::new(
        account_delta,
        InputNotes::new(Vec::new()).map_err(|e| anyhow!("failed to build input notes: {e}"))?,
        OutputNotes::new(Vec::new()).map_err(|e| anyhow!("failed to build output notes: {e}"))?,
        Word::from([ZERO; 4]),
    );
    Ok(tx_summary.to_json())
}

fn build_signer_commitments(
    owner_commitment: &str,
    signers_per_account: usize,
    auth_scheme: AuthScheme,
) -> Vec<String> {
    let mut signer_commitments = Vec::with_capacity(signers_per_account);
    signer_commitments.push(owner_commitment.to_owned());

    while signer_commitments.len() < signers_per_account {
        signer_commitments.push(random_commitment_hex(auth_scheme));
    }

    signer_commitments
}

fn commitment_hex_from_pubkey_hex(pubkey_hex: &str, auth_scheme: AuthScheme) -> Result<String> {
    let hex = pubkey_hex.trim_start_matches("0x");
    let bytes = hex::decode(hex).context("failed to decode public key hex")?;
    let commitment_bytes = match auth_scheme {
        AuthScheme::Falcon => {
            let pubkey = FalconPublicKey::read_from_bytes(&bytes)
                .context("failed to parse Falcon public key bytes")?;
            pubkey.to_commitment().to_bytes()
        }
        AuthScheme::Ecdsa => {
            let pubkey = EcdsaPublicKey::read_from_bytes(&bytes)
                .context("failed to parse ECDSA public key bytes")?;
            pubkey.to_commitment().to_bytes()
        }
    };
    Ok(format!("0x{}", hex::encode(commitment_bytes)))
}

fn word_from_hex(input: &str) -> Result<Word> {
    let bytes = hex::decode(input.trim_start_matches("0x"))
        .with_context(|| format!("failed to decode hex word {input}"))?;
    Word::read_from_bytes(&bytes).with_context(|| format!("failed to parse word {input}"))
}

fn latency_summary(latencies_ms: &mut [f64]) -> LatencySummary {
    if latencies_ms.is_empty() {
        return LatencySummary {
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
        };
    }

    latencies_ms.sort_by(f64::total_cmp);

    LatencySummary {
        p50_ms: percentile(latencies_ms, 0.50),
        p95_ms: percentile(latencies_ms, 0.95),
        p99_ms: percentile(latencies_ms, 0.99),
        max_ms: *latencies_ms.last().unwrap_or(&0.0),
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }

    let idx = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
