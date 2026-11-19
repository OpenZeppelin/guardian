//! The secrets backend for module credentials and `$secret` params: files in a directory, or
//! AWS Secrets Manager. Secrets are read once, when the catalog's instances are created.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use guardian_module_sdk::secrecy::{ExposeSecret, SecretString};
use guardian_module_sdk::{ClientIdentity, ModuleTls};

use super::catalog::{Catalog, Credentials, SecretRefs, check_secret_name};

pub const PROVIDER_ENV: &str = "GUARDIAN_MODULE_SECRET_PROVIDER";
pub const DIR_ENV: &str = "GUARDIAN_MODULE_SECRET_DIR";

#[async_trait]
pub trait SecretSource: Send + Sync {
    async fn read(&self, name: &str) -> Result<SecretString, String>;
}

/// One file per secret, at `<dir>/<name>`, readable by its owner only.
pub struct FileSecrets {
    dir: PathBuf,
}

impl FileSecrets {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        check_secret_name(name)?;
        let relative = Path::new(name);
        let plain = relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)));
        if plain {
            Ok(self.dir.join(relative))
        } else {
            Err(format!(
                "secret name {name:?} must be a relative path without . or .. segments"
            ))
        }
    }
}

#[async_trait]
impl SecretSource for FileSecrets {
    async fn read(&self, name: &str) -> Result<SecretString, String> {
        let path = self.path(name)?;
        ensure_owner_only(&path)?;
        std::fs::read_to_string(&path)
            .map(SecretString::from)
            .map_err(|e| format!("cannot read secret file {}: {e}", path.display()))
    }
}

pub struct AwsSecrets {
    client: aws_sdk_secretsmanager::Client,
}

impl AwsSecrets {
    pub async fn from_env() -> Result<Self, String> {
        if std::env::var("AWS_REGION").map_or(true, |region| region.is_empty()) {
            return Err(format!("{PROVIDER_ENV}=aws needs AWS_REGION to be set"));
        }
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        Ok(Self {
            client: aws_sdk_secretsmanager::Client::new(&config),
        })
    }
}

#[async_trait]
impl SecretSource for AwsSecrets {
    async fn read(&self, name: &str) -> Result<SecretString, String> {
        check_secret_name(name)?;
        let response = self
            .client
            .get_secret_value()
            .secret_id(name)
            .send()
            .await
            .map_err(|e| format!("cannot read secret {name} from Secrets Manager: {e}"))?;
        response
            .secret_string()
            .map(|value| SecretString::from(value.to_owned()))
            .ok_or_else(|| format!("secret {name} holds no secret string"))
    }
}

/// The provider named by `GUARDIAN_MODULE_SECRET_PROVIDER`: `aws` by default in the prod stage,
/// `file` otherwise, reading `GUARDIAN_MODULE_SECRET_DIR` or the catalog's directory.
pub async fn from_env(catalog_dir: &Path, prod: bool) -> Result<Box<dyn SecretSource>, String> {
    let provider = std::env::var(PROVIDER_ENV).unwrap_or_default();
    let provider = match provider.as_str() {
        "" if prod => "aws",
        "" => "file",
        other => other,
    };
    match provider {
        "file" => {
            let dir = std::env::var_os(DIR_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| catalog_dir.to_path_buf());
            Ok(Box::new(FileSecrets::new(dir)))
        }
        "aws" => Ok(Box::new(AwsSecrets::from_env().await?)),
        other => Err(format!(
            "{PROVIDER_ENV} must be \"file\" or \"aws\", got {other:?}"
        )),
    }
}

impl SecretRefs {
    /// Reads every secret the instance names; an error names the secret, never its value.
    pub async fn resolve(&self, source: Option<&dyn SecretSource>) -> Result<Credentials, String> {
        let read = |name: &str| {
            let name = name.to_string();
            async move {
                match source {
                    Some(source) => source.read(&name).await,
                    None => Err("no secrets provider".to_string()),
                }
                .map_err(|e| format!("secret {name}: {e}"))
            }
        };
        let auth_token = match &self.token {
            Some(name) => {
                let token = read(name).await?.expose_secret().trim().to_owned();
                if token.is_empty() {
                    return Err(format!("secret {name}: the token is empty"));
                }
                Some(SecretString::from(token))
            }
            None => None,
        };
        let tls = match &self.tls {
            Some(tls) => {
                let ca_pem = tls
                    .ca_file
                    .as_ref()
                    .map(|path| {
                        std::fs::read(path)
                            .map_err(|e| format!("cannot read CA file {}: {e}", path.display()))
                    })
                    .transpose()?;
                let client_identity = match (&tls.client_cert, &tls.client_key) {
                    (Some(cert), Some(key)) => Some(ClientIdentity::new(
                        read(cert).await?.expose_secret().as_bytes().to_vec(),
                        read(key).await?,
                    )),
                    _ => None,
                };
                Some(ModuleTls::new(ca_pem, client_identity))
            }
            None => None,
        };
        let mut params = std::collections::BTreeMap::new();
        for name in &self.params {
            params.insert(name.clone(), read(name).await?);
        }
        Ok(Credentials {
            auth_token,
            tls,
            params,
        })
    }
}

