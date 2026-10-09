//! Guardian sessions (issue #219): the key manager signs one session grant,
//! then a delegated P-256 signer signs the session-eligible Guardian requests
//! of every operation until the session ends.

use guardian_client::{SessionInfo, SessionSlot, StartSessionOptions};

use super::MultisigClient;
use crate::error::Result;

impl MultisigClient {
    /// Starts a Guardian session: the key manager signs one grant, then a
    /// delegated signer signs this client's state and delta reads and its
    /// proposal list, get, create and sign requests until the session ends.
    /// Account registration, delta pushes, candidate abandons, account lookup
    /// and transaction approvals keep using the key manager.
    ///
    /// A session this client already holds is logged out, best effort, once
    /// the new one is registered. Changing the Guardian endpoint drops the
    /// session.
    pub async fn start_session(&mut self, options: StartSessionOptions) -> Result<SessionInfo> {
        let session = SessionSlot::default();
        let info = self
            .create_guardian_client()
            .await?
            .with_signer(self.key_manager.clone())
            .with_session_slot(session.clone())
            .start_session(options)
            .await?;
        let previous = std::mem::replace(&mut self.guardian_session, session);
        if previous.info().is_some() {
            self.log_out(previous).await;
        }
        Ok(info)
    }

    /// The active Guardian session, if one is registered and not about to
    /// expire. A session Guardian ended or no longer accepts is dropped by the
    /// operation that learns of it, and this returns `None` afterwards.
    pub fn session(&self) -> Option<SessionInfo> {
        self.guardian_session.info()
    }

    /// Logs out the current session, signed by the session key. Returns
    /// whether Guardian still had it active; `false` when there is none. On
    /// error the session stays in use.
    pub async fn end_session(&self) -> Result<bool> {
        Ok(self
            .create_authenticated_guardian_client()
            .await?
            .revoke_session()
            .await?)
    }

    /// Revokes every session of this client's key on the Guardian, including
    /// sessions started elsewhere, and stops using the current one. Signed by
    /// the key manager. Use it when a session key may be compromised, and run
    /// it again 10 minutes later: it ends only sessions already registered,
    /// and a grant that was signed but not yet registered can still be
    /// registered for up to about 10 minutes. Returns how many sessions
    /// Guardian revoked.
    pub async fn revoke_all_sessions(&self) -> Result<u64> {
        Ok(self
            .create_authenticated_guardian_client()
            .await?
            .revoke_all_sessions()
            .await?)
    }

    async fn log_out(&self, session: SessionSlot) {
        if let Ok(client) = self.create_guardian_client().await {
            let _ = client
                .with_signer(self.key_manager.clone())
                .with_session_slot(session)
                .revoke_session()
                .await;
        }
    }
}
