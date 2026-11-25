//! The module catalog: instances declared in the file at `GUARDIAN_MODULES_CONFIG`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    /// Every account, always; an account layer can only add restrictions.
    All,
    /// Copied into an account's assignment when it first registers; replaceable per account.
    Default,
    /// Only accounts an operator assigns it to.
    Assigned,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Default => "default",
            Scope::Assigned => "assigned",
        }
    }
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
    pub const GATES: [HookName; 3] = [
        HookName::BeforeTransactionApproved,
        HookName::BeforeProposalCreated,
        HookName::BeforeAccountRegistered,
    ];

    /// The catalog spelling, for example `before_transaction_approved`.
    pub fn as_str(self) -> &'static str {
        match self {
            HookName::BeforeTransactionApproved => "before_transaction_approved",
            HookName::BeforeProposalCreated => "before_proposal_created",
            HookName::BeforeAccountRegistered => "before_account_registered",
        }
    }

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
    token_secret: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsEntry {
    ca_file: Option<PathBuf>,
    client_cert_secret: Option<String>,
    client_key_secret: Option<String>,
}

/// The secrets an instance names, resolved through the secrets backend before it is created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretRefs {
    pub token: Option<String>,
    pub tls: Option<TlsRefs>,
    /// Names of the `$secret` references in the catalog params of an in-process instance.
    pub params: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsRefs {
    pub ca_file: Option<PathBuf>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
}

impl SecretRefs {
    pub fn is_empty(&self) -> bool {
        self.token.is_none() && self.tls.is_none() && self.params.is_empty()
    }

    pub fn names_secrets(&self) -> bool {
        self.token.is_some()
            || !self.params.is_empty()
            || self
                .tls
                .as_ref()
                .is_some_and(|tls| tls.client_cert.is_some())
    }
}

/// What an instance's [`SecretRefs`] resolved to.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub auth_token: Option<guardian_module_sdk::secrecy::SecretString>,
    pub tls: Option<guardian_module_sdk::ModuleTls>,
    pub params: BTreeMap<String, guardian_module_sdk::secrecy::SecretString>,
}

/// Describes a catalog that does not parse by position and the parser's message, leaving out
/// the source excerpt the parser's own `Display` quotes and everything the message quotes
/// (`invalid type: string "..."`, ``unknown variant `...` ``), since a line can hold a value an
/// operator pasted by mistake.
fn parse_error(text: &str, error: &toml::de::Error) -> String {
    let message = without_quoted(error.message());
    match error.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1;
            format!("invalid module catalog at line {line}, column {column}: {message}")
        }
        None => format!("invalid module catalog: {message}"),
    }
}

/// `message` with everything from its first `"` or `` ` `` to its last replaced by `...`. A
/// quoted value can itself hold a quote character, so pairing the delimiters could end a value
/// early and keep the rest of it; only the parser's own text outside every quote is kept.
fn without_quoted(message: &str) -> String {
    let is_quote = |c: char| c == '"' || c == '`';
    match (message.find(is_quote), message.rfind(is_quote)) {
        (Some(first), Some(last)) => {
            format!("{}...{}", &message[..first], &message[last + 1..])
        }
        _ => message.to_string(),
    }
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
    #[serde(default = "enabled_default")]
    operator_can_set_params: bool,
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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Disclosure {
    #[default]
    None,
    Summary,
    Params,
}

/// How a denial from an instance reaches the client (RFC 0002 section 4.1).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClientReason {
    #[default]
    Full,
    Generic,
}

const DESCRIPTION_MAX_CHARS: usize = 500;
const INSTANCE_ID_MAX_CHARS: usize = 64;

/// An instance id is a metric label, a log field and a path segment of the operator routes.
fn valid_instance_id(id: &str) -> bool {
    (1..=INSTANCE_ID_MAX_CHARS).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

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
    /// `false` keeps the global params in the catalog file: the operator API refuses to write
    /// them, and each start stores the catalog's. Only for `scope = "all"`, where per-account
    /// writes can only add restrictions.
    pub operator_can_set_params: bool,
    pub disclosure: Disclosure,
    pub description: Option<String>,
    pub client_reason: ClientReason,
    /// Whether the dry-run check may call the instance.
    pub dry_run: bool,
    pub secrets: SecretRefs,
    /// What `secrets` resolved to, or why they did not; an instance is created only from a
    /// resolution, so a missing secret fails it like any other creation error.
    pub credentials: Result<Credentials, String>,
    /// Seed for the instance's global params; operator-managed after the first start.
    pub params: serde_json::Value,
    /// Params holding secret references stay file-managed: the operator API cannot change them.
    pub file_managed_params: bool,
}

