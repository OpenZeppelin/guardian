use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use miden_multisig_client::{MultisigClient, SecretKey};
use miden_protocol::utils::serde::Serializable;
use serde::{Deserialize, Serialize};
use tempfile::{NamedTempFile, TempDir};

use crate::config::parse_miden_endpoint;

const FIXTURE_VERSION: u32 = 1;

/// How far an account got through provisioning.
///
/// The key is persisted before Guardian registration so an interrupted run
/// never loses it, which means a persisted entry does not imply a registered
/// account. Without recording that distinction, a resume would count the entry
/// as done and leave Guardian holding fewer accounts than the fixture claims --
/// invisible at two accounts, material at the sizes the target needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningState {
    /// Key generated and account created on the Miden side; not yet registered.
    Created,
    /// Registered with Guardian and ready to use.
    #[default]
    Registered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountFixture {
    pub label: String,
    pub account_id: String,
    pub secret_key_hex: String,
    /// Defaults to `Registered` so fixtures written before this field existed
    /// keep working; they were only ever persisted by a run that completed.
    #[serde(default)]
    pub state: ProvisioningState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub version: u32,
    pub guardian_endpoint: String,
    pub miden_endpoint: String,
    pub accounts: Vec<AccountFixture>,
}

impl Fixture {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read account fixture {}", path.display()))?;
        let fixture: Self = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse account fixture {}", path.display()))?;
        if fixture.version != FIXTURE_VERSION {
            bail!(
                "unsupported account fixture version {}; expected {}",
                fixture.version,
                FIXTURE_VERSION
            );
        }
        if fixture.accounts.len() < 2 {
            bail!(
                "account fixture must contain at least two accounts, found {}",
                fixture.accounts.len()
            );
        }
        let unregistered: Vec<&str> = fixture
            .accounts
            .iter()
            .filter(|account| account.state != ProvisioningState::Registered)
            .map(|account| account.label.as_str())
            .collect();
        if !unregistered.is_empty() {
            bail!(
                "fixture has {} account(s) not registered with Guardian ({}); re-run `prepare` to \
                 finish provisioning",
                unregistered.len(),
                unregistered.join(", ")
            );
        }
        Ok(fixture)
    }

    /// Load without the minimum-account check, for resuming provisioning.
    ///
    /// A fixture interrupted after its first account is valid input for a
    /// top-up even though it is not yet usable by a run.
    fn load_partial(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read account fixture {}", path.display()))?;
        let fixture: Self = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse account fixture {}", path.display()))?;
        if fixture.version != FIXTURE_VERSION {
            bail!(
                "unsupported account fixture version {}; expected {}",
                fixture.version,
                FIXTURE_VERSION
            );
        }
        Ok(fixture)
    }
}

/// Label for the account at `index`.
///
/// The first two keep their original names so existing fixtures, the runner's
/// sender/receiver pair, and the README stay valid; the rest are numbered.
fn label_for(index: usize) -> String {
    match index {
        0 => "alice".to_string(),
        1 => "bob".to_string(),
        other => format!("account-{other:04}"),
    }
}

