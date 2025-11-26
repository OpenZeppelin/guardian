use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::PsmError;
use crate::services::ResolvedAccount;
use crate::state::AppState;
use crate::state_object::StateObject;
use tracing::{error, info, warn};

pub struct CommitContext<'a> {
    pub state: &'a AppState,
    pub resolved: &'a ResolvedAccount,
    pub current_state: &'a StateObject,
    pub now: String,
}

#[derive(Clone)]
pub enum DeltaCommitStrategy {
    Candidate,
    Optimistic,
}

impl DeltaCommitStrategy {
    pub fn from_app_state(state: &AppState) -> Self {
        if state.canonicalization.is_some() {
            Self::Candidate
        } else {
            Self::Optimistic
        }
    }

    pub async fn commit(
        &self,
        ctx: CommitContext<'_>,
        delta: &mut DeltaObject,
        new_state_json: serde_json::Value,
        new_commitment: &str,
    ) -> Result<(), PsmError> {
        match self {
            DeltaCommitStrategy::Candidate => {
                delta.status = DeltaStatus::candidate(ctx.now.clone());
                ctx.resolved.backend.submit_delta(delta).await.map_err(|e| {
                    error!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        error = %e,
                        "Failed to submit candidate delta"
                    );
                    PsmError::StorageError(format!("Failed to submit delta: {e}"))
                })
            }
            DeltaCommitStrategy::Optimistic => {
                delta.status = DeltaStatus::canonical(ctx.now.clone());

                let new_state = StateObject {
                    account_id: delta.account_id.clone(),
                    commitment: new_commitment.to_string(),
                    state_json: new_state_json,
                    created_at: ctx.current_state.created_at.clone(),
                    updated_at: ctx.now.clone(),
                };

                ctx.resolved
                    .backend
                    .submit_state(&new_state)
                    .await
                    .map_err(|e| {
                        error!(
                            account_id = %delta.account_id,
                            error = %e,
                            "Failed to update state in optimistic mode"
                        );
                        PsmError::StorageError(format!("Failed to update state: {e}"))
                    })?;

                ctx.resolved
                    .backend
                    .submit_delta(delta)
                    .await
                    .map_err(|e| {
                        error!(
                            account_id = %delta.account_id,
                            nonce = delta.nonce,
                            error = %e,
                            "Failed to submit canonical delta in optimistic mode"
                        );
                        PsmError::StorageError(format!("Failed to submit delta: {e}"))
                    })?;

                // Delete matching proposal now that delta is canonical
                let proposal_id = {
                    let client = ctx.state.network_client.lock().await;
                    client
                        .delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload)
                        .ok()
                };

                if let Some(ref id) = proposal_id
                    && let Ok(_existing_proposal) = ctx
                        .resolved
                        .backend
                        .pull_delta_proposal(&delta.account_id, id)
                        .await
                {
                    info!(
                        account_id = %delta.account_id,
                        proposal_id = %id,
                        "Deleting matching proposal as delta is now canonical"
                    );
                    if let Err(e) = ctx
                        .resolved
                        .backend
                        .delete_delta_proposal(&delta.account_id, id)
                        .await
                    {
                        warn!(
                            account_id = %delta.account_id,
                            proposal_id = %id,
                            error = %e,
                            "Failed to delete proposal, but continuing"
                        );
                    }
                }

                Ok(())
            }
        }
    }
}
