//! Server-side transaction execution for GUARDIAN-executed proposals (issue #254).
//!
//! Gate 0 spike scope: ephemeral chain-view assembly and the
//! [`DataStore`](miden_tx::DataStore) seam. Execution and proving are validated in tests;
//! service-level orchestration and live submission are not wired here.

mod blockchain;
mod store;

pub use blockchain::{ChainView, build_chain_view, verify_against_reference};
pub use store::ExecutionDataStore;

#[cfg(all(test, feature = "e2e"))]
mod tests;

// Live-network checks need only the `proving` feature: they are read-only RPC queries with no
// MockChain involvement.
#[cfg(test)]
mod live_tests;
