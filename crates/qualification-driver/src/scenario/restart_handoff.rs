//! What a scenario's pass before a GUARDIAN restart leaves for the pass after it. The two passes
//! are separate driver processes writing to the same results directory, so the record of what
//! the first pass saw has to go through a file.

use std::path::{Path, PathBuf};

use anyhow::Context;
use guardian_client::ProposalExecution;
use serde::{Deserialize, Serialize};

/// An execution record exactly as GUARDIAN reported it, without `newly_accepted`, which
/// describes the request that read the record rather than the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    pub account_id: String,
    pub proposal_id: String,
    pub state: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub delta_nonce: Option<u64>,
    pub proposal_exists: bool,
    pub ignored_signatures: u32,
    pub updated_at: String,
}

impl ExecutionSnapshot {
    pub fn of(execution: &ProposalExecution) -> Self {
        Self {
            account_id: execution.account_id.clone(),
            proposal_id: execution.proposal_id.clone(),
            state: execution.state.as_str().to_string(),
            error_code: execution
                .error
                .as_ref()
                .map(|error| error.code.as_str().to_string()),
            error_message: execution.error.as_ref().map(|error| error.message.clone()),
            delta_nonce: execution.delta_nonce,
            proposal_exists: execution.proposal_exists,
            ignored_signatures: execution.ignored_signatures,
            updated_at: execution.updated_at.clone(),
        }
    }

    pub fn path(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.json"))
    }

    pub fn save(&self, dir: &Path, name: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating the restart handoff directory {}", dir.display()))?;
        let path = Self::path(dir, name);
        std::fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(dir: &Path, name: &str) -> anyhow::Result<Self> {
        let path = Self::path(dir, name);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutionSnapshot;

    fn snapshot() -> ExecutionSnapshot {
        ExecutionSnapshot {
            account_id: "0xabc".to_string(),
            proposal_id: "0xdef".to_string(),
            state: "failed".to_string(),
            error_code: Some("GUARDIAN_EXECUTION_NODE_UNAVAILABLE".to_string()),
            error_message: Some("Guardian could not read the chain".to_string()),
            delta_nonce: None,
            proposal_exists: true,
            ignored_signatures: 0,
            updated_at: "2026-10-19T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn a_snapshot_reads_back_as_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let written = snapshot();
        written.save(dir.path(), "execution").expect("saves");
        let read = ExecutionSnapshot::load(dir.path(), "execution").expect("loads");
        assert_eq!(read, written);
    }

    #[test]
    fn a_missing_snapshot_is_an_error_not_a_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(ExecutionSnapshot::load(dir.path(), "execution").is_err());
    }
}
