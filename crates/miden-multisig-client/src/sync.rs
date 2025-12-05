//! State synchronization utilities.

use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use miden_client::Client;

use crate::error::{MultisigError, Result};

/// Syncs the miden-client state with the Miden network.
///
/// This function wraps the sync call with panic handling for the miden-client v0.12.x
/// issue where partial MMR state corruption can cause panics during sync.
pub async fn sync_miden_state(client: &mut Client<()>) -> Result<()> {
    // WORKAROUND: https://github.com/0xMiden/crypto/issues/693#issuecomment-3617553447
    // miden-client v0.12.x can panic during sync_state() when the local
    // partial MMR state becomes inconsistent. This happens in miden-crypto's
    // partial_mmr.rs with the assertion "if there is an odd element, a merge is required".
    //
    // We catch this panic and convert it to an error so the caller can recover.
    // TODO: Remove this workaround when miden-client is updated with a fix.
    let result = AssertUnwindSafe(client.sync_state()).catch_unwind().await;

    match result {
        Ok(Ok(_sync_summary)) => Ok(()),
        Ok(Err(e)) => Err(MultisigError::MidenClient(format!(
            "failed to sync state: {}",
            e
        ))),
        Err(panic_info) => {
            // Extract panic message if possible
            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };

            // Use SyncPanicked to distinguish from regular errors - only panics
            // should trigger recovery (not network errors, timeouts, etc.)
            Err(MultisigError::SyncPanicked(msg))
        }
    }
}