impl InstanceConfig {
    /// Whether the global params come from the catalog file whatever is stored: params that
    /// reference secrets, and params the catalog locks (`operator_can_set_params = false`).
    /// Evaluation reads them from the catalog, so a stored value written by a replica on an
    /// older catalog never applies, and nothing but startup stores them.
    pub fn catalog_managed_params(&self) -> bool {
        self.file_managed_params || !self.operator_can_set_params
    }
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
            map.contains_key(SECRET_KEY) || map.values().any(contains_secret_reference)
        }
        serde_json::Value::Array(items) => items.iter().any(contains_secret_reference),
        _ => false,
    }
}

const SECRET_KEY: &str = "$secret";
const SECRET_NAME_MAX_CHARS: usize = 512;

/// A secrets backend name: what AWS Secrets Manager accepts, which the file provider reads as a
/// path below its directory.
pub(crate) fn check_secret_name(name: &str) -> Result<(), String> {
    let valid = (1..=SECRET_NAME_MAX_CHARS).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_+=.@-".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "secret name {name:?} must be 1 to {SECRET_NAME_MAX_CHARS} characters of letters, digits and /_+=.@-"
        ))
    }
}

/// Collects the names of the `$secret` references in `value`; a reference is an object whose
/// only key is `$secret`, with a secret name as its value.
fn secret_references(
    value: &serde_json::Value,
    names: &mut BTreeSet<String>,
) -> Result<(), String> {
    match value {
        serde_json::Value::Object(map) => match map.get(SECRET_KEY) {
            Some(serde_json::Value::String(name)) if map.len() == 1 => {
                check_secret_name(name)?;
                names.insert(name.clone());
                Ok(())
            }
            Some(_) => Err(format!(
                "a secret reference is {{ \"{SECRET_KEY}\" = \"<name>\" }} and nothing else"
            )),
            None => map
                .values()
                .try_for_each(|value| secret_references(value, names)),
        },
        serde_json::Value::Array(items) => items
            .iter()
            .try_for_each(|value| secret_references(value, names)),
        _ => Ok(()),
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
            if Transport::of(endpoint, config.secrets.tls.is_some())? != Transport::Tls {
                return Err(format!(
                    "module {}: plain text is not allowed in the prod stage",
                    config.id
                ));
            }
            if config.secrets.token.is_none() {
                return Err(format!(
                    "module {}: auth is mandatory for remote modules in the prod stage",
                    config.id
                ));
            }
        }
        Ok(())
    }

    /// Prod-stage budget checks: for every hook, the timeouts of the instances every new account
    /// receives (all but `assigned`, enabled or not, since enabling needs no further check) fit
    /// in `budget`, and `budget` is below the execution reservation `lease`.
    pub fn check_budget(&self, budget: Duration, lease: Duration) -> Result<(), String> {
        if budget >= lease {
            return Err(format!(
                "the module evaluation budget ({} ms) must be below the execution reservation \
                 lease ({} ms)",
                budget.as_millis(),
                lease.as_millis()
            ));
        }
        for hook in HookName::GATES {
            let total: Duration = self
                .instances
                .iter()
                .filter(|config| config.scope != Scope::Assigned && config.hooks.contains(&hook))
                .map(|config| config.timeout)
                .sum();
            if total > budget {
                return Err(format!(
                    "the timeouts of the modules every account receives at {} add up to {} ms, \
                     over the evaluation budget of {} ms",
                    hook.as_str(),
                    total.as_millis(),
                    budget.as_millis()
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
        let file: CatalogFile = toml::from_str(text).map_err(|e| parse_error(text, &e))?;

        let mut seen = HashSet::new();
        let mut instances = Vec::with_capacity(file.modules.len());
        for entry in file.modules {
            let id = entry.id;
            if !valid_instance_id(&id) {
                return Err(format!(
                    "module {id:?}: an id is 1 to {INSTANCE_ID_MAX_CHARS} characters of a-z, 0-9, '_' or '-'"
                ));
            }
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
            if entry.scope == Scope::Default && contains_secret_reference(&params) {
                return Err(format!(
                    "module {id}: scope = \"default\" copies its params into each new account, and secret references cannot be stored per account; use scope \"all\" or \"assigned\""
                ));
            }
            if !entry.operator_can_set_params && entry.scope != Scope::All {
                return Err(format!(
                    "module {id}: operator_can_set_params = false protects the global params of a scope \"all\" instance; per-account params of a \"{}\" instance replace them, so it cannot lock them",
                    entry.scope.as_str()
                ));
            }
            if entry.disclosure == Disclosure::Params && contains_secret_reference(&params) {
                return Err(format!(
                    "module {id}: disclosure = \"params\" would show cosigners the secret references in its params; use \"summary\" or \"none\""
                ));
            }
            let mut param_secrets = BTreeSet::new();
            secret_references(&params, &mut param_secrets)
                .map_err(|e| format!("module {id} params: {e}"))?;
            let token = entry
                .auth
                .map(|auth| check_secret_name(&auth.token_secret).map(|()| auth.token_secret))
                .transpose()
                .map_err(|e| format!("module {id} auth: {e}"))?;
            let tls = entry
                .tls
                .map(|tls| {
                    if tls.client_cert_secret.is_some() != tls.client_key_secret.is_some() {
                        return Err(
                            "client_cert_secret and client_key_secret go together".to_string()
                        );
                    }
                    for name in [&tls.client_cert_secret, &tls.client_key_secret]
                        .into_iter()
                        .flatten()
                    {
                        check_secret_name(name)?;
                    }
                    Ok(TlsRefs {
                        ca_file: tls.ca_file.map(|path| match base_dir {
                            Some(dir) if path.is_relative() => dir.join(path),
                            _ => path,
                        }),
                        client_cert: tls.client_cert_secret,
                        client_key: tls.client_key_secret,
                    })
                })
                .transpose()
                .map_err(|e| format!("module {id} tls: {e}"))?;
            let secrets = SecretRefs {
                token,
                tls,
                params: param_secrets,
            };
            let credentials = if secrets.is_empty() {
                Ok(Credentials::default())
            } else {
                Err("secrets not resolved".to_string())
            };
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
                operator_can_set_params: entry.operator_can_set_params,
                disclosure: entry.disclosure,
                description,
                client_reason: entry.client_reason,
                dry_run: entry.dry_run,
                secrets,
                credentials,
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
kind = "spending-limit"
scope = "all"
hooks = ["before_transaction_approved", "before_proposal_created"]
timeout_ms = 20
params = { max_per_transaction = { "0x0000000000000000000000000000aa" = "1000" } }

[[module]]
id = "signers"
kind = "signer-policy"
scope = "assigned"
hooks = ["before_account_registered"]
timeout_ms = 20
on_error = "allow"
"#;

    #[test]
    fn a_catalog_that_does_not_parse_is_reported_without_the_values_in_it() {
        let entry = "[[module]]\nid = \"m\"\nkind = \"spending-limit\"\nscope = \"all\"\nhooks = [\"before_transaction_approved\"]\n";
        for (field, expected_line) in [
            ("token = very-secret-token", "line 6"),
            ("timeout_ms = \"very-secret-token\"", "line 6"),
            (
                "timeout_ms = 20\non_error = \"very-secret-token\"",
                "line 7",
            ),
            (
                "timeout_ms = 20\non_error = \"prefix`very-secret-token\"",
                "line 7",
            ),
            (
                "timeout_ms = 20\non_error = 'prefix\"very-secret-token'",
                "line 7",
            ),
        ] {
            let error = parse(&format!("{entry}{field}\n")).unwrap_err();
            assert!(error.contains(expected_line), "{field}: {error}");
            assert!(!error.contains("very-secret-token"), "{field}: {error}");
        }
    }

    #[test]
    fn operator_can_set_params_defaults_to_true_and_can_lock_an_all_instance() {
        let catalog = parse(VALID).unwrap();
        assert!(
            catalog
                .instances
                .iter()
                .all(|config| config.operator_can_set_params)
        );
        let locked = parse(&VALID.replacen(
            "timeout_ms = 20\nparams",
            "timeout_ms = 20\noperator_can_set_params = false\nparams",
            1,
        ))
        .unwrap();
        assert!(!locked.instances[0].operator_can_set_params);
    }

    fn timed(instances: &[(&str, &str, u64)]) -> Catalog {
        let text: String = instances
            .iter()
            .map(|(id, scope, timeout_ms)| {
                format!(
                    "[[module]]\nid = \"{id}\"\nkind = \"spending-limit\"\nscope = \"{scope}\"\n\
                     hooks = [\"before_transaction_approved\"]\ntimeout_ms = {timeout_ms}\n\n"
                )
            })
            .collect();
        parse(&text).unwrap()
    }

    #[test]
    fn the_modules_every_account_receives_must_fit_in_the_budget() {
        let budget = Duration::from_millis(1_000);
        let lease = Duration::from_secs(60);
        assert!(
            timed(&[("a", "all", 500), ("b", "all", 500)])
                .check_budget(budget, lease)
                .is_ok()
        );
        let over = timed(&[("a", "all", 600), ("b", "all", 600)])
            .check_budget(budget, lease)
            .unwrap_err();
        assert!(over.contains("before_transaction_approved"), "{over}");
        assert!(
            timed(&[("a", "all", 600), ("b", "assigned", 600)])
                .check_budget(budget, lease)
                .is_ok(),
            "assigned instances are checked per account, not at startup"
        );
    }

    #[test]
    fn the_budget_must_be_below_the_execution_lease() {
        let catalog = timed(&[("a", "all", 10)]);
        assert!(
            catalog
                .check_budget(Duration::from_secs(30), Duration::from_secs(30))
                .is_err()
        );
        assert!(
            catalog
                .check_budget(Duration::from_secs(1), Duration::from_secs(30))
                .is_ok()
        );
    }

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
                "params locked on an instance whose per-account params replace them",
                VALID.replacen(
                    r#"on_error = "allow""#,
                    "on_error = \"allow\"\noperator_can_set_params = false",
                    1,
                ),
            ),
            (
                "grpc without endpoint",
                VALID.replacen(r#"kind = "spending-limit""#, r#"kind = "grpc""#, 1),
            ),
            (
                "empty id",
                VALID.replacen(r#"id = "caps""#, r#"id = """#, 1),
            ),
            (
                "id that is not a path segment",
                VALID.replacen(r#"id = "caps""#, r#"id = "Caps/v2""#, 1),
            ),
            (
                "id over the length limit",
                VALID.replacen(r#"id = "caps""#, &format!("id = \"{}\"", "a".repeat(65)), 1),
            ),
        ];
        for (name, text) in cases {
            assert!(parse(&text).is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn default_instances_cannot_hold_secret_references() {
        let catalog = |scope: &str| {
            format!(
                r#"
[[module]]
id = "vendor"
kind = "vendor-screening"
scope = "{scope}"
hooks = ["before_transaction_approved"]
timeout_ms = 20
params = {{ api_key = {{ "$secret" = "vendor-key" }} }}
"#
            )
        };
        assert!(parse(&catalog("all")).is_ok());
        let refused = parse(&catalog("default")).unwrap_err();
        assert!(refused.contains("copies its params"), "{refused}");
    }

    #[test]
    fn params_with_secret_references_are_file_managed() {
        let text = VALID.replacen(
            r#"params = { max_per_transaction = { "0x0000000000000000000000000000aa" = "1000" } }"#,
            r#"params = { api_key = { "$secret" = "x" } }"#,
            1,
        );
        assert!(parse(&text).unwrap().instances[0].file_managed_params);
    }

    #[test]
    fn credentials_are_secret_names_resolved_later() {
        let text = remote(
            "grpc://127.0.0.1:7443",
            r#"auth = { token_secret = "guardian/modules/risk-token" }
tls = { ca_file = "ca.pem", client_cert_secret = "risk-cert", client_key_secret = "risk-key" }"#,
        );
        let catalog = Catalog::parse(&text, Some(Path::new("/etc/guardian"))).unwrap();
        let config = &catalog.instances[0];
        assert_eq!(
            config.secrets,
            SecretRefs {
                token: Some("guardian/modules/risk-token".into()),
                tls: Some(TlsRefs {
                    ca_file: Some(PathBuf::from("/etc/guardian/ca.pem")),
                    client_cert: Some("risk-cert".into()),
                    client_key: Some("risk-key".into()),
                }),
                params: BTreeSet::new(),
            }
        );
        assert!(config.credentials.is_err());
        let invalid = remote(
            "grpc://127.0.0.1:7443",
            r#"auth = { token_secret = "../token file" }"#,
        );
        assert!(parse(&invalid).unwrap_err().contains("secret name"));
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
        let auth = r#"auth = { token_secret = "token" }"#;
        let check = |endpoint: &str, extra: &str| {
            parse(&remote(endpoint, extra)).unwrap().check_production()
        };
        assert!(check("grpc://risk.internal:7443", auth).is_ok());
        assert!(check("grpc://risk.internal:7443", "").is_err());
        assert!(check("grpc+insecure://127.0.0.1:7443", auth).is_err());
    }

    #[test]
    fn a_client_certificate_needs_its_key() {
        let text = remote(
            "grpc://risk.internal:7443",
            r#"tls = { client_cert_secret = "cert" }"#,
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

    #[test]
    fn params_holding_secret_references_cannot_be_disclosed() {
        let catalog = |disclosure: &str| {
            format!(
                r#"
[[module]]
id = "vendor"
kind = "vendor-screening"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
disclosure = "{disclosure}"
description = "Vendor screening"
params = {{ api_key = {{ "$secret" = "vendor-key" }} }}
"#
            )
        };
        assert!(
            parse(&catalog("params"))
                .unwrap_err()
                .contains("secret references")
        );
        assert!(parse(&catalog("summary")).is_ok());
    }
}
