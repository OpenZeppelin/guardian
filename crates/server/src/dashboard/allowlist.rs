use std::collections::{BTreeSet, HashMap, HashSet};
use std::env;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::PublicKey;
use serde::Deserialize;

use super::permissions::Permission;
use super::types::AuthenticatedOperator;

/// Wire shape of a single operator allowlist array element: either a
/// bare hex string (legacy, `{dashboard:read}` only — FR-002) or a
/// structured object with an explicit permission set (FR-001). Mixed
/// arrays of the two shapes are permitted in one document.
///
/// Dispatched manually on `serde_json::Value` rather than via
/// `#[serde(untagged)]` so failures name the offending entry
/// ("entry 3 is missing field `permissions`") instead of "data did
/// not match any variant" — the load is operator-edited config, and
/// the error reaches deployment operators directly.
enum AllowlistEntryWire {
    LegacyHex(String),
    Structured(StructuredEntry),
}

/// Object-shape element. `#[serde(deny_unknown_fields)]` rejects any
/// unknown JSON property (e.g. `comment`, `role`) so a typo or a
/// forward-incompatible schema extension surfaces at load time rather
/// than silently degrading. FR-001 update.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StructuredEntry {
    public_key: String,
    permissions: Vec<String>,
}

impl AllowlistEntryWire {
    fn from_value(value: serde_json::Value) -> std::result::Result<Self, String> {
        match value {
            serde_json::Value::String(hex) => Ok(Self::LegacyHex(hex)),
            obj @ serde_json::Value::Object(_) => serde_json::from_value::<StructuredEntry>(obj)
                .map(Self::Structured)
                .map_err(|err| err.to_string()),
            other => Err(format!(
                "expected a hex string or `{{public_key, permissions}}` object, got {}",
                shape_name(&other)
            )),
        }
    }
}

fn shape_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Object(_) => "object",
    }
}

pub(crate) const ENV_OPERATOR_PUBLIC_KEYS_FILE: &str = "GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE";
pub(crate) const ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID: &str =
    "GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID";
const ENV_AWS_REGION: &str = "AWS_REGION";

#[derive(Clone)]
pub(crate) enum AllowlistSource {
    Static,
    File(PathBuf),
    AwsSecret {
        secret_id: String,
        client: SecretsManagerClient,
    },
}

impl fmt::Debug for AllowlistSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static => formatter.debug_tuple("Static").finish(),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
            Self::AwsSecret { secret_id, .. } => formatter
                .debug_struct("AwsSecret")
                .field("secret_id", secret_id)
                .finish_non_exhaustive(),
        }
    }
}

impl AllowlistSource {
    pub(crate) async fn from_env() -> std::result::Result<Self, String> {
        if let Ok(secret_id) = env::var(ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID) {
            let secret_id = secret_id.trim();
            if !secret_id.is_empty() {
                ensure_aws_region()?;
                let config = aws_config::defaults(BehaviorVersion::latest()).load().await;
                return Ok(Self::AwsSecret {
                    secret_id: secret_id.to_string(),
                    client: SecretsManagerClient::new(&config),
                });
            }
        }

        match env::var(ENV_OPERATOR_PUBLIC_KEYS_FILE) {
            Ok(path) if !path.trim().is_empty() => Ok(Self::File(PathBuf::from(path.trim()))),
            _ => Ok(Self::Static),
        }
    }

    pub(crate) async fn load(&self) -> std::result::Result<OperatorAllowlist, String> {
        match self {
            Self::Static => Ok(OperatorAllowlist::default()),
            Self::File(path) => {
                let json = tokio::fs::read_to_string(path).await.map_err(|error| {
                    format!(
                        "Failed to read {ENV_OPERATOR_PUBLIC_KEYS_FILE} file {}: {error}",
                        path.display()
                    )
                })?;
                OperatorAllowlist::from_json(
                    &format!("{}={}", ENV_OPERATOR_PUBLIC_KEYS_FILE, path.display()),
                    &json,
                )
            }
            Self::AwsSecret { secret_id, client } => {
                let response = client
                    .get_secret_value()
                    .secret_id(secret_id)
                    .send()
                    .await
                    .map_err(|error| {
                        format!(
                            "Failed to load {ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID} {secret_id} from Secrets Manager: {error}"
                        )
                    })?;
                let json = response.secret_string().ok_or_else(|| {
                    format!("Secret {secret_id} does not contain a secret string value")
                })?;
                OperatorAllowlist::from_json(
                    &format!("{ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID}={secret_id}"),
                    json,
                )
            }
        }
    }

