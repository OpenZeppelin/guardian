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
        let chains = Arc::new(EvmChainRegistry::from_env()?);
        let sessions = Arc::new(EvmSessionState::from_env()?);

        Ok(Self { chains, sessions })
    }

    pub fn for_tests() -> Self {
        Self {
            chains: Arc::new(EvmChainRegistry::default()),
            sessions: Arc::new(EvmSessionState::default()),
        }
    }
}
