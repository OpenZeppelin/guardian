use anyhow::{Context, Result, anyhow};
use base64::Engine;
use guardian_client::{
    Auth, AuthConfig, EcdsaSigner, FalconRpoSigner, GuardianClient, MidenEcdsaAuth,
    MidenFalconRpoAuth, Signer, auth_config::AuthType,
};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::{Credential, RunConfig, SESSION_TTL_MARGIN_SECONDS};
use crate::distributed::ExecutionShard;
use crate::model::AuthScheme;
use crate::schemes::build_scheme_plan;
use crate::workload::{Role, role_for_user, watched_writer};
use guardian_client::StartSessionOptions;
use std::time::Duration;

const ACCOUNT_CONFIGURE_CONCURRENCY: usize = 32;
const SEED_GENERATION_CONCURRENCY: usize = 8;

pub struct SeededUser {
    pub user_id: u32,
    pub auth_scheme: AuthScheme,
    pub signer_pubkey: String,
    pub client: GuardianClient,
    /// The same key the client signs requests with, to approve proposals.
    pub signer: Auth,
    pub account_id: AccountId,
    pub commitment: String,
    pub next_nonce: u64,
    pub created_delta_nonces: Vec<u64>,
    /// The writer whose account this user polls as a cosigner; `None` when
    /// the account is its own.
    pub watches: Option<u32>,
}

#[derive(Clone, Copy)]
struct UserPlanEntry {
    user_id: u32,
    auth_scheme: AuthScheme,
    active: bool,
    watches: Option<u32>,
}

#[derive(Clone)]
struct AccountSeed {
    account_id: AccountId,
    initial_state: Value,
    commitment: String,
    cosigner_commitments: Vec<String>,
}

struct AuthBundle {
    signer_pubkey: String,
    signer_commitment: String,
    secret_key_hex: String,
}

#[derive(Clone)]
struct GuardianCommitments {
    falcon: String,
    ecdsa: String,
}

impl GuardianCommitments {
    fn for_scheme(&self, scheme: AuthScheme) -> &str {
        match scheme {
            AuthScheme::Falcon => &self.falcon,
            AuthScheme::Ecdsa => &self.ecdsa,
        }
    }
}

/// Seeds this shard's users: an account per user that does not watch a
/// writer, with the readers watching it as extra cosigners, then a client per
/// user, holding a session when the run is signed by sessions.
pub async fn seed_users(config: &RunConfig, shard: ExecutionShard) -> Result<Vec<SeededUser>> {
    let guardian_endpoint = config.normalized_guardian_endpoint();
    let plan = shard_user_plan(config, shard)?;
    let commitments = GuardianCommitments {
        falcon: fetch_guardian_commitment_hex(&guardian_endpoint, AuthScheme::Falcon).await?,
        ecdsa: fetch_guardian_commitment_hex(&guardian_endpoint, AuthScheme::Ecdsa).await?,
    };
    let bundles = generate_auth_bundles(&plan).await?;
    let mut cosigners: BTreeMap<u32, Vec<String>> = plan
        .iter()
        .filter(|user| user.watches.is_none())
        .map(|user| {
            (
                user.user_id,
                vec![bundles[&user.user_id].signer_commitment.clone()],
            )
        })
        .collect();
    for user in &plan {
        if let Some(writer) = user.watches {
            cosigners
                .get_mut(&writer)
                .ok_or_else(|| anyhow!("user {} watches unseeded writer {writer}", user.user_id))?
                .push(bundles[&user.user_id].signer_commitment.clone());
        }
    }
    let accounts = generate_accounts(&plan, cosigners, &commitments).await?;

    let semaphore = Arc::new(Semaphore::new(ACCOUNT_CONFIGURE_CONCURRENCY.max(1)));
    let bundles = Arc::new(bundles);
    let accounts = Arc::new(accounts);
    let mut tasks = JoinSet::new();
    for user in plan.iter().copied().filter(|user| user.watches.is_none()) {
        let (endpoint, semaphore) = (guardian_endpoint.clone(), Arc::clone(&semaphore));
        let (bundles, accounts) = (Arc::clone(&bundles), Arc::clone(&accounts));
        let credential = config.credential.clone();
        let ttl = session_ttl(config);
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("seed semaphore should remain available");
            let account = &accounts[&user.user_id];
            let mut seeded =
                connect_user(&endpoint, user, &bundles[&user.user_id], account).await?;
            seeded
                .client
                .configure(
                    &account.account_id,
                    build_auth_config(account.cosigner_commitments.clone(), user.auth_scheme),
                    account.initial_state.clone(),
                )
                .await
                .with_context(|| format!("failed to configure account {}", account.account_id))?;
            start_session(&mut seeded, user, &credential, ttl).await?;
            Ok::<_, anyhow::Error>(seeded)
        });
    }
    let mut users = Vec::with_capacity(plan.len());
    while let Some(joined) = tasks.join_next().await {
        users.push(joined.map_err(|error| anyhow!("seed task failed: {error}"))??);
    }

    let mut tasks = JoinSet::new();
    for user in plan.iter().copied() {
        let Some(writer) = user.watches else {
            continue;
        };
        let (endpoint, semaphore) = (guardian_endpoint.clone(), Arc::clone(&semaphore));
        let (bundles, accounts) = (Arc::clone(&bundles), Arc::clone(&accounts));
        let credential = config.credential.clone();
        let ttl = session_ttl(config);
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("seed semaphore should remain available");
            let mut seeded =
                connect_user(&endpoint, user, &bundles[&user.user_id], &accounts[&writer]).await?;
            start_session(&mut seeded, user, &credential, ttl).await?;
            Ok::<_, anyhow::Error>(seeded)
        });
    }
    while let Some(joined) = tasks.join_next().await {
        users.push(joined.map_err(|error| anyhow!("seed task failed: {error}"))??);
    }
    users.sort_by_key(|user| user.user_id);
    Ok(users)
}