impl Catalog {
    /// Resolves every instance's secrets. A failure is kept on the instance, so it stops startup
    /// for an enabled instance and leaves a disabled one disabled.
    pub async fn resolve_secrets(&mut self, source: Option<&dyn SecretSource>) {
        for config in &mut self.instances {
            config.credentials = config.secrets.resolve(source).await;
        }
    }

    pub fn names_secrets(&self) -> bool {
        self.instances
            .iter()
            .any(|config| config.secrets.names_secrets())
    }
}

#[cfg(unix)]
fn ensure_owner_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(|e| format!("cannot read secret file {}: {e}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(format!(
            "secret file {} must not be accessible by group or others (set its permissions to 0600)",
            path.display()
        ))
    }
}

#[cfg(not(unix))]
fn ensure_owner_only(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct Fixed(HashMap<&'static str, &'static str>);

    #[async_trait]
    impl SecretSource for Fixed {
        async fn read(&self, name: &str) -> Result<SecretString, String> {
            self.0
                .get(name)
                .map(|value| SecretString::from(value.to_string()))
                .ok_or_else(|| "not found".to_string())
        }
    }

    fn catalog(extra: &str, params: &str) -> Catalog {
        Catalog::parse(
            &format!(
                r#"
[[module]]
id = "m"
kind = "{kind}"
{endpoint}
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
{extra}
params = {params}
"#,
                kind = if extra.is_empty() {
                    "vendor-screening"
                } else {
                    "grpc"
                },
                endpoint = if extra.is_empty() {
                    ""
                } else {
                    r#"endpoint = "grpc://risk.internal:7443""#
                },
            ),
            None,
        )
        .unwrap()
    }

    fn private_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("secrets_{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(dir.join("modules")).unwrap();
        dir
    }

    #[cfg(unix)]
    fn write_private(path: &Path, value: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, value).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[tokio::test]
    async fn a_token_and_client_identity_resolve_from_the_source() {
        let mut catalog = catalog(
            r#"auth = { token_secret = "guardian/modules/risk-token" }
tls = { client_cert_secret = "risk-cert", client_key_secret = "risk-key" }"#,
            "{}",
        );
        assert!(catalog.instances[0].credentials.is_err());
        let source = Fixed(HashMap::from([
            ("guardian/modules/risk-token", "very-secret-token\n"),
            ("risk-cert", "CERT"),
            ("risk-key", "very-secret-key"),
        ]));
        catalog.resolve_secrets(Some(&source)).await;
        let credentials = catalog.instances[0].credentials.as_ref().unwrap();
        assert_eq!(
            credentials.auth_token.as_ref().unwrap().expose_secret(),
            "very-secret-token"
        );
        let identity = credentials
            .tls
            .as_ref()
            .unwrap()
            .client_identity
            .as_ref()
            .unwrap();
        assert_eq!(identity.cert_pem, b"CERT");
        let debug = format!("{catalog:?}");
        assert!(!debug.contains("very-secret-token") && !debug.contains("very-secret-key"));
    }

    #[tokio::test]
    async fn a_missing_secret_is_kept_on_the_instance_by_name() {
        let mut catalog = catalog(r#"auth = { token_secret = "absent" }"#, "{}");
        catalog.resolve_secrets(Some(&Fixed(HashMap::new()))).await;
        let error = catalog.instances[0].credentials.as_ref().unwrap_err();
        assert!(error.contains("secret absent"), "{error}");
    }

    #[tokio::test]
    async fn params_secrets_resolve_and_the_params_keep_the_references() {
        let mut catalog = catalog(
            "",
            r#"{ vendor = { api_key = { "$secret" = "vendor-key" } } }"#,
        );
        let source = Fixed(HashMap::from([("vendor-key", "k1")]));
        catalog.resolve_secrets(Some(&source)).await;
        let config = &catalog.instances[0];
        assert_eq!(
            config.credentials.as_ref().unwrap().params["vendor-key"].expose_secret(),
            "k1"
        );
        assert_eq!(
            config.params,
            serde_json::json!({ "vendor": { "api_key": { "$secret": "vendor-key" } } })
        );
    }

    #[test]
    fn malformed_references_are_refused_at_parse() {
        for params in [
            r#"{ api_key = { "$secret" = "k", extra = 1 } }"#,
            r#"{ api_key = { "$secret" = 1 } }"#,
            r#"{ api_key = { "$secret" = "has space" } }"#,
        ] {
            let text = format!(
                "[[module]]\nid = \"m\"\nkind = \"vendor-screening\"\nscope = \"all\"\nhooks = [\"before_transaction_approved\"]\ntimeout_ms = 20\nparams = {params}\n"
            );
            assert!(Catalog::parse(&text, None).is_err(), "{params}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_provider_reads_private_files_below_its_directory() {
        let dir = private_dir();
        write_private(&dir.join("modules/token"), "t1\n");
        let source = FileSecrets::new(dir.clone());
        assert_eq!(
            source.read("modules/token").await.unwrap().expose_secret(),
            "t1\n"
        );
        for name in ["../token", "modules/../token", "/etc/passwd", "./token"] {
            assert!(source.read(name).await.is_err(), "{name}");
        }
        std::fs::write(dir.join("open"), "t2").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("open"), std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        let error = source.read("open").await.unwrap_err();
        assert!(error.contains("0600"), "{error}");
        std::fs::remove_dir_all(dir).ok();
    }
}
