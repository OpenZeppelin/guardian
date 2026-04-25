use anyhow::{Context, Result, anyhow};
use base64::Engine;
use guardian_client::{
    Auth, AuthConfig, EcdsaSigner, FalconRpoSigner, GuardianClient, MidenEcdsaAuth,
    MidenFalconRpoAuth, auth_config::AuthType,
};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, SecretKey as EcdsaSecretKey,
};
use miden_protocol::crypto::dsa::falcon512_poseidon2::{
    PublicKey as FalconPublicKey, SecretKey as FalconSecretKey,
};
use miden_protocol::utils::serde::{Deserializable, Serializable};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::RunConfig;
use crate::distributed::ExecutionShard;
use crate::model::AuthScheme;
use crate::schemes::build_scheme_plan;

const ACCOUNT_CONFIGURE_CONCURRENCY: usize = 32;
const SEED_GENERATION_CONCURRENCY: usize = 8;

pub struct SeededUser {
    pub user_id: u32,
    pub auth_scheme: AuthScheme,
    pub signer_pubkey: String,
    pub client: GuardianClient,
    pub account_id: AccountId,
    pub commitment: String,
    pub next_nonce: u64,
    pub created_delta_nonces: Vec<u64>,
}

#[derive(Clone, Copy)]
struct UserPlanEntry {
    user_id: u32,
    auth_scheme: AuthScheme,
}

struct AccountSeed {
    account_id: AccountId,
    initial_state: Value,
    commitment: String,
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

#[derive(Clone)]
struct SeedEntry {
    auth_scheme: AuthScheme,
    secret_key_hex: String,
    signer_pubkey: String,
    signer_commitment: String,
    account_id: String,
    account_commitment: String,
    initial_state: Value,
}

pub async fn seed_users(config: &RunConfig, shard: ExecutionShard) -> Result<Vec<SeededUser>> {
    let guardian_endpoint = config.normalized_guardian_endpoint();
    let assigned_users = shard_user_plan(config, shard)?;
    let commitments = GuardianCommitments {
        falcon: fetch_guardian_commitment_hex(&guardian_endpoint, AuthScheme::Falcon).await?,
        ecdsa: fetch_guardian_commitment_hex(&guardian_endpoint, AuthScheme::Ecdsa).await?,
    };
    let required_counts = count_required_entries(&assigned_users);
    let entries = generate_seed_entries(&commitments, &required_counts).await?;
    let assignments = assign_seed_entries(&assigned_users, entries)?;
    let assignment_count = assignments.len();
    let concurrency_limit = ACCOUNT_CONFIGURE_CONCURRENCY
        .max(1)
        .min(assignment_count.max(1));
    let semaphore = Arc::new(Semaphore::new(concurrency_limit));
    let mut tasks = JoinSet::new();

    for (user_id, entry) in assignments {
        let endpoint = guardian_endpoint.clone();
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("seed semaphore should remain available");
            configure_seeded_user(endpoint, user_id, entry).await
        });
    }

    let mut users = Vec::with_capacity(assignment_count);
    while let Some(joined) = tasks.join_next().await {
        users.push(joined.map_err(|error| anyhow!("seed task failed: {error}"))??);
    }
    users.sort_by_key(|user| user.user_id);
    Ok(users)
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

fn build_auth_bundle(scheme: AuthScheme) -> Result<AuthBundle> {
    match scheme {
        AuthScheme::Falcon => {
            let secret_key = FalconSecretKey::new();
            let secret_key_hex = hex::encode(secret_key.to_bytes());
            let signer = FalconRpoSigner::new(secret_key);
            let signer_pubkey = signer.public_key_hex();
            let signer_commitment = commitment_hex_from_pubkey_hex(&signer_pubkey, scheme)?;
            Ok(AuthBundle {
                signer_pubkey,
                signer_commitment,
                secret_key_hex,
            })
        }
        AuthScheme::Ecdsa => {
            let secret_key = EcdsaSecretKey::new();
            let secret_key_hex = hex::encode(secret_key.to_bytes());
            let signer = EcdsaSigner::new(secret_key);
            let signer_pubkey = signer.public_key_hex();
            let signer_commitment = commitment_hex_from_pubkey_hex(&signer_pubkey, scheme)?;
            Ok(AuthBundle {
                signer_pubkey,
                signer_commitment,
                secret_key_hex,
            })
        }
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

fn create_account_seed(
    owner_signer_commitment: &str,
    guardian_commitment: Word,
    seed: [u8; 32],
) -> Result<AccountSeed> {
    let signer_commitments = [owner_signer_commitment.to_string()];
    let signer_words = signer_commitments
        .iter()
        .map(|commitment| word_from_hex(commitment))
        .collect::<Result<Vec<_>>>()?;

    let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
        1,
        signer_words,
        guardian_commitment,
    ))
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
    })
}