/// Provision `count` accounts into `output`, resuming an interrupted run.
///
/// Resume rather than refuse: at the sizes the scalability target needs, a
/// provisioning run can be interrupted after some accounts are already created,
/// registered, and funded. Existing entries are never regenerated or reordered —
/// only missing ones are appended — so a funded account can never be stranded by
/// re-running this command. Reprovisioning from scratch still requires moving the
/// file away explicitly.
pub async fn prepare(
    guardian_endpoint: String,
    miden_endpoint: String,
    output: &Path,
    count: usize,
    discard_unregistered: bool,
) -> Result<Fixture> {
    if count < 2 {
        bail!("account count must be at least 2, got {count}");
    }
    let endpoint = parse_miden_endpoint(&miden_endpoint)?;
    let mut fixture = if output.exists() {
        let existing = Fixture::load_partial(output)?;
        // Topping up against different endpoints would mix accounts from two
        // networks into one fixture, and the extras would be unusable.
        if existing.guardian_endpoint != guardian_endpoint
            || existing.miden_endpoint != miden_endpoint
        {
            bail!(
                "existing fixture {} targets guardian={} miden={}, but this run targets guardian={} miden={}; \
                 move the fixture aside to provision against different endpoints",
                output.display(),
                existing.guardian_endpoint,
                existing.miden_endpoint,
                guardian_endpoint,
                miden_endpoint
            );
        }
        existing
    } else {
        Fixture {
            version: FIXTURE_VERSION,
            guardian_endpoint,
            miden_endpoint,
            accounts: Vec::with_capacity(count),
        }
    };

    // An account interrupted between persistence and registration cannot be
    // registered later: `push_account` needs the local account store, which
    // lived in a temporary directory that is gone once the process exited, and
    // the client offers no way to rebuild it from the persisted key
    // (`recover_by_key` only finds accounts Guardian already knows).
    //
    // Such an account is therefore unusable, and it cannot have been funded --
    // funding uses the IDs `prepare` prints only on success. Replacing it is
    // safe, but it does discard persisted key material, so it stays opt-in.
    let unregistered: Vec<String> = fixture
        .accounts
        .iter()
        .filter(|account| account.state != ProvisioningState::Registered)
        .map(|account| account.label.clone())
        .collect();
    if !unregistered.is_empty() {
        if !discard_unregistered {
            bail!(
                "fixture {} has {} account(s) created but never registered with Guardian ({}). \
                 They cannot be registered now because the local account store is gone, and they \
                 are unusable. Re-run with --discard-unregistered to replace them, or move the \
                 fixture aside to start over.",
                output.display(),
                unregistered.len(),
                unregistered.join(", ")
            );
        }
        fixture
            .accounts
            .retain(|account| account.state == ProvisioningState::Registered);
        persist_fixture(output, &fixture)?;
        eprintln!(
            "discarded {} unregistered account(s): {}",
            unregistered.len(),
            unregistered.join(", ")
        );
    }

    if fixture.accounts.len() >= count {
        return Ok(fixture);
    }

    while fixture.accounts.len() < count {
        let label = label_for(fixture.accounts.len());
        let label = label.as_str();
        let secret_key = SecretKey::new();
        let secret_key_hex = hex::encode(secret_key.to_bytes());
        let data_dir =
            TempDir::new().context("failed to create temporary Miden client directory")?;
        let mut client = MultisigClient::builder()
            .miden_endpoint(endpoint.clone())
            .guardian_endpoint(fixture.guardian_endpoint.clone())
            .account_dir(data_dir.path())
            .with_secret_key(secret_key)
            .build()
            .await
            .with_context(|| format!("failed to build {label} client"))?;
        let commitment = client.user_commitment();
        let account_id = client
            .create_account(1, vec![commitment])
            .await
            .with_context(|| format!("failed to create {label} account"))?
            .id();
        fixture.accounts.push(AccountFixture {
            label: label.to_string(),
            account_id: account_id.to_string(),
            secret_key_hex,
            state: ProvisioningState::Created,
        });
        // Persist before registering: the key must survive an interruption at
        // any point. The `Created` state records that registration is still
        // outstanding so a resume retries it.
        persist_fixture(output, &fixture)?;
        client
            .push_account()
            .await
            .with_context(|| format!("failed to register {label} account with Guardian"))?;
        let last = fixture.accounts.len() - 1;
        fixture.accounts[last].state = ProvisioningState::Registered;
        persist_fixture(output, &fixture)?;
    }

    Ok(fixture)
}

