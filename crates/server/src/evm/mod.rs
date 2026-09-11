pub mod config;
pub mod contracts;
pub mod proposal;
pub mod service;
pub mod session;

use std::sync::Arc;

pub use config::{EvmChainConfig, EvmChainRegistry};
pub use proposal::{
    EvmProposal, EvmProposalFilter, EvmProposalSignature, ExecutableEvmProposal,
    NormalizedEvmProposalInput,
};
pub use session::EvmSessionState;

#[derive(Clone)]
pub struct EvmAppState {
    pub chains: Arc<EvmChainRegistry>,
    pub sessions: Arc<EvmSessionState>,
}

impl EvmAppState {
    pub async fn from_env() -> Result<Self, String> {
        Self::from_env_with_sessions(EvmSessionState::default()).await
    }

    /// Build EVM state with explicit (evm-realm) session state. The server
    /// builder passes shared (Postgres) stores on the Postgres backend.
    pub async fn from_env_with_sessions(sessions: EvmSessionState) -> Result<Self, String> {
        let chains = Arc::new(EvmChainRegistry::from_env()?);
        Ok(Self {
            chains,
            sessions: Arc::new(sessions),
        })
    }

    pub fn for_tests() -> Self {
        Self {
            chains: Arc::new(EvmChainRegistry::default()),
            sessions: Arc::new(EvmSessionState::default()),
        }
    }
}
