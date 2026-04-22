use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    Pending,
    Complete,
    Partial,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupManifest {
    pub run_id: String,
    pub created_at: DateTime<Utc>,
    pub guardian_endpoint: String,
    pub cleanup_target: CleanupTarget,
    pub accounts: Vec<CleanupAccountRecord>,
    pub purge_status: CleanupStatus,
    pub purged_at: Option<DateTime<Utc>>,
    pub verification_summary: Option<VerificationSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupTarget {
    pub aws: CleanupAwsTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupAwsTarget {
    pub profile: Option<String>,
    pub region: String,
    pub ecs_cluster: String,
    pub ecs_service: String,
    pub ecs_container: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupAccountRecord {
    pub account_id: String,
    pub owner_user_id: u32,
    pub auth_scheme: String,
    pub created_delta_nonces: Vec<u64>,
    pub last_known_commitment: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationSummary {
    pub states_remaining: u64,
    pub deltas_remaining: u64,
    pub delta_proposals_remaining: u64,
    pub account_metadata_remaining: u64,
}

impl CleanupManifest {
    pub fn new(run_id: String, guardian_endpoint: String, cleanup_target: CleanupTarget) -> Self {
        Self {
            run_id,
            created_at: Utc::now(),
            guardian_endpoint,
            cleanup_target,
            accounts: Vec::new(),
            purge_status: CleanupStatus::Pending,
            purged_at: None,
            verification_summary: None,
        }
    }

    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn write_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}