async fn generate_seed_entries(
    commitments: &GuardianCommitments,
    required_counts: &BTreeMap<AuthScheme, usize>,
) -> Result<BTreeMap<AuthScheme, Vec<SeedEntry>>> {
    let mut tasks = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(SEED_GENERATION_CONCURRENCY.max(1)));

    for scheme in [AuthScheme::Falcon, AuthScheme::Ecdsa] {
        let required = required_counts.get(&scheme).copied().unwrap_or_default();
        if required == 0 {
            continue;
        }

        let guardian_commitment = match scheme {
            AuthScheme::Falcon => commitments.falcon.clone(),
            AuthScheme::Ecdsa => commitments.ecdsa.clone(),
        };
        for _ in 0..required {
            let semaphore = Arc::clone(&semaphore);
            let guardian_commitment = guardian_commitment.clone();
            tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .expect("seed generation semaphore should remain available");
                tokio::task::spawn_blocking(move || {
                    generate_seed_entry(scheme, &guardian_commitment)
                })
                .await
                .map_err(|error| anyhow!("seed generation task failed: {error}"))?
            });
        }
    }

    let mut entries_by_scheme = BTreeMap::new();
    entries_by_scheme.insert(AuthScheme::Falcon, Vec::new());
    entries_by_scheme.insert(AuthScheme::Ecdsa, Vec::new());

    while let Some(joined) = tasks.join_next().await {
        let entry = joined.map_err(|error| anyhow!("seed task failed: {error}"))??;
        entries_by_scheme
            .entry(entry.auth_scheme)
            .or_insert_with(Vec::new)
            .push(entry);
    }

    Ok(entries_by_scheme)
}

fn generate_seed_entry(scheme: AuthScheme, guardian_commitment_hex: &str) -> Result<SeedEntry> {
    let auth_bundle = build_auth_bundle(scheme)?;
    let account_seed = create_account_seed(
        &auth_bundle.signer_commitment,
        word_from_hex(guardian_commitment_hex)?,
        rand::random::<[u8; 32]>(),
    )?;

    Ok(SeedEntry {
        auth_scheme: scheme,
        secret_key_hex: auth_bundle.secret_key_hex,
        signer_pubkey: auth_bundle.signer_pubkey,
        signer_commitment: auth_bundle.signer_commitment,
        account_id: account_seed.account_id.to_string(),
        account_commitment: account_seed.commitment,
        initial_state: account_seed.initial_state,
    })
}

fn assign_seed_entries(
    user_plan: &[UserPlanEntry],
    entries_by_scheme: BTreeMap<AuthScheme, Vec<SeedEntry>>,
) -> Result<Vec<(u32, SeedEntry)>> {
    let mut entries_by_scheme = entries_by_scheme
        .into_iter()
        .map(|(scheme, entries)| (scheme, entries.into_iter()))
        .collect::<BTreeMap<_, _>>();
    let mut assignments = Vec::with_capacity(user_plan.len());

    for user in user_plan {
        let entry = entries_by_scheme
            .get_mut(&user.auth_scheme)
            .and_then(|entries| entries.next())
            .ok_or_else(|| {
                anyhow!(
                    "missing generated seed entry for {}",
                    user.auth_scheme.as_str()
                )
            })?;
        assignments.push((user.user_id, entry));
    }

    Ok(assignments)
}

fn count_required_entries(user_plan: &[UserPlanEntry]) -> BTreeMap<AuthScheme, usize> {
    let mut counts = BTreeMap::new();
    for entry in user_plan {
        *counts.entry(entry.auth_scheme).or_default() += 1;
    }
    counts
}

fn shard_user_plan(config: &RunConfig, shard: ExecutionShard) -> Result<Vec<UserPlanEntry>> {
    let scheme_plan = build_scheme_plan(config.users, &config.scheme_distribution);
    let assigned_user_ids = shard.assigned_user_ids(config.users);
    let mut plan = Vec::with_capacity(assigned_user_ids.len());
    for user_id in assigned_user_ids {
        let auth_scheme = *scheme_plan
            .get(user_id as usize)
            .ok_or_else(|| anyhow!("missing scheme assignment for user {user_id}"))?;
        plan.push(UserPlanEntry {
            user_id,
            auth_scheme,
        });
    }
    Ok(plan)
}

async fn configure_seeded_user(
    guardian_endpoint: String,
    user_id: u32,
    entry: SeedEntry,
) -> Result<SeededUser> {
    let auth = build_auth_from_seed_entry(&entry)?;
    let account_id = AccountId::from_hex(&entry.account_id)
        .with_context(|| format!("failed to parse seeded account id {}", entry.account_id))?;
    let mut client = GuardianClient::connect(guardian_endpoint.clone())
        .await
        .with_context(|| format!("failed to connect to {}", guardian_endpoint))?
        .with_auth(auth);
    client
        .configure(
            &account_id,
            build_auth_config(vec![entry.signer_commitment.clone()], entry.auth_scheme),
            entry.initial_state.clone(),
        )
        .await
        .with_context(|| format!("failed to configure account {}", entry.account_id))?;

    Ok(SeededUser {
        user_id,
        auth_scheme: entry.auth_scheme,
        signer_pubkey: entry.signer_pubkey,
        client,
        account_id,
        commitment: entry.account_commitment,
        next_nonce: 1,
        created_delta_nonces: Vec::new(),
    })
}

fn build_auth_from_seed_entry(entry: &SeedEntry) -> Result<Auth> {
    let secret_key_bytes = hex::decode(&entry.secret_key_hex).with_context(|| {
        format!(
            "failed to decode seeded secret key for {}",
            entry.account_id
        )
    })?;
    match entry.auth_scheme {
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
