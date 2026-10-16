//! The module catalog: instances declared in the file at `GUARDIAN_MODULES_CONFIG`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use guardian_module_sdk::Hook;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const CONFIG_ENV: &str = "GUARDIAN_MODULES_CONFIG";
const TIMEOUT_MAX_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    All,
    Assigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    #[default]
    Deny,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookName {
    BeforeTransactionApproved,
    BeforeProposalCreated,
    BeforeAccountRegistered,
}

impl HookName {
    pub fn proto(self) -> Hook {
        match self {
            HookName::BeforeTransactionApproved => Hook::BeforeTransactionApproved,
            HookName::BeforeProposalCreated => Hook::BeforeProposalCreated,
            HookName::BeforeAccountRegistered => Hook::BeforeAccountRegistered,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthEntry {
    token_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsEntry {
    ca_file: Option<PathBuf>,
    client_cert_file: Option<PathBuf>,
    client_key_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceEntry {
    id: String,
    kind: String,
    endpoint: Option<String>,
    scope: Scope,
    hooks: Vec<HookName>,
    timeout_ms: u64,
    #[serde(default)]
    on_error: OnError,
    #[serde(default = "enabled_default")]
    enabled: bool,
    #[serde(default)]
    operator_can_disable: bool,
    #[serde(default)]
    disclosure: Disclosure,
    description: Option<String>,
    #[serde(default)]
    client_reason: ClientReason,
    #[serde(default = "enabled_default")]
    dry_run: bool,
    auth: Option<AuthEntry>,
    tls: Option<TlsEntry>,
    #[serde(default)]
    params: Option<toml::Value>,
}

fn enabled_default() -> bool {
    true
}

/// What an account's cosigners may learn about an instance (RFC 0002 section 2.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disclosure {
    #[default]
    None,
    Summary,
    Params,
}

/// How a denial from an instance reaches the client (RFC 0002 section 4.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientReason {
    #[default]
    Full,
    Generic,
}

const DESCRIPTION_MAX_CHARS: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    #[serde(default, rename = "module")]
    modules: Vec<InstanceEntry>,
}

#[derive(Debug, Clone)]
pub struct InstanceConfig {
    pub id: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub scope: Scope,
    pub hooks: Vec<HookName>,
    pub timeout: Duration,
    pub on_error: OnError,
    /// The flag in force: the stored runtime flag once the instance has been seeded.
    pub enabled: bool,
    /// The catalog's `enabled`, a seed only; kept to report drift.
    pub catalog_enabled: bool,
    pub operator_can_disable: bool,
    pub disclosure: Disclosure,
    pub description: Option<String>,
    pub client_reason: ClientReason,
    /// Whether the dry-run check may call the instance.
    pub dry_run: bool,
    pub auth_token: Option<guardian_module_sdk::secrecy::SecretString>,
    pub tls: Option<guardian_module_sdk::ModuleTls>,
    /// Seed for the instance's global params; operator-managed after the first start.
    pub params: serde_json::Value,
    /// Params holding secret references stay file-managed: the operator API cannot change them.
    pub file_managed_params: bool,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub instances: Vec<InstanceConfig>,
    pub hash: String,
}

fn json_object(value: Option<toml::Value>, what: &str) -> Result<serde_json::Value, String> {
    let value = match value {
        Some(value) => serde_json::to_value(value).map_err(|e| format!("{what}: {e}"))?,
        None => serde_json::Value::Object(Default::default()),
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(format!("{what} must be a table"))
    }
}

pub(crate) fn contains_secret_reference(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("$secret") || map.values().any(contains_secret_reference)
        }
        serde_json::Value::Array(items) => items.iter().any(contains_secret_reference),
        _ => false,
    }
}

/// How the Guardian reaches a remote module. `grpc://` is TLS; plain text is the explicit
/// `grpc+insecure://` scheme and only for a loopback host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tls,
    PlaintextLoopback,
}

impl Transport {
    pub const TLS_SCHEME: &str = "grpc://";
    pub const PLAINTEXT_SCHEME: &str = "grpc+insecure://";

