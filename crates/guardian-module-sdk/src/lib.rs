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
    AccountStateProjection, AccountView, Asset, CommitEvent, Decision, DescribeRequest, Direction,
    Effect, Gap, Hook, HookRequest, InputNote, MidenAccountState, MidenTransaction, ModuleInfo,
    NoteType, OutputNote, ParamsVerdict, PendingTransaction, RawTransaction, RegistrationSubject,
    StorageChange, StorageOp, StorageSlotKind, TransactionProjection, TransactionSubject,
    ValidateParamsRequest, VaultChange,
};

pub const PROTOCOL_VERSION: u32 = 1;

/// Optional protocol features, named in `DescribeRequest.guardian_capabilities` and
/// `ModuleInfo.required_capabilities`. New features are added as capabilities, not as a new
/// protocol version, so modules and Guardians of one major version keep working together.
pub mod capability {
    /// `Decision.effect` is read.
    pub const DECISION_EFFECT: &str = "decision.effect";
    /// `ParamsVerdict.normalized_params` is stored.
    pub const PARAMS_NORMALIZATION: &str = "params.normalization";
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    /// `guardian` carries the Guardian's protocol range and capabilities.
    async fn describe(&self, guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError>;

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
#[non_exhaustive]
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
impl ModuleInstanceConfig {
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        timeout: std::time::Duration,
        params: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            endpoint: None,
            auth_token: None,
            tls: None,
            timeout,
            params,
        }
    }

    pub fn with_endpoint(mut self, endpoint: Option<String>) -> Self {
        self.endpoint = endpoint;
        self
    }

    pub fn with_auth_token(mut self, auth_token: Option<secrecy::SecretString>) -> Self {
        self.auth_token = auth_token;
        self
    }

    pub fn with_tls(mut self, tls: Option<ModuleTls>) -> Self {
        self.tls = tls;
        self
    }
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModuleTls {
    pub ca_pem: Option<Vec<u8>>,
    pub client_identity: Option<ClientIdentity>,
}

impl ModuleTls {
    pub fn new(ca_pem: Option<Vec<u8>>, client_identity: Option<ClientIdentity>) -> Self {
        Self {
            ca_pem,
            client_identity,
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClientIdentity {
    pub cert_pem: Vec<u8>,
    pub key_pem: secrecy::SecretString,
}

impl ClientIdentity {
    pub fn new(cert_pem: Vec<u8>, key_pem: secrecy::SecretString) -> Self {
        Self { cert_pem, key_pem }
    }
}

pub trait ModuleFactory: Send + Sync {
    fn create(&self, instance: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError>;
}

impl TransactionProjection {
    pub fn miden(transaction: MidenTransaction) -> Self {
        Self {
            network: Some(proto::transaction_projection::Network::Miden(transaction)),
        }
    }
}

impl AccountStateProjection {
    pub fn miden(state: MidenAccountState) -> Self {
        Self {
            network: Some(proto::account_state_projection::Network::Miden(state)),
        }
    }
}

impl RawTransaction {
    /// `encoding` of a Miden `TransactionSummary` in Miden's binary serialization.
    pub const MIDEN_TRANSACTION_SUMMARY: &str = "miden.transaction_summary";
}

fn miden_transaction(projection: &Option<TransactionProjection>) -> Option<&MidenTransaction> {
    match projection.as_ref()?.network.as_ref()? {
        proto::transaction_projection::Network::Miden(transaction) => Some(transaction),
    }
}

impl TransactionSubject {
    /// The Miden projection, when this is a Miden transaction.
    pub fn miden(&self) -> Option<&MidenTransaction> {
        miden_transaction(&self.projection)
    }
}

impl PendingTransaction {
    /// The Miden projection, when this is a Miden transaction.
    pub fn miden(&self) -> Option<&MidenTransaction> {
        miden_transaction(&self.projection)
    }
}

impl CommitEvent {
    /// The Miden projection, when this is a Miden transaction.
    pub fn miden(&self) -> Option<&MidenTransaction> {
        miden_transaction(&self.projection)
    }
}

impl RegistrationSubject {
    /// The Miden account state, when this is a Miden account.
    pub fn miden(&self) -> Option<&MidenAccountState> {
        match self.initial_state.as_ref()?.network.as_ref()? {
            proto::account_state_projection::Network::Miden(state) => Some(state),
        }
    }
}

impl Decision {
    pub fn allow() -> Self {
        Self {
            allow: true,
            reason_code: String::new(),
            message: String::new(),
            effect: Effect::Allow as i32,
            client_message: String::new(),
        }
    }

    pub fn deny(reason_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            allow: false,
            reason_code: reason_code.into(),
            message: message.into(),
            effect: Effect::Deny as i32,
            client_message: String::new(),
        }
    }

    /// Text for the person who sent the request. The Guardian forwards it only for instances
    /// whose catalog entry allows it, as plain text of at most 256 characters.
    pub fn with_client_message(mut self, client_message: impl Into<String>) -> Self {
        self.client_message = client_message.into();
        self
    }
}

impl ParamsVerdict {
    pub fn ok() -> Self {
        Self {
            ok: true,
            message: String::new(),
            normalized_params: Vec::new(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            normalized_params: Vec::new(),
        }
    }

    /// Valid, and to be stored in the module's current shape: `params` is what the Guardian
    /// keeps instead of what the operator or an older catalog sent.
    pub fn normalized(params: &serde_json::Value) -> Self {
        Self {
            ok: true,
            message: String::new(),
            normalized_params: serde_json::to_vec(params).unwrap_or_default(),
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

    #[test]
    fn the_miden_accessors_read_only_the_miden_variant() {
        assert!(TransactionSubject::default().miden().is_none());
        let subject = TransactionSubject {
            projection: Some(TransactionProjection::miden(MidenTransaction {
                nonce_delta: 1,
                ..Default::default()
            })),
            ..Default::default()
        };
        assert_eq!(subject.miden().map(|miden| miden.nonce_delta), Some(1));
        assert!(RegistrationSubject::default().miden().is_none());
        let registration = RegistrationSubject {
            initial_state: Some(AccountStateProjection::miden(MidenAccountState::default())),
        };
        assert!(registration.miden().is_some());
    }
}
