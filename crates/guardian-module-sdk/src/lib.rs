//! Interface, protocol types and serving harness for Guardian policy modules.

use std::sync::Arc;

use async_trait::async_trait;

pub mod basic_policy;
pub mod serve;

pub use prost_types;
pub use secrecy;

pub mod proto {
    tonic::include_proto!("guardian.module.v1");
}

pub use proto::{
    AccountProjection, AccountView, Asset, CommitEvent, Decision, DescribeRequest, Direction, Gap,
    Hook, HookRequest, InputNote, ModuleInfo, NoteType, OutputNote, ParamsVerdict,
    PendingTransaction, RegistrationSubject, StorageChange, StorageOp, StorageSlotKind,
    TransactionSubject, TxProjection, ValidateParamsRequest, VaultChange,
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("module unavailable: {0}")]
    Unavailable(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("module error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait Module: Send + Sync {
    async fn describe(&self) -> Result<ModuleInfo, ModuleError>;

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError>;

    /// Must be free of side effects: the same transaction is evaluated at proposal creation
    /// and again at approval, and clients retry.
    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError>;

    async fn notify(&self, _event: &CommitEvent) -> Result<(), ModuleError> {
        Ok(())
    }

    async fn notify_gap(&self, _gap: &Gap) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ModuleInstanceConfig {
    pub id: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub auth_token: Option<secrecy::SecretString>,
    pub tls: Option<ModuleTls>,
    pub timeout: std::time::Duration,
    pub params: serde_json::Value,
}

/// TLS settings of a remote module: a private CA (the system roots when absent) and an
/// optional client identity for mutual TLS.
#[derive(Debug, Clone, Default)]
pub struct ModuleTls {
    pub ca_pem: Option<Vec<u8>>,
    pub client_identity: Option<ClientIdentity>,
}

#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub cert_pem: Vec<u8>,
    pub key_pem: secrecy::SecretString,
}

pub trait ModuleFactory: Send + Sync {
    fn create(&self, instance: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError>;
}

impl Decision {
    pub fn allow() -> Self {
        Self {
            allow: true,
            reason_code: String::new(),
            message: String::new(),
        }
    }

    pub fn deny(reason_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            allow: false,
            reason_code: reason_code.into(),
            message: message.into(),
        }
    }
}

impl ParamsVerdict {
    pub fn ok() -> Self {
        Self {
            ok: true,
            message: String::new(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
        }
    }
}

pub fn is_valid_reason_code(code: &str) -> bool {
    (1..=64).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_config_debug_redacts_the_token() {
        let config = ModuleInstanceConfig {
            id: "remote".into(),
            kind: "grpc".into(),
            endpoint: None,
            auth_token: Some(secrecy::SecretString::from("very-secret-token")),
            tls: Some(ModuleTls {
                ca_pem: None,
                client_identity: Some(ClientIdentity {
                    cert_pem: Vec::new(),
                    key_pem: secrecy::SecretString::from("very-secret-key"),
                }),
            }),
            timeout: std::time::Duration::from_millis(10),
            params: serde_json::json!({}),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("very-secret-token"));
        assert!(!debug.contains("very-secret-key"));
    }
}
