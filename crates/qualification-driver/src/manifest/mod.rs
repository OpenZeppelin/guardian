pub mod validate;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::duration::Budget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    Deterministic,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageSource {
    Built,
    Pulled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sdk {
    Rust,
    Typescript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SdkSelector {
    Rust,
    Typescript,
    Both,
}

impl SdkSelector {
    pub fn includes(self, sdk: Sdk) -> bool {
        match self {
            Self::Both => true,
            Self::Rust => sdk == Sdk::Rust,
            Self::Typescript => sdk == Sdk::Typescript,
        }
    }

    pub fn expand(self) -> Vec<Sdk> {
        match self {
            Self::Rust => vec![Sdk::Rust],
            Self::Typescript => vec![Sdk::Typescript],
            Self::Both => vec![Sdk::Rust, Sdk::Typescript],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    Native,
    ServerSide,
    Browser,
}

/// `Mixed` exists so a manifest naming a mixed signer set fails validation with
/// a message about the capability gap, rather than a bare deserialization error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scheme {
    Falcon,
    Ecdsa,
    Mixed,
    #[serde(rename = "n/a")]
    NotApplicable,
}

impl From<String> for Scheme {
    fn from(raw: String) -> Self {
        match raw.as_str() {
            "falcon" => Self::Falcon,
            "ecdsa" => Self::Ecdsa,
            "n/a" | "none" => Self::NotApplicable,
            _ => Self::Mixed,
        }
    }
}

impl<'de> Deserialize<'de> for Scheme {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Shape {
    #[serde(rename = "1-of-1")]
    OneOfOne,
    #[serde(rename = "2-of-3")]
    TwoOfThree,
    #[serde(rename = "3-of-3")]
    ThreeOfThree,
    #[serde(rename = "n/a")]
    NotApplicable,
}

impl Shape {
    pub fn threshold_and_total(self) -> Option<(u32, u32)> {
        match self {
            Self::OneOfOne => Some((1, 1)),
            Self::TwoOfThree => Some((2, 3)),
            Self::ThreeOfThree => Some((3, 3)),
            Self::NotApplicable => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Online,
    Offline,
    #[serde(rename = "n/a")]
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    AccountCreate,
    AccountRegister,
    AccountRecoverByCosigner,
    ProposalCreate,
    ProposalCreateOffline,
    ProposalExport,
    ProposalSign,
    ProposalSignExternal,
    ProposalImport,
    ProposalExecute,
    ProposalRejectBelowThreshold,
    ProposalRejectDuplicateSignature,
    SignerAdd,
    SignerRemove,
    ThresholdChange,
    SignerSetAssert,
    SignerRemovedRefused,
    AssetSend,
    AssetSendAssert,
    ProcedureThresholdSet,
    ProcedureThresholdAssert,
    AssetTransfer,
    NoteConsume,
    BalanceAssert,
    CommitmentVerify,
    GuardianMigrate,
    GuardianSwitchOnline,
    GuardianSwitchAssert,
    HandoffRustToTs,
    HandoffTsToRust,
    StatusIdentity,
    ErrorEnvelope,
    RestartDurability,
    DiscardedDeltaHidden,
    AbandonAndAssertHidden,
    P2ideSend,
    P2ideTimelockAssert,
    OperatorSession,
    OperatorAccounts,
    OperatorDenial,
    OperatorAllowlistReload,
    OperatorLogout,
    OperatorAudit,
    SchemeGate,
    AccountPausedRefuses,
    PausedRefusesExecution,
    CustomProposalCreate,
    CustomProposalAssert,
    CustomProposalPrepare,
    Unknown(String),
}

impl From<String> for Action {
    fn from(raw: String) -> Self {
        match raw.as_str() {
            "account-create" => Self::AccountCreate,
            "account-register" => Self::AccountRegister,
            "account-recover-by-cosigner" => Self::AccountRecoverByCosigner,
            "proposal-create" => Self::ProposalCreate,
            "proposal-create-offline" => Self::ProposalCreateOffline,
            "proposal-export" => Self::ProposalExport,
            "proposal-sign" => Self::ProposalSign,
            "proposal-sign-external" => Self::ProposalSignExternal,
            "proposal-import" => Self::ProposalImport,
            "proposal-execute" => Self::ProposalExecute,
            "proposal-reject-below-threshold" => Self::ProposalRejectBelowThreshold,
            "proposal-reject-duplicate-signature" => Self::ProposalRejectDuplicateSignature,
            "signer-add" => Self::SignerAdd,
            "signer-remove" => Self::SignerRemove,
            "threshold-change" => Self::ThresholdChange,
            "signer-set-assert" => Self::SignerSetAssert,
            "signer-removed-refused" => Self::SignerRemovedRefused,
            "asset-send" => Self::AssetSend,
            "asset-send-assert" => Self::AssetSendAssert,
            "procedure-threshold-set" => Self::ProcedureThresholdSet,
            "procedure-threshold-assert" => Self::ProcedureThresholdAssert,
            "asset-transfer" => Self::AssetTransfer,
            "note-consume" => Self::NoteConsume,
            "balance-assert" => Self::BalanceAssert,
            "commitment-verify" => Self::CommitmentVerify,
            "guardian-migrate" => Self::GuardianMigrate,
            "guardian-switch-online" => Self::GuardianSwitchOnline,
            "guardian-switch-assert" => Self::GuardianSwitchAssert,
            "handoff-rust-to-ts" => Self::HandoffRustToTs,
            "handoff-ts-to-rust" => Self::HandoffTsToRust,
            "status-identity" => Self::StatusIdentity,
            "error-envelope" => Self::ErrorEnvelope,
            "restart-durability" => Self::RestartDurability,
            "discarded-delta-hidden" => Self::DiscardedDeltaHidden,
            "abandon-and-assert-hidden" => Self::AbandonAndAssertHidden,
            "p2ide-send" => Self::P2ideSend,
            "p2ide-timelock-assert" => Self::P2ideTimelockAssert,
            "operator-session" => Self::OperatorSession,
            "operator-accounts" => Self::OperatorAccounts,
            "operator-denial" => Self::OperatorDenial,
            "operator-allowlist-reload" => Self::OperatorAllowlistReload,
            "operator-logout" => Self::OperatorLogout,
            "operator-audit" => Self::OperatorAudit,
            "scheme-gate" => Self::SchemeGate,
            "account-paused-refuses" => Self::AccountPausedRefuses,
            "paused-refuses-execution" => Self::PausedRefusesExecution,
            "custom-proposal-create" => Self::CustomProposalCreate,
            "custom-proposal-assert" => Self::CustomProposalAssert,
            "custom-proposal-prepare" => Self::CustomProposalPrepare,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

impl Action {
    /// Guardian migration is the only proposal type that can be created while
    /// offline; every other type must be created online before it can be
    /// exported.
    pub fn permits_offline_creation(&self) -> bool {
        matches!(self, Self::GuardianMigrate | Self::ProposalCreateOffline)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    pub profile: Profile,
    pub sdk: SdkSelector,
    #[serde(default)]
    pub runtime: Option<Runtime>,
    pub scheme: Scheme,
    pub shape: Shape,
    pub mode: Mode,
    pub actions: Vec<Action>,
    pub step_budget: Budget,
    pub required: bool,
    /// Part of the reduced set a reviewer can request on a pull request.
    #[serde(default)]
    pub core: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ScenarioFile {
    #[serde(default, rename = "scenario")]
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkName {
    Devnet,
    Testnet,
}

impl NetworkName {
    pub fn to_network_id(self) -> miden_protocol::address::NetworkId {
        match self {
            Self::Devnet => miden_protocol::address::NetworkId::Devnet,
            Self::Testnet => miden_protocol::address::NetworkId::Testnet,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Devnet => "devnet",
            Self::Testnet => "testnet",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    pub name: NetworkName,
    pub rpc_endpoint: String,
    pub historical_window: Budget,
}

/// `Unavailable` is reserved for a limitation believed to be structural. A
/// network that is merely down stays available and reports environment-blocked
/// at run time, so recovery needs no manifest edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Pair {
    pub network: NetworkName,
    pub sdk: Sdk,
    pub availability: Availability,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub excluded_scenarios: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MatrixFile {
    #[serde(default, rename = "network")]
    pub networks: Vec<Network>,
    #[serde(default, rename = "pair")]
    pub pairs: Vec<Pair>,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub scenarios: Vec<Scenario>,
    pub networks: Vec<Network>,
    pub pairs: Vec<Pair>,
}

impl Manifest {
    /// The (scenario, sdk) pairs a run must pass to claim qualification.
    ///
    /// Shared by the runner and the report merger so a claim derived after
    /// merging both SDKs' results is computed the same way as one derived
    /// during a single-SDK run.
    pub fn required_entries(
        &self,
        profile: Profile,
        network: Option<NetworkName>,
        sdk: Option<Sdk>,
    ) -> Vec<(String, Sdk)> {
        let mut entries: Vec<(String, Sdk)> = match profile {
            Profile::Deterministic => sdk
                .map(|sdk| vec![sdk])
                .unwrap_or(vec![Sdk::Rust, Sdk::Typescript])
                .into_iter()
                .flat_map(|sdk| {
                    self.required_deterministic(sdk)
                        .into_iter()
                        .map(move |scenario| (scenario.id.clone(), sdk))
                        .collect::<Vec<_>>()
                })
                .collect(),
            Profile::Live => {
                let Some(network) = network else {
                    return Vec::new();
                };
                self.pairs
                    .iter()
                    .filter(|pair| pair.network == network)
                    .filter(|pair| sdk.is_none_or(|sdk| sdk == pair.sdk))
                    .flat_map(|pair| {
                        let sdk = pair.sdk;
                        self.required_scenarios(pair)
                            .into_iter()
                            .map(move |scenario| (scenario.id.clone(), sdk))
                    })
                    .collect()
            }
        };
        entries.sort();
        entries.dedup();
        entries
    }

    /// The generated form the TypeScript driver consumes. Committed alongside
    /// the sources and guarded against drift by a test.
    pub fn to_export_json(&self) -> anyhow::Result<String> {
        let exported = serde_json::json!({
            "scenarios": self.scenarios,
            "networks": self.networks,
        });
        Ok(format!("{}\n", serde_json::to_string_pretty(&exported)?))
    }

    pub fn load(scenarios_path: &Path, matrix_path: &Path) -> anyhow::Result<Self> {
        let scenarios_raw = std::fs::read_to_string(scenarios_path)?;
        let matrix_raw = std::fs::read_to_string(matrix_path)?;
        Self::from_toml(&scenarios_raw, &matrix_raw)
    }

    pub fn from_toml(scenarios_raw: &str, matrix_raw: &str) -> anyhow::Result<Self> {
        let scenarios: ScenarioFile = toml::from_str(scenarios_raw)?;
        let matrix: MatrixFile = toml::from_str(matrix_raw)?;
        Ok(Self {
            scenarios: scenarios.scenarios,
            networks: matrix.networks,
            pairs: matrix.pairs,
        })
    }

    pub fn scenario(&self, id: &str) -> Option<&Scenario> {
        self.scenarios.iter().find(|scenario| scenario.id == id)
    }

    pub fn network(&self, name: NetworkName) -> Option<&Network> {
        self.networks.iter().find(|network| network.name == name)
    }

    /// The scenarios a pair must pass for a run against it to claim full
    /// qualification: every eligible live scenario for that SDK, minus the ones
    /// the pair explicitly excludes. Pairs describe live networks, so
    /// deterministic scenarios never belong to one.
    pub fn required_scenarios(&self, pair: &Pair) -> Vec<&Scenario> {
        self.scenarios
            .iter()
            .filter(|scenario| scenario.required)
            .filter(|scenario| scenario.profile == Profile::Live)
            .filter(|scenario| scenario.sdk.includes(pair.sdk))
            .filter(|scenario| !pair.excluded_scenarios.contains(&scenario.id))
            .collect()
    }

    pub fn required_deterministic(&self, sdk: Sdk) -> Vec<&Scenario> {
        self.scenarios
            .iter()
            .filter(|scenario| scenario.required)
            .filter(|scenario| scenario.profile == Profile::Deterministic)
            .filter(|scenario| scenario.sdk.includes(sdk))
            .collect()
    }

    pub fn required_by_pair(&self) -> BTreeMap<(NetworkName, Sdk), Vec<String>> {
        self.pairs
            .iter()
            .map(|pair| {
                let ids = self
                    .required_scenarios(pair)
                    .into_iter()
                    .map(|scenario| scenario.id.clone())
                    .collect();
                ((pair.network, pair.sdk), ids)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TypeScript driver consumes the JSON form of this manifest, so a
    /// serializer that emits a spelling the deserializer does not accept would
    /// break it silently.
    #[test]
    fn enum_spellings_round_trip_through_json() {
        for scheme in [
            Scheme::Falcon,
            Scheme::Ecdsa,
            Scheme::Mixed,
            Scheme::NotApplicable,
        ] {
            let rendered = serde_json::to_string(&scheme).expect("serializes");
            let parsed: Scheme = serde_json::from_str(&rendered).expect("round-trips");
            assert_eq!(
                parsed, scheme,
                "scheme spelling {rendered} does not round-trip"
            );
        }

        for mode in [Mode::Online, Mode::Offline, Mode::NotApplicable] {
            let rendered = serde_json::to_string(&mode).expect("serializes");
            let parsed: Mode = serde_json::from_str(&rendered).expect("round-trips");
            assert_eq!(parsed, mode);
        }

        for shape in [
            Shape::OneOfOne,
            Shape::TwoOfThree,
            Shape::ThreeOfThree,
            Shape::NotApplicable,
        ] {
            let rendered = serde_json::to_string(&shape).expect("serializes");
            let parsed: Shape = serde_json::from_str(&rendered).expect("round-trips");
            assert_eq!(parsed, shape);
        }

        for action in [
            Action::AccountCreate,
            Action::ProposalCreateOffline,
            Action::HandoffRustToTs,
            Action::OperatorAllowlistReload,
        ] {
            let rendered = serde_json::to_string(&action).expect("serializes");
            let parsed: Action = serde_json::from_str(&rendered).expect("round-trips");
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn the_generated_manifest_json_is_up_to_date() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("qualification/manifest");
        let manifest = Manifest::load(&root.join("scenarios.toml"), &root.join("matrix.toml"))
            .expect("the committed manifest loads");
        let expected = manifest.to_export_json().expect("exports");
        let committed = std::fs::read_to_string(root.join("manifest.json"))
            .expect("qualification/manifest/manifest.json is committed");
        assert_eq!(
            committed, expected,
            "regenerate with: cargo run -p guardian-qualification-driver -- \
             export-manifest --out qualification/manifest/manifest.json"
        );
    }

    #[test]
    fn the_committed_manifest_loads_and_validates() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("qualification/manifest");
        let manifest = Manifest::load(&root.join("scenarios.toml"), &root.join("matrix.toml"))
            .expect("the committed manifest loads");
        validate::validate(&manifest).expect("the committed manifest validates");
        assert!(!manifest.scenarios.is_empty());
    }
}
