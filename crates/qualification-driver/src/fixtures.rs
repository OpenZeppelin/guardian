use anyhow::{Context, anyhow};
use guardian_shared::FromJson;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use miden_multisig_client::ProposalPayload;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::Deserializable;
use serde_json::Value;

/// The server's committed test fixtures: a real multisig account bound to a
/// known guardian identity, with the cosigner keys that authorize it. The
/// deterministic profile registers this account, so the server it runs against
/// must carry the matching acknowledgement identity.
const FIXTURE_DIR: &str = "crates/server/src/testing/fixtures";

pub struct Fixtures {
    pub account: Value,
    pub account_id: String,
    /// The commitment the fixture account carries before any delta is applied,
    /// which is the state the deterministic profile registers and reads back.
    pub initial_commitment: String,
    pub cosigner_commitments: Vec<String>,
    pub delta: Value,
    signer_key: SecretKey,
    /// The operator identity the allowlist grants `accounts:pause`, kept whole
    /// so a dashboard challenge can be signed rather than only recognised.
    operator_key: SecretKey,
}

impl Fixtures {
    pub fn load(repo_root: &std::path::Path) -> anyhow::Result<Self> {
        let dir = repo_root.join(FIXTURE_DIR);
        let read = |name: &str| -> anyhow::Result<Value> {
            let path = dir.join(name);
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Ok(serde_json::from_str(&raw)?)
        };

        let keys = read("keys.json")?;
        let commitments = read("commitments.json")?;
        let account = read("account.json")?;
        let delta = read("delta_1.json")?;

        let account_id = commitments["account_id"]
            .as_str()
            .ok_or_else(|| anyhow!("commitments.json has no account_id"))?
            .to_string();

        let initial_commitment = commitments["initial_commitment"]
            .as_str()
            .ok_or_else(|| anyhow!("commitments.json has no initial_commitment"))?
            .to_string();

        let secret_hex = keys["signer_1_secret_key"]
            .as_str()
            .ok_or_else(|| anyhow!("keys.json has no signer_1_secret_key"))?;
        let signer_key = SecretKey::read_from_bytes(&hex::decode(secret_hex)?)
            .map_err(|error| anyhow!("signer_1_secret_key is not a Falcon key: {error}"))?;

        let cosigner_commitments = (1..=3)
            .map(|index| {
                keys[format!("signer_{index}_commitment")]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("keys.json has no signer_{index}_commitment"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let operator_hex = keys["signer_4_secret_key"]
            .as_str()
            .ok_or_else(|| anyhow!("keys.json has no signer_4_secret_key"))?;
        let operator_key = SecretKey::read_from_bytes(&hex::decode(operator_hex)?)
            .map_err(|error| anyhow!("signer_4 is not a Falcon key: {error}"))?;

        Ok(Self {
            account,
            account_id,
            initial_commitment,
            cosigner_commitments,
            delta,
            signer_key,
            operator_key,
        })
    }

    pub fn guardian_secret_key_hex(repo_root: &std::path::Path) -> anyhow::Result<String> {
        let raw = std::fs::read_to_string(repo_root.join(FIXTURE_DIR).join("keys.json"))?;
        let keys: Value = serde_json::from_str(&raw)?;
        keys["guardian_secret_key"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("keys.json has no guardian_secret_key"))
    }

    /// The operator key and its commitment, as the dashboard expects them.
    pub fn operator(&self) -> (&SecretKey, String) {
        let commitment = format!(
            "0x{}",
            hex::encode(miden_protocol::utils::serde::Serializable::to_bytes(
                &self.operator_key.public_key().to_commitment()
            ))
        );
        (&self.operator_key, commitment)
    }

    pub fn signer_public_key_hex(&self) -> String {
        self.signer_key.public_key().into_hex()
    }

    pub fn signer_commitment_hex(&self) -> String {
        self.signer_key.public_key().to_commitment().into_hex()
    }

    /// The proposal payload a real client sends: the SDK's own
    /// [`ProposalPayload`] carrying the generated fixture's transaction
    /// summary. Built through the SDK type rather than hand-written JSON so
    /// the scenario cannot drift from the wire shape the multisig client
    /// actually produces.
    pub fn proposal_payload(&self) -> anyhow::Result<ProposalPayload> {
        let summary = TransactionSummary::from_json(&self.delta["delta_payload"])
            .map_err(|error| anyhow!("the fixture transaction summary does not load: {error}"))?;
        Ok(ProposalPayload::new(&summary).with_custom_metadata("qualification".to_string()))
    }

    pub fn proposal_nonce(&self) -> u64 {
        self.delta["nonce"].as_u64().unwrap_or(1)
    }

    pub fn sign_account_request(&self, timestamp: i64) -> anyhow::Result<String> {
        let message = AuthRequestMessage::from_account_id_hex(
            &self.account_id,
            timestamp,
            AuthRequestPayload::empty(),
        )
        .map_err(|error| anyhow!("building the auth message: {error}"))?
        .to_word();
        let signature = self.signer_key.sign(message);
        Ok(format!(
            "0x{}",
            hex::encode(miden_protocol::utils::serde::Serializable::to_bytes(
                &signature
            ))
        ))
    }

    pub fn signer(&self) -> anyhow::Result<guardian_client::FalconKeyStore> {
        let bytes = miden_protocol::utils::serde::Serializable::to_bytes(&self.signer_key);
        let key = SecretKey::read_from_bytes(&bytes)
            .map_err(|error| anyhow!("cloning the fixture signer: {error}"))?;
        Ok(guardian_client::FalconKeyStore::new(key))
    }
}

/// Operator identities for the dashboard scenarios, taken from the server
/// fixtures so the allowlist the stack writes and the keys the scenarios sign
/// with cannot drift apart.
pub fn operator_public_keys(repo_root: &std::path::Path) -> anyhow::Result<(String, String)> {
    let raw = std::fs::read_to_string(repo_root.join(FIXTURE_DIR).join("keys.json"))?;
    let keys: Value = serde_json::from_str(&raw)?;
    let read = |index: usize| -> anyhow::Result<String> {
        let hex_key = keys[format!("signer_{index}_secret_key")]
            .as_str()
            .ok_or_else(|| anyhow!("keys.json has no signer_{index}_secret_key"))?;
        let key = SecretKey::read_from_bytes(&hex::decode(hex_key)?)
            .map_err(|error| anyhow!("signer_{index} is not a Falcon key: {error}"))?;
        Ok(key.public_key().into_hex())
    };
    Ok((read(4)?, read(5)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_server_fixtures_load() {
        let fixtures = Fixtures::load(&repo_root()).expect("fixtures load");
        assert!(fixtures.account_id.starts_with("0x"));
        assert_eq!(fixtures.cosigner_commitments.len(), 3);
        assert!(fixtures.account.is_object());
    }

    #[test]
    fn the_fixture_signer_produces_a_signature() {
        let fixtures = Fixtures::load(&repo_root()).expect("fixtures load");
        let signature = fixtures.sign_account_request(1).expect("signs");
        assert!(signature.starts_with("0x"));
        assert!(signature.len() > 64);
    }

    /// The fixture account binds this guardian identity, so a server generating
    /// its own acknowledgement key rejects the registration.
    #[test]
    fn the_fixture_signer_derives_the_recorded_commitment() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let raw =
            std::fs::read_to_string(root.join("crates/server/src/testing/fixtures/keys.json"))
                .expect("keys.json");
        let keys: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        let fixtures = Fixtures::load(&root).expect("fixtures");
        assert_eq!(
            fixtures.signer_commitment_hex(),
            keys["signer_1_commitment"].as_str().unwrap(),
            "the fixture signer key must derive the commitment keys.json records"
        );
    }

    #[test]
    fn the_fixture_guardian_key_is_available() {
        let key = Fixtures::guardian_secret_key_hex(&repo_root()).expect("guardian key");
        assert!(!key.is_empty());
        assert!(hex::decode(&key).is_ok());
    }
}