fn session_ttl(config: &RunConfig) -> Duration {
    Duration::from_secs(config.duration_seconds + SESSION_TTL_MARGIN_SECONDS)
}

async fn connect_user(
    endpoint: &str,
    user: UserPlanEntry,
    bundle: &AuthBundle,
    account: &AccountSeed,
) -> Result<SeededUser> {
    let client = GuardianClient::connect(endpoint.to_string())
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))?
        .with_auth(build_auth(user.auth_scheme, &bundle.secret_key_hex)?);
    Ok(SeededUser {
        user_id: user.user_id,
        auth_scheme: user.auth_scheme,
        signer_pubkey: bundle.signer_pubkey.clone(),
        client,
        signer: build_auth(user.auth_scheme, &bundle.secret_key_hex)?,
        account_id: account.account_id,
        commitment: account.commitment.clone(),
        next_nonce: 1,
        created_delta_nonces: Vec::new(),
        watches: user.watches,
    })
}

async fn start_session(
    seeded: &mut SeededUser,
    user: UserPlanEntry,
    credential: &Credential,
    ttl: Duration,
) -> Result<()> {
    let Credential::Session { network } = credential else {
        return Ok(());
    };
    if !user.active {
        return Ok(());
    }
    seeded
        .client
        .start_session(StartSessionOptions {
            network: network.clone(),
            ttl,
        })
        .await
        .with_context(|| format!("failed to start a session for user {}", user.user_id))?;
    Ok(())
}

async fn fetch_guardian_commitment_hex(endpoint: &str, scheme: AuthScheme) -> Result<String> {
    let mut client = GuardianClient::connect(endpoint.to_string())
        .await
        .with_context(|| format!("failed to connect to {}", endpoint))?;
    let (commitment_hex, _) = client
        .get_pubkey(Some(scheme.as_str()))
        .await
        .with_context(|| {
            format!(
                "failed to read {} GUARDIAN commitment via get_pubkey",
                scheme.as_str()
            )
        })?;
    Ok(commitment_hex)
}

/// A fresh signer of `scheme`. The commitment is the signer's own, as the
/// server derives it, rather than recomputed from the public key bytes.
fn build_auth_bundle(scheme: AuthScheme) -> Result<AuthBundle> {
    let (auth, secret_key_hex) = match scheme {
        AuthScheme::Falcon => {
            let secret_key = FalconSecretKey::new();
            let secret_key_hex = hex::encode(secret_key.to_bytes());
            (
                Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key)),
                secret_key_hex,
            )
        }
        AuthScheme::Ecdsa => {
            let secret_key = EcdsaSecretKey::new();
            let secret_key_hex = hex::encode(secret_key.to_bytes());
            (
                Auth::EcdsaSigner(EcdsaSigner::new(secret_key)),
                secret_key_hex,
            )
        }
    };
    Ok(AuthBundle {
        signer_pubkey: auth.public_key_hex(),
        signer_commitment: auth.commitment_hex(),
        secret_key_hex,
    })
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

fn create_account_seed(
    cosigner_commitments: Vec<String>,
    auth_scheme: AuthScheme,
    guardian_commitment: Word,
    seed: [u8; 32],
) -> Result<AccountSeed> {
    let signer_words = cosigner_commitments
        .iter()
        .map(|commitment| word_from_hex(commitment))
        .collect::<Result<Vec<_>>>()?;

    let signature_scheme = match auth_scheme {
        AuthScheme::Falcon => guardian_shared::SignatureScheme::Falcon,
        AuthScheme::Ecdsa => guardian_shared::SignatureScheme::Ecdsa,
    };
    let account = MultisigGuardianBuilder::new(
        MultisigGuardianConfig::new(1, signer_words, guardian_commitment)
            .with_signature_scheme(signature_scheme),
    )
    .with_seed(seed)
    .build()?;

    let account_id = account.id();
    let commitment = format!("0x{}", hex::encode(account.to_commitment().as_bytes()));
    let account_data = base64::engine::general_purpose::STANDARD.encode(account.to_bytes());
    let initial_state = serde_json::json!({
        "data": account_data,
        "account_id": account_id.to_string(),
    });

    Ok(AccountSeed {
        account_id,
        initial_state,
        commitment,
        cosigner_commitments,
    })
}