    pub(crate) async fn load_dynamic(
        &self,
    ) -> std::result::Result<Option<OperatorAllowlist>, String> {
        match self {
            Self::Static => Ok(None),
            Self::File(_) | Self::AwsSecret { .. } => self.load().await.map(Some),
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Static => "static".to_string(),
            Self::File(path) => format!("{ENV_OPERATOR_PUBLIC_KEYS_FILE}={}", path.display()),
            Self::AwsSecret { secret_id, .. } => {
                format!("{ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID}={secret_id}")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct OperatorAllowlist {
    by_commitment: HashMap<String, AuthenticatedOperator>,
}

impl OperatorAllowlist {
    /// Parse the heterogeneous allowlist JSON (feature 006-operator-authz).
    /// Each array element is either a bare hex string (legacy
    /// `{dashboard:read}`) or `{ "public_key", "permissions" }`. Mixed
    /// arrays are permitted. Returns a deterministic error on duplicate
    /// commitments, unknown permission strings, or malformed objects.
    pub(crate) fn from_json(source_label: &str, json: &str) -> std::result::Result<Self, String> {
        let raw_entries: Vec<serde_json::Value> = serde_json::from_str(json)
            .map_err(|error| format!("Failed to parse {source_label}: {error}"))?;

        let mut by_commitment = HashMap::with_capacity(raw_entries.len());
        let mut commitments = HashSet::with_capacity(raw_entries.len());

        for (index, raw) in raw_entries.into_iter().enumerate() {
            let entry = AllowlistEntryWire::from_value(raw)
                .map_err(|error| format!("{source_label} entry {}: {error}", index + 1))?;
            let (public_key_hex, permission_strings, is_legacy) = match entry {
                AllowlistEntryWire::LegacyHex(hex) => (hex, Vec::<String>::new(), true),
                AllowlistEntryWire::Structured(StructuredEntry {
                    public_key,
                    permissions,
                }) => (public_key, permissions, false),
            };

            let public_key_hex = public_key_hex.trim();
            if public_key_hex.is_empty() {
                return Err(format!(
                    "{source_label} entry {} must not be empty",
                    index + 1
                ));
            }

            let public_key = PublicKey::from_hex(public_key_hex).map_err(|error| {
                format!(
                    "Failed to parse {source_label} entry {}: {error}",
                    index + 1
                )
            })?;
            let commitment = public_key.to_commitment().into_hex();
            if !commitments.insert(commitment.clone()) {
                return Err(format!(
                    "Duplicate operator public key commitment in {source_label}: {commitment}"
                ));
            }

            // Map permission strings to the v1 vocabulary. Legacy hex
            // entries skip parsing and take the `{dashboard:read}`
            // default; structured entries with `permissions: []` get
            // an empty set (FR-003 — explicit revocation).
            let mut effective: BTreeSet<Permission> = BTreeSet::new();
            if is_legacy {
                effective.insert(Permission::DashboardRead);
            } else {
                for raw in &permission_strings {
                    let permission = Permission::from_str(raw).map_err(|err| {
                        format!(
                            "{source_label} entry {} has unknown permission {}: {err}",
                            index + 1,
                            err.0
                        )
                    })?;
                    effective.insert(permission); // BTreeSet dedupes (FR-006)
                }
            }

            by_commitment.insert(
                commitment.clone(),
                AuthenticatedOperator {
                    operator_id: commitment.clone(),
                    commitment,
                    effective_permissions: Arc::new(effective),
                },
            );
        }

        Ok(Self { by_commitment })
    }

    pub(crate) fn from_entries(
        entries: Vec<OperatorAllowlistEntryInput>,
    ) -> std::result::Result<Self, String> {
        let mut by_commitment = HashMap::with_capacity(entries.len());
        let mut operator_ids = HashSet::with_capacity(entries.len());
        let mut commitments = HashSet::with_capacity(entries.len());

        for entry in entries {
            if entry.operator_id.trim().is_empty() {
                return Err(
                    "Operator allowlist entries must have a non-empty operator_id".to_string(),
                );
            }

            let normalized_commitment = normalize_commitment(&entry.commitment)?;
            if !operator_ids.insert(entry.operator_id.clone()) {
                return Err(format!(
                    "Duplicate operator identifier in allowlist: {}",
                    entry.operator_id
                ));
            }
            if !commitments.insert(normalized_commitment.clone()) {
                return Err(format!(
                    "Duplicate operator commitment in allowlist: {}",
                    normalized_commitment
                ));
            }

            // `from_entries` is the structured-input path used by
            // tests and internal callers; treat it as legacy-grant
            // (`{dashboard:read}`) so existing call sites need no
            // change. Heterogeneous-permission entries flow through
            // `from_json` instead.
            let mut effective = BTreeSet::new();
            effective.insert(Permission::DashboardRead);
            by_commitment.insert(
                normalized_commitment.clone(),
                AuthenticatedOperator {
                    operator_id: entry.operator_id,
                    commitment: normalized_commitment,
                    effective_permissions: Arc::new(effective),
                },
            );
        }

        Ok(Self { by_commitment })
    }

    pub(crate) fn lookup(&self, commitment: &str) -> Option<&AuthenticatedOperator> {
        self.by_commitment.get(commitment)
    }

    pub(crate) fn len(&self) -> usize {
        self.by_commitment.len()
    }

    /// Test-only constructor that bypasses `from_entries`'s
    /// legacy-grant default and lets integration tests assemble an
    /// allowlist of operators with arbitrary permission sets. Used by
    /// feature 006-operator-authz US1 / US2 tests to exercise both
    /// the `{dashboard:read}` and the `permissions: []` paths through
    /// the new authorization middleware.
    #[cfg(test)]
    pub(crate) fn from_authenticated_operators(
        operators: Vec<AuthenticatedOperator>,
    ) -> std::result::Result<Self, String> {
        let mut by_commitment = HashMap::with_capacity(operators.len());
        for op in operators {
            if by_commitment.insert(op.commitment.clone(), op).is_some() {
                return Err("Duplicate operator commitment in test allowlist".into());
            }
        }
        Ok(Self { by_commitment })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OperatorAllowlistEntryInput {
    pub(crate) operator_id: String,
    pub(crate) commitment: String,
}

pub(crate) fn normalize_commitment(commitment: &str) -> std::result::Result<String, String> {
    Word::from_hex(commitment).map(|parsed| parsed.into_hex())
}

fn ensure_aws_region() -> std::result::Result<(), String> {
    match env::var(ENV_AWS_REGION) {
        Ok(value) if !value.trim().is_empty() => Ok(()),
        Ok(_) => Err(format!(
            "{ENV_AWS_REGION} must not be empty when {ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID} is set"
        )),
        Err(env::VarError::NotPresent) => Err(format!(
            "{ENV_AWS_REGION} is required when {ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID} is set"
        )),
        Err(env::VarError::NotUnicode(_)) => {
            Err(format!("{ENV_AWS_REGION} must contain valid UTF-8"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::helpers::TestSigner;

    fn pk(signer: &TestSigner) -> &str {
        &signer.pubkey_hex
    }

    /// FR-001 + FR-002: legacy bare-hex array still loads, every entry
    /// gets `{dashboard:read}` only.
    #[test]
    fn legacy_array_loads_with_dashboard_read_grant() {
        let signer = TestSigner::new();
        let json = format!("[{:?}]", pk(&signer));
        let allowlist = OperatorAllowlist::from_json("test", &json).unwrap();
        assert_eq!(allowlist.len(), 1);
        let op = allowlist.lookup(&signer.commitment_hex).unwrap();
        let perms: Vec<Permission> = op.effective_permissions.iter().copied().collect();
        assert_eq!(perms, vec![Permission::DashboardRead]);
    }

    /// FR-001: mixed array of bare hex + structured object loads.
    #[test]
    fn mixed_array_loads_independently() {
        let signer_a = TestSigner::new();
        let signer_b = TestSigner::new();
        let json = format!(
            r#"[{:?}, {{"public_key": {:?}, "permissions": ["dashboard:read", "accounts:pause"]}}]"#,
            pk(&signer_a),
            pk(&signer_b),
        );
        let allowlist = OperatorAllowlist::from_json("test", &json).unwrap();
        assert_eq!(allowlist.len(), 2);

        let a = allowlist.lookup(&signer_a.commitment_hex).unwrap();
        assert_eq!(
            a.effective_permissions.iter().copied().collect::<Vec<_>>(),
            vec![Permission::DashboardRead]
        );

        let b = allowlist.lookup(&signer_b.commitment_hex).unwrap();
        let b_perms: Vec<Permission> = b.effective_permissions.iter().copied().collect();
        // BTreeSet ordering — alphabetic over the enum's Ord impl
        // (DashboardRead < AccountsPause? Actually the Ord is derived
        // from declaration order: DashboardRead=0, AccountsPause=1).
        assert_eq!(
            b_perms,
            vec![Permission::DashboardRead, Permission::AccountsPause]
        );
    }

    /// FR-003: object entry with `permissions: []` loads with empty
    /// set — explicit revocation, not legacy-grant.
    #[test]
    fn object_with_empty_permissions_loads_as_empty_set() {
        let signer = TestSigner::new();
        let json = format!(
            r#"[{{"public_key": {:?}, "permissions": []}}]"#,
            pk(&signer)
        );
        let allowlist = OperatorAllowlist::from_json("test", &json).unwrap();
        let op = allowlist.lookup(&signer.commitment_hex).unwrap();
        assert!(op.effective_permissions.is_empty());
    }

    /// FR-004: unknown permission string rejects the load.
    #[test]
    fn unknown_permission_string_rejected() {
        let signer = TestSigner::new();
        let json = format!(
            r#"[{{"public_key": {:?}, "permissions": ["accounts:freeze"]}}]"#,
            pk(&signer)
        );
        let err = OperatorAllowlist::from_json("test", &json).unwrap_err();
        assert!(
            err.contains("accounts:freeze"),
            "expected unknown permission in error: {err}"
        );
    }

    /// FR-005: case mismatch and trailing whitespace are unknown, not
    /// coerced.
    #[test]
    fn case_mismatch_rejected() {
        let signer = TestSigner::new();
        let json = format!(
            r#"[{{"public_key": {:?}, "permissions": ["Accounts:Pause"]}}]"#,
            pk(&signer)
        );
        assert!(OperatorAllowlist::from_json("test", &json).is_err());
    }

    #[test]
    fn whitespace_in_permission_rejected() {
        let signer = TestSigner::new();
        let json = format!(
            r#"[{{"public_key": {:?}, "permissions": [" accounts:pause"]}}]"#,
            pk(&signer)
        );
        assert!(OperatorAllowlist::from_json("test", &json).is_err());
    }

    /// FR-001: object with unknown property rejected, and the error
    /// names the offending entry index + field.
    #[test]
    fn unknown_object_property_rejected() {
        let signer = TestSigner::new();
        let json = format!(
            r#"[{{"public_key": {:?}, "permissions": [], "comment": "test"}}]"#,
            pk(&signer)
        );
        let err = OperatorAllowlist::from_json("test", &json).unwrap_err();
        assert!(err.contains("entry 1"), "expected entry index in: {err}");
        assert!(err.contains("comment"), "expected field name in: {err}");
    }

    /// FR-006: duplicate permissions within one entry dedupe.
    #[test]
    fn duplicate_permissions_within_entry_dedupe() {
        let signer = TestSigner::new();
        let json = format!(
            r#"[{{"public_key": {:?}, "permissions": ["dashboard:read", "dashboard:read"]}}]"#,
            pk(&signer)
        );
        let allowlist = OperatorAllowlist::from_json("test", &json).unwrap();
        let op = allowlist.lookup(&signer.commitment_hex).unwrap();
        assert_eq!(op.effective_permissions.len(), 1);
    }

    /// FR-007: duplicate `public_key` across two array elements
    /// rejects the load.
    #[test]
    fn duplicate_public_key_across_entries_rejected() {
        let signer = TestSigner::new();
        let json = format!(r#"[{:?}, {:?}]"#, pk(&signer), pk(&signer));
        let err = OperatorAllowlist::from_json("test", &json).unwrap_err();
        assert!(err.contains("Duplicate"), "expected duplicate error: {err}");
    }

    /// Edge Case 4: object entry missing `permissions` is rejected as
    /// malformed; the error names the entry index and the missing field.
    #[test]
    fn object_missing_permissions_rejected() {
        let signer = TestSigner::new();
        let json = format!(r#"[{{"public_key": {:?}}}]"#, pk(&signer));
        let err = OperatorAllowlist::from_json("test", &json).unwrap_err();
        assert!(err.contains("entry 1"), "expected entry index in: {err}");
        assert!(
            err.contains("permissions"),
            "expected missing-field name in: {err}",
        );
    }

    /// FR-001: a non-string, non-object array element (e.g. number)
    /// is rejected with a clear shape diagnostic.
    #[test]
    fn wrong_shape_entry_rejected_with_shape_name() {
        let err = OperatorAllowlist::from_json("test", "[42]").unwrap_err();
        assert!(err.contains("entry 1"), "expected entry index in: {err}");
        assert!(err.contains("number"), "expected shape name in: {err}");
    }
}