fn persist_fixture(output: &Path, fixture: &Fixture) -> Result<()> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create fixture directory {}", parent.display()))?;
    }
    let directory = parent.unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(directory)
        .with_context(|| format!("failed to create fixture file in {}", directory.display()))?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), fixture)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    restrict_permissions(temporary.path())?;
    temporary
        .persist(output)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to write account fixture {}", output.display()))?;
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(path: &Path) -> Result<()> {
    bail!(
        "cannot restrict permissions on {} on this platform; refusing to persist secret keys unprotected",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_partial_fixture_for_account_recovery() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        let mut fixture = Fixture {
            version: FIXTURE_VERSION,
            guardian_endpoint: "http://localhost:50051".to_string(),
            miden_endpoint: "https://rpc.testnet.miden.io".to_string(),
            accounts: vec![AccountFixture {
                label: "alice".to_string(),
                account_id: "0xalice".to_string(),
                secret_key_hex: "secret".to_string(),
                state: ProvisioningState::Registered,
            }],
        };

        persist_fixture(&path, &fixture).unwrap();

        let persisted: Fixture = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(persisted.accounts[0].secret_key_hex, "secret");

        fixture.accounts.push(AccountFixture {
            label: "bob".to_string(),
            account_id: "0xbob".to_string(),
            secret_key_hex: "another-secret".to_string(),
            state: ProvisioningState::Registered,
        });
        persist_fixture(&path, &fixture).unwrap();

        let persisted: Fixture = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(persisted.accounts.len(), 2);
    }

    fn fixture_with(count: usize) -> Fixture {
        Fixture {
            version: FIXTURE_VERSION,
            guardian_endpoint: "http://localhost:50051".to_string(),
            miden_endpoint: "https://rpc.testnet.miden.io".to_string(),
            accounts: (0..count)
                .map(|index| AccountFixture {
                    label: label_for(index),
                    account_id: format!("0x{index:04}"),
                    secret_key_hex: format!("secret-{index}"),
                    state: ProvisioningState::Registered,
                })
                .collect(),
        }
    }

    #[test]
    fn labels_keep_original_names_for_the_first_two() {
        assert_eq!(label_for(0), "alice");
        assert_eq!(label_for(1), "bob");
        assert_eq!(label_for(2), "account-0002");
        assert_eq!(label_for(117), "account-0117");
    }

    #[test]
    fn load_accepts_more_than_two_accounts() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        persist_fixture(&path, &fixture_with(100)).unwrap();

        assert_eq!(Fixture::load(&path).unwrap().accounts.len(), 100);
    }

    #[test]
    fn load_rejects_a_fixture_with_fewer_than_two_accounts() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        persist_fixture(&path, &fixture_with(1)).unwrap();

        assert!(Fixture::load(&path).is_err());
    }

    #[test]
    fn load_partial_accepts_an_interrupted_fixture_so_it_can_be_resumed() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        persist_fixture(&path, &fixture_with(1)).unwrap();

        let partial = Fixture::load_partial(&path).unwrap();
        assert_eq!(partial.accounts.len(), 1);
        assert_eq!(partial.accounts[0].secret_key_hex, "secret-0");
    }

    #[tokio::test]
    async fn prepare_is_a_noop_when_the_fixture_already_has_enough_accounts() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        let existing = fixture_with(4);
        persist_fixture(&path, &existing).unwrap();

        // No network access: the requested count is already satisfied, so this
        // must return the existing fixture without creating anything.
        let resumed = prepare(
            existing.guardian_endpoint.clone(),
            existing.miden_endpoint.clone(),
            &path,
            4,
            false,
        )
        .await
        .unwrap();

        assert_eq!(resumed.accounts.len(), 4);
        assert_eq!(resumed.accounts[0].secret_key_hex, "secret-0");
    }

    #[tokio::test]
    async fn prepare_refuses_to_top_up_a_fixture_from_different_endpoints() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        persist_fixture(&path, &fixture_with(2)).unwrap();

        // Topping up against another network would mix unusable accounts into a
        // fixture whose existing entries are funded on the original one.
        let error = prepare(
            "http://localhost:50051".to_string(),
            "https://rpc.devnet.miden.io".to_string(),
            &path,
            4,
            false,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("move the fixture aside"));
    }

    #[test]
    fn load_rejects_a_fixture_holding_an_unregistered_account() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        let mut fixture = fixture_with(3);
        fixture.accounts[2].state = ProvisioningState::Created;
        persist_fixture(&path, &fixture).unwrap();

        let error = Fixture::load(&path).unwrap_err();
        assert!(error.to_string().contains("not registered"));
    }

    #[test]
    fn fixtures_written_before_the_state_field_load_as_registered() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        let legacy = r#"{
            "version": 1,
            "guardian_endpoint": "http://localhost:50051",
            "miden_endpoint": "https://rpc.testnet.miden.io",
            "accounts": [
                {"label": "alice", "account_id": "0x01", "secret_key_hex": "a"},
                {"label": "bob", "account_id": "0x02", "secret_key_hex": "b"}
            ]
        }"#;
        fs::write(&path, legacy).unwrap();

        let fixture = Fixture::load(&path).unwrap();
        assert_eq!(fixture.accounts[0].state, ProvisioningState::Registered);
    }

    #[tokio::test]
    async fn prepare_refuses_to_silently_skip_an_unregistered_account() {
        // The original resume counted a created-but-unregistered entry as done,
        // so Guardian could hold fewer accounts than the fixture claimed.
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        let mut fixture = fixture_with(2);
        fixture.accounts[1].state = ProvisioningState::Created;
        persist_fixture(&path, &fixture).unwrap();

        let error = prepare(
            fixture.guardian_endpoint.clone(),
            fixture.miden_endpoint.clone(),
            &path,
            2,
            false,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("never registered"));
        assert!(error.to_string().contains("--discard-unregistered"));
    }

    #[tokio::test]
    async fn discard_unregistered_drops_only_the_incomplete_entries() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");
        let mut fixture = fixture_with(3);
        fixture.accounts[2].state = ProvisioningState::Created;
        persist_fixture(&path, &fixture).unwrap();

        // Requesting 2 leaves nothing to create, so this exercises the discard
        // path alone without needing a network.
        let resumed = prepare(
            fixture.guardian_endpoint.clone(),
            fixture.miden_endpoint.clone(),
            &path,
            2,
            true,
        )
        .await
        .unwrap();

        assert_eq!(resumed.accounts.len(), 2);
        assert!(
            resumed
                .accounts
                .iter()
                .all(|account| account.state == ProvisioningState::Registered)
        );
        assert_eq!(resumed.accounts[0].secret_key_hex, "secret-0");
    }

    #[tokio::test]
    async fn prepare_rejects_a_count_below_the_runner_minimum() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("accounts.json");

        assert!(
            prepare(
                "http://localhost:50051".to_string(),
                "https://rpc.testnet.miden.io".to_string(),
                &path,
                1,
                false,
            )
            .await
            .is_err()
        );
    }
}