async fn generate_auth_bundles(plan: &[UserPlanEntry]) -> Result<BTreeMap<u32, AuthBundle>> {
    let semaphore = Arc::new(Semaphore::new(SEED_GENERATION_CONCURRENCY.max(1)));
    let mut tasks = JoinSet::new();
    for user in plan.iter().copied() {
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("seed generation semaphore should remain available");
            let bundle = tokio::task::spawn_blocking(move || build_auth_bundle(user.auth_scheme))
                .await
                .map_err(|error| anyhow!("seed generation task failed: {error}"))??;
            Ok::<_, anyhow::Error>((user.user_id, bundle))
        });
    }
    let mut bundles = BTreeMap::new();
    while let Some(joined) = tasks.join_next().await {
        let (user_id, bundle) = joined.map_err(|error| anyhow!("seed task failed: {error}"))??;
        bundles.insert(user_id, bundle);
    }
    Ok(bundles)
}

async fn generate_accounts(
    plan: &[UserPlanEntry],
    cosigners: BTreeMap<u32, Vec<String>>,
    commitments: &GuardianCommitments,
) -> Result<BTreeMap<u32, AccountSeed>> {
    let semaphore = Arc::new(Semaphore::new(SEED_GENERATION_CONCURRENCY.max(1)));
    let schemes: BTreeMap<u32, AuthScheme> = plan
        .iter()
        .map(|user| (user.user_id, user.auth_scheme))
        .collect();
    let mut tasks = JoinSet::new();
    for (user_id, cosigner_commitments) in cosigners {
        let semaphore = Arc::clone(&semaphore);
        let auth_scheme = schemes[&user_id];
        let guardian_commitment = word_from_hex(commitments.for_scheme(auth_scheme))?;
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("seed generation semaphore should remain available");
            let account = tokio::task::spawn_blocking(move || {
                create_account_seed(
                    cosigner_commitments,
                    auth_scheme,
                    guardian_commitment,
                    rand::random::<[u8; 32]>(),
                )
            })
            .await
            .map_err(|error| anyhow!("seed generation task failed: {error}"))??;
            Ok::<_, anyhow::Error>((user_id, account))
        });
    }
    let mut accounts = BTreeMap::new();
    while let Some(joined) = tasks.join_next().await {
        let (user_id, account) = joined.map_err(|error| anyhow!("seed task failed: {error}"))??;
        accounts.insert(user_id, account);
    }
    Ok(accounts)
}

/// This shard's users with their schemes and roles. A reader that watches a
/// writer takes that writer's scheme: an account registers one scheme.
fn shard_user_plan(config: &RunConfig, shard: ExecutionShard) -> Result<Vec<UserPlanEntry>> {
    let scheme_plan = build_scheme_plan(config.users, &config.scheme_distribution);
    let assigned_user_ids = shard.assigned_user_ids(config.users);
    let scheme_of = |user_id: u32| {
        scheme_plan
            .get(user_id as usize)
            .copied()
            .ok_or_else(|| anyhow!("missing scheme assignment for user {user_id}"))
    };
    let Some(roles) = &config.roles else {
        return assigned_user_ids
            .into_iter()
            .map(|user_id| {
                Ok(UserPlanEntry {
                    user_id,
                    auth_scheme: scheme_of(user_id)?,
                    active: true,
                    watches: None,
                })
            })
            .collect();
    };
    let writers: Vec<u32> = assigned_user_ids
        .iter()
        .copied()
        .filter(|user_id| role_for_user(roles, *user_id) == Role::Writer)
        .collect();
    let mut reader_rank = 0;
    let mut plan = Vec::with_capacity(assigned_user_ids.len());
    for user_id in assigned_user_ids {
        let role = role_for_user(roles, user_id);
        let watches = match role {
            Role::Reader => {
                let watches = watched_writer(roles, &writers, reader_rank);
                reader_rank += 1;
                watches
            }
            Role::Writer | Role::Idle => None,
        };
        plan.push(UserPlanEntry {
            user_id,
            auth_scheme: scheme_of(watches.unwrap_or(user_id))?,
            active: role != Role::Idle,
            watches,
        });
    }
    Ok(plan)
}

fn build_auth(scheme: AuthScheme, secret_key_hex: &str) -> Result<Auth> {
    let secret_key_bytes =
        hex::decode(secret_key_hex).context("failed to decode seeded secret key")?;
    match scheme {
        AuthScheme::Falcon => {
            let secret_key = FalconSecretKey::read_from_bytes(&secret_key_bytes)
                .context("failed to parse seeded Falcon secret key")?;
            Ok(Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key)))
        }
        AuthScheme::Ecdsa => {
            let secret_key = EcdsaSecretKey::read_from_bytes(&secret_key_bytes)
                .context("failed to parse seeded ECDSA secret key")?;
            Ok(Auth::EcdsaSigner(EcdsaSigner::new(secret_key)))
        }
    }
}

fn word_from_hex(input: &str) -> Result<Word> {
    let bytes = hex::decode(input.trim_start_matches("0x"))
        .with_context(|| format!("failed to decode hex word {input}"))?;
    Word::read_from_bytes(&bytes).with_context(|| format!("failed to parse word {input}"))
}