    pub fn of(endpoint: &str, has_tls_settings: bool) -> Result<Self, String> {
        if let Some(rest) = endpoint.strip_prefix(Self::PLAINTEXT_SCHEME) {
            if has_tls_settings {
                return Err("tls settings need a grpc:// endpoint".into());
            }
            let uri: tonic::transport::Uri = format!("http://{rest}")
                .parse()
                .map_err(|e| format!("invalid endpoint: {e}"))?;
            let host = uri.host().unwrap_or_default();
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            if !loopback {
                return Err(format!(
                    "plain text is only allowed to a loopback host, not {host}; use grpc://"
                ));
            }
            Ok(Self::PlaintextLoopback)
        } else if endpoint.starts_with(Self::TLS_SCHEME) {
            Ok(Self::Tls)
        } else {
            Err("endpoint must start with grpc:// (TLS) or grpc+insecure:// (loopback)".into())
        }
    }
}

impl Catalog {
    /// Prod-stage rules: remote modules use TLS and authenticate with a bearer token.
    pub fn check_production(&self) -> Result<(), String> {
        for config in &self.instances {
            let Some(endpoint) = &config.endpoint else {
                continue;
            };
            if Transport::of(endpoint, config.tls.is_some())? != Transport::Tls {
                return Err(format!(
                    "module {}: plain text is not allowed in the prod stage",
                    config.id
                ));
            }
            if config.auth_token.is_none() {
                return Err(format!(
                    "module {}: auth is mandatory for remote modules in the prod stage",
                    config.id
                ));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read module catalog {}: {e}", path.display()))?;
        Self::parse(&text, path.parent())
    }

    pub fn parse(text: &str, base_dir: Option<&Path>) -> Result<Self, String> {
        let file: CatalogFile =
            toml::from_str(text).map_err(|e| format!("invalid module catalog: {e}"))?;

        let mut seen = HashSet::new();
        let mut instances = Vec::with_capacity(file.modules.len());
        for entry in file.modules {
            let id = entry.id;
            if !seen.insert(id.clone()) {
                return Err(format!("module {id}: duplicate id"));
            }
            if entry.hooks.is_empty() {
                return Err(format!("module {id}: hooks must not be empty"));
            }
            if !(1..=TIMEOUT_MAX_MS).contains(&entry.timeout_ms) {
                return Err(format!(
                    "module {id}: timeout_ms must be between 1 and {TIMEOUT_MAX_MS}"
                ));
            }
            let remote = entry.kind == "grpc";
            if remote != entry.endpoint.is_some() {
                return Err(format!(
                    "module {id}: endpoint is required for kind grpc and only allowed for it"
                ));
            }
            let description = entry
                .description
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            if entry.disclosure != Disclosure::None && description.is_none() {
                return Err(format!(
                    "module {id}: a description is required unless disclosure = \"none\""
                ));
            }
            if description
                .as_ref()
                .is_some_and(|text| text.chars().count() > DESCRIPTION_MAX_CHARS)
            {
                return Err(format!(
                    "module {id}: description is longer than {DESCRIPTION_MAX_CHARS} characters"
                ));
            }
            if !remote && (entry.auth.is_some() || entry.tls.is_some()) {
                return Err(format!(
                    "module {id}: auth and tls are only allowed for remote kinds"
                ));
            }
            if let Some(endpoint) = &entry.endpoint {
                Transport::of(endpoint, entry.tls.is_some())
                    .map_err(|e| format!("module {id}: {e}"))?;
            }
            let params = json_object(entry.params, &format!("module {id} params"))?;
            if remote && contains_secret_reference(&params) {
                return Err(format!(
                    "module {id}: secret references are not allowed in remote module params"
                ));
            }
            let auth_token = entry
                .auth
                .map(|auth| {
                    let path = match base_dir {
                        Some(dir) if auth.token_file.is_relative() => dir.join(&auth.token_file),
                        _ => auth.token_file.clone(),
                    };
                    std::fs::read_to_string(&path)
                        .map(zeroize::Zeroizing::new)
                        .map(|token| guardian_module_sdk::secrecy::SecretString::from(token.trim()))
                        .map_err(|e| format!("module {id}: cannot read token file: {e}"))
                })
                .transpose()?;
            let resolve = |path: &PathBuf| match base_dir {
                Some(dir) if path.is_relative() => dir.join(path),
                _ => path.clone(),
            };
            let read = |path: &PathBuf, what: &str| {
                std::fs::read(resolve(path))
                    .map_err(|e| format!("module {id}: cannot read {what}: {e}"))
            };
            let tls = entry
                .tls
                .map(|tls| {
                    let client_identity = match (&tls.client_cert_file, &tls.client_key_file) {
                        (Some(cert), Some(key)) => {
                            let key = zeroize::Zeroizing::new(read(key, "client key")?);
                            let key = String::from_utf8(key.to_vec())
                                .map_err(|_| format!("module {id}: client key is not PEM"))?;
                            Some(guardian_module_sdk::ClientIdentity::new(
                                read(cert, "client certificate")?,
                                guardian_module_sdk::secrecy::SecretString::from(key),
                            ))
                        }
                        (None, None) => None,
                        _ => {
                            return Err(format!(
                                "module {id}: client_cert_file and client_key_file go together"
                            ));
                        }
                    };
                    Ok(guardian_module_sdk::ModuleTls::new(
                        tls.ca_file
                            .as_ref()
                            .map(|ca| read(ca, "CA file"))
                            .transpose()?,
                        client_identity,
                    ))
                })
                .transpose()?;
            instances.push(InstanceConfig {
                id,
                kind: entry.kind,
                endpoint: entry.endpoint,
                scope: entry.scope,
                hooks: entry.hooks,
                timeout: Duration::from_millis(entry.timeout_ms),
                on_error: entry.on_error,
                enabled: entry.enabled,
                catalog_enabled: entry.enabled,
                operator_can_disable: entry.operator_can_disable,
                disclosure: entry.disclosure,
                description,
                client_reason: entry.client_reason,
                dry_run: entry.dry_run,
                auth_token,
                tls,
                file_managed_params: contains_secret_reference(&params),
                params,
            });
        }

        Ok(Self {
            instances,
            hash: format!("0x{}", hex::encode(Sha256::digest(text.as_bytes()))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Catalog, String> {
        Catalog::parse(text, None)
    }

    const VALID: &str = r#"
[[module]]
id = "caps"
kind = "basic-policy"
scope = "all"
hooks = ["before_transaction_approved", "before_proposal_created"]
timeout_ms = 20
params = { max_amount_per_tx = { "0xaa" = "1000" } }

[[module]]
id = "signers"
kind = "basic-policy"
scope = "assigned"
hooks = ["before_account_registered"]
timeout_ms = 20
on_error = "allow"
"#;

    #[test]
    fn parses_instances() {
        let catalog = parse(VALID).unwrap();
        assert_eq!(catalog.instances.len(), 2);
        assert_eq!(catalog.instances[1].on_error, OnError::Allow);
        assert!(!catalog.instances[0].file_managed_params);
        assert!(catalog.hash.starts_with("0x"));
    }

    #[test]
    fn rejects_invalid_catalogs() {
        let cases = [
            (
                "empty hooks",
                VALID.replacen(r#"hooks = ["before_account_registered"]"#, "hooks = []", 1),
            ),
            (
                "unknown hook",
                VALID.replacen(
                    "before_account_registered",
                    "after_transaction_committed",
                    1,
                ),
            ),
            (
                "default scope",
                VALID.replacen(r#"scope = "assigned""#, r#"scope = "default""#, 1),
            ),
            (
                "timeout",
                VALID.replacen("timeout_ms = 20", "timeout_ms = 5000", 1),
            ),
            (
                "per-account params in the file",
                VALID.replacen(
                    r#"on_error = "allow""#,
                    "on_error = \"allow\"\naccount_params = { \"0x1\" = {} }",
                    1,
                ),
            ),
            (
                "duplicate id",
                VALID.replacen(r#"id = "signers""#, r#"id = "caps""#, 1),
            ),
            (
                "grpc without endpoint",
                VALID.replacen(r#"kind = "basic-policy""#, r#"kind = "grpc""#, 1),
            ),
        ];
        for (name, text) in cases {
            assert!(parse(&text).is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn params_with_secret_references_are_file_managed() {
        let text = VALID.replacen(
            r#"params = { max_amount_per_tx = { "0xaa" = "1000" } }"#,
            r#"params = { api_key = { "$secret" = "x" } }"#,
            1,
        );
        assert!(parse(&text).unwrap().instances[0].file_managed_params);
    }

    #[test]
    fn tokens_never_appear_in_debug_output() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "very-secret-token\n").unwrap();
        let text = r#"
[[module]]
id = "remote"
kind = "grpc"
endpoint = "grpc://127.0.0.1:7443"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
auth = { token_file = "token" }
"#;
        let catalog = Catalog::parse(text, Some(&dir)).unwrap();
        assert!(!format!("{catalog:?}").contains("very-secret-token"));
        assert!(catalog.instances[0].auth_token.is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    fn remote(endpoint: &str, extra: &str) -> String {
        format!(
            r#"
[[module]]
id = "remote"
kind = "grpc"
endpoint = "{endpoint}"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
{extra}
"#
        )
    }

    #[test]
    fn plain_text_is_only_for_loopback_hosts() {
        for endpoint in [
            "grpc+insecure://127.0.0.1:7443",
            "grpc+insecure://localhost:7443",
            "grpc+insecure://[::1]:7443",
        ] {
            assert!(parse(&remote(endpoint, "")).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "grpc+insecure://risk.internal:7443",
            "grpc+insecure://10.0.0.5:7443",
            "http://127.0.0.1:7443",
        ] {
            assert!(parse(&remote(endpoint, "")).is_err(), "{endpoint}");
        }
        assert!(
            parse(&remote(
                "grpc+insecure://127.0.0.1:7443",
                r#"tls = { ca_file = "ca.pem" }"#
            ))
            .is_err()
        );
    }

    #[test]
    fn prod_requires_tls_and_a_token_for_remote_modules() {
        let dir = std::env::temp_dir().join(format!("modules_{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "t").unwrap();
        let auth = r#"auth = { token_file = "token" }"#;
        let check = |endpoint: &str, extra: &str| {
            Catalog::parse(&remote(endpoint, extra), Some(&dir))
                .unwrap()
                .check_production()
        };
        assert!(check("grpc://risk.internal:7443", auth).is_ok());
        assert!(check("grpc://risk.internal:7443", "").is_err());
        assert!(check("grpc+insecure://127.0.0.1:7443", auth).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_client_certificate_needs_its_key() {
        let text = remote(
            "grpc://risk.internal:7443",
            r#"tls = { client_cert_file = "cert.pem" }"#,
        );
        assert!(parse(&text).unwrap_err().contains("go together"));
    }

    #[test]
    fn disclosure_defaults_to_nothing_and_needs_a_description_to_open_up() {
        let base = remote("grpc+insecure://127.0.0.1:7443", "");
        let module = &parse(&base).unwrap().instances[0];
        assert_eq!(module.disclosure, Disclosure::None);
        assert_eq!(module.client_reason, ClientReason::Full);
        assert!(module.dry_run);

        let without = remote(
            "grpc+insecure://127.0.0.1:7443",
            r#"disclosure = "summary""#,
        );
        assert!(
            parse(&without)
                .unwrap_err()
                .contains("description is required")
        );

        let with = remote(
            "grpc+insecure://127.0.0.1:7443",
            "disclosure = \"params\"\ndescription = \"At most 5 per transaction\"\nclient_reason = \"generic\"\ndry_run = false",
        );
        let module = &parse(&with).unwrap().instances[0];
        assert_eq!(module.disclosure, Disclosure::Params);
        assert_eq!(
            module.description.as_deref(),
            Some("At most 5 per transaction")
        );
        assert_eq!(module.client_reason, ClientReason::Generic);
        assert!(!module.dry_run);

        let long = remote(
            "grpc+insecure://127.0.0.1:7443",
            &format!(
                "disclosure = \"summary\"\ndescription = \"{}\"",
                "x".repeat(501)
            ),
        );
        assert!(parse(&long).unwrap_err().contains("longer than"));
    }
}
