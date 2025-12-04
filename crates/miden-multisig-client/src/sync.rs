//! State synchronization utilities.

use miden_client::Client;

use crate::error::{MultisigError, Result};

/// Syncs the miden-client state with the Miden network.
pub async fn sync_miden_state(client: &mut Client<()>) -> Result<()> {
    client
        .sync_state()
        .await
        .map_err(|e| MultisigError::MidenClient(format!("failed to sync state: {}", e)))?;
    Ok(())
}
