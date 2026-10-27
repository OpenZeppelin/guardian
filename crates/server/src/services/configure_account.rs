use crate::error::{GuardianError, Result};
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials};
use crate::metadata::{AccountMetadata, ClearTransition};
use crate::network::{RpcReadMode, StateVerification};
use crate::services::{consume_auth_timestamp, validate_request_timestamp};
use crate::state::AppState;
use crate::state_object::{StateHead, StateObject};
use crate::storage::StateReplace;
use guardian_shared::SignatureScheme;

#[derive(Debug, Clone)]
pub struct ConfigureAccountParams {
    pub account_id: String,
    pub auth: Auth,
    pub network_config: NetworkConfig,
    pub initial_state: serde_json::Value,
    pub credential: Credentials,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountResult {
    pub account_id: String,
    pub ack_pubkey: String,
    pub ack_commitment: String,
}

/// Configure a new account
#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> Result<ConfigureAccountResult> {
    tracing::debug!("Configuring account");

    if params.credential.auth_format() == crate::metadata::auth::RequestAuthFormat::Session {
        validate_request_timestamp(state, &params.account_id, &params.credential)?;
        let message = super::auth_request_word(&params.account_id, &params.credential)?;
        return Err(super::reject_session_credentials(state, &params.credential, message).await);
    }

    let network_config = params
        .network_config
        .validate_for_account(&params.account_id)
        .map_err(GuardianError::InvalidNetworkConfig)?;

    if network_config.is_evm() || matches!(params.auth, Auth::EvmEcdsa { .. }) {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "configure".to_string(),
        });
    }

    params
        .auth
        .validate_canonical_signer_set()
        .map_err(GuardianError::InvalidInput)?;

    let request_timestamp =
        validate_request_timestamp(state, &params.account_id, &params.credential)?;

    let existing = state.metadata.get(&params.account_id).await.map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to check existing account in configure_account"
        );
        GuardianError::StorageError(format!("Failed to check existing account: {e}"))
    })?;
    // Metadata without a stored state is a first configuration that claimed
    // the account below and then failed before writing its state (a storage
    // error after the claim). Its retry finishes that first configuration:
    // it is held to the scheme policy for new accounts and counted as a
    // creation. A failed state read keeps the reconfiguration treatment, so
    // it can never block an account that does have state.
    let unfinished_claim = match &existing {
        Some(_) => match state
            .storage
            .pull_state_commitment(&params.account_id)
            .await
        {
            Ok(_) => false,
            Err(e) if crate::storage::is_storage_not_found(&e) => true,
            Err(e) => {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "Could not check for a stored state; treating the account as configured"
                );
                false
            }
        },
        None => false,
    };
    // The GUARDIAN's scheme selects this server's ACK identity for the
    // account; every scheme the account uses, its cosigners' included, is
    // held to the policy for new accounts. A reconfiguration is held to it
    // for every scheme the stored auth does not use yet, so an account
    // cannot register under an allowed scheme and then switch to another;
    // re-onboarding under the schemes it already has stays open, whatever
    // the policy is now.
    let guardian_scheme = params.auth.guardian_scheme();
    let registered_schemes = match &existing {
        Some(metadata) if !unfinished_claim => metadata.auth.schemes_in_use(),
        _ => Vec::new(),
    };
    for scheme in params.auth.schemes_in_use() {
        if !registered_schemes.contains(&scheme) {
            reject_disallowed_scheme(state, scheme)?;
        }
    }

    let (head, signer_commitment) = {
        let client = &state.network_client;
        let expected_guardian_commitment = state.ack.commitment(&guardian_scheme);

        // Validates that the credential is valid for the account state.
        client
            .validate_credential(&params.initial_state, &params.credential, &params.auth)
            .map_err(|e| {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to validate credential"
                );
                GuardianError::NetworkError(format!("Failed to validate credential: {e}"))
            })?;

        client
            .validate_guardian_commitment(
                &params.initial_state,
                &expected_guardian_commitment,
                guardian_scheme,
            )
            .map_err(|e| {
                tracing::warn!(
                    account_id = %params.account_id,
                    expected_guardian_commitment = %expected_guardian_commitment,
                    error = %e,
                    "Unauthorized account configuration: invalid GUARDIAN public key binding"
                );
                GuardianError::AuthorizationFailed(format!(
                    "Unauthorized account configuration: {e}"
                ))
            })?;

        // Guardian acknowledges a delta only after the multisig push gate
        // counts verified cosigner approvals, so the account must be a
        // guarded multisig. The chain verifies each approver under the scheme
        // the state stores for it, so a state missing one would fail every
        // transaction that approver signs. Without this check the cosigner
        // comparison below would take the declared scheme for such an
        // approver and pass.
        client
            .validate_multisig_auth(&params.initial_state)
            .map_err(|e| {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "initial_state is not a valid guarded multisig in configure_account"
                );
                GuardianError::InvalidInput(format!("Invalid multisig auth in initial_state: {e}"))
            })?;

        // The stored cosigner list is the authorization source of truth for
        // every later request (`Auth::verify`), so the full client-declared
        // set must match the signer set actually stored in the submitted
        // account state — `validate_credential` above only proves the
        // *requesting* key is a signer, not the rest of the list (#102).
        // `None` means the state carries no extractable signer set, matching
        // the canonicalization-side semantics of `should_update_auth`; a
        // guarded multisig, checked above, always carries one.
        let extracted_auth = client
            .should_update_auth(&params.initial_state, &params.auth)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to extract signer commitments from initial state"
                );
                GuardianError::NetworkError(format!(
                    "Failed to extract signer commitments from initial state: {e}"
                ))
            })?;
        if let Some(expected_auth) = extracted_auth
            && expected_auth != params.auth
        {
            tracing::warn!(
                account_id = %params.account_id,
                expected = ?expected_auth,
                provided = ?params.auth,
                "Cosigner commitments do not match account state in configure_account"
            );
            // Commitments that match with an auth that still differs differ
            // in schemes: the declared variant or a cosigner's scheme is not
            // the one the state stores.
            if expected_auth.cosigner_commitments() == params.auth.cosigner_commitments() {
                return Err(GuardianError::InvalidInput(format!(
                    "cosigner schemes do not match the signer set in initial_state: expected {}, provided {}",
                    describe_cosigners(&expected_auth),
                    describe_cosigners(&params.auth)
                )));
            }
            return Err(GuardianError::InvalidInput(format!(
                "cosigner_commitments do not match the signer set in initial_state: expected {:?}, provided {:?}",
                expected_auth.cosigner_commitments(),
                params.auth.cosigner_commitments()
            )));
        }

        // Verifies the credential authorization.
        let signer_commitment = params
            .auth
            .verify(&params.account_id, &params.credential)
            .map_err(|e| {
                tracing::warn!(
                    account_id = %params.account_id,
                    error = %e,
                    "Signature verification failed in configure_account"
                );
                GuardianError::AuthenticationFailed(format!("Signature verification failed: {e}"))
            })?;

        // Commitment and nonce of the account state, from one decode; both
        // are stored with the state.
        let head = client
            .get_state_head(&params.account_id, &params.initial_state)
            .map_err(GuardianError::NetworkError)?;
        (head, signer_commitment)
    };

    let now = state.clock.now_rfc3339();

    // A first configuration claims the account before anything else: of two
    // concurrent first configurations (a client retry, or a replay of the
    // signed request) exactly one creates the metadata row, and the other
    // is judged below as a reconfiguration of the account it lost to. Its
    // replay timestamp is then consumed before it writes anything, so a
    // duplicate is refused instead of storing the initial state over
    // whatever the account has moved to since.
    let created = existing.is_none()
        && state
            .metadata
            .create(AccountMetadata {
                account_id: params.account_id.clone(),
                auth: params.auth.clone(),
                network_config: network_config.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            })
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to create account metadata"
                );
                GuardianError::StorageError(format!("Failed to create account metadata: {e}"))
            })?;
    let existing = if existing.is_none() && !created {
        let claimed = state.metadata.get(&params.account_id).await.map_err(|e| {
            GuardianError::StorageError(format!("Failed to re-read account metadata: {e}"))
        })?;
        Some(claimed.ok_or_else(|| {
            GuardianError::StorageError(
                "Account metadata vanished after a concurrent first configuration".to_string(),
            )
        })?)
    } else {
        existing
    };

    // The replay-state row has an FK to account metadata, which the claim
    // above guarantees, so every configuration consumes its timestamp
    // before it writes any state.
    consume_auth_timestamp(
        state,
        &params.account_id,
        &signer_commitment,
        request_timestamp,
    )
    .await?;

    // A reconfiguration takes its approval rules from the chain, so the
    // submitted state must be the account's on-chain state, whenever the
    // stored account cannot vouch for them: it was released, and its stored
    // signers may have changed since; the requesting key is not one of its
    // stored signers; or the submitted state changes who may approve or how
    // many approvals a transaction needs. A stored cosigner re-registering
    // under the stored rules needs no chain read.
    let vouch = match &existing {
        None => Vouch::NoStoredState,
        Some(existing)
            if existing.released_at.is_some()
                || existing.auth.signer_scheme(&signer_commitment).is_none() =>
        {
            Vouch::Chain
        }
        Some(_) => vouch_by_stored_rules(state, &params.account_id, &params.initial_state).await?,
    };
    if let Vouch::Chain = vouch {
        ensure_on_chain_state(state, &params.account_id, &head.commitment).await?;
    }

    // The stored state this re-onboarding replaces, read only once the
    // request is authenticated and fresh. The write below refuses to
    // overwrite a state promoted after this read.
    let stored_base = match &existing {
        Some(_) => stored_head(state, &params.account_id).await?,
        None => None,
    };

    // The stored rules vouch only for the state they were read from: one
    // promoted since, such as a landed removal of the requesting signer, has
    // rules of its own, and the write below must not replace it.
    if let Vouch::StoredRules { commitment } = &vouch
        && stored_base.as_ref().map(|stored| &stored.commitment) != Some(commitment)
    {
        let current = stored_base
            .as_ref()
            .map_or_else(String::new, |stored| stored.commitment.clone());
        tracing::warn!(
            account_id = %params.account_id,
            compared = %commitment,
            current = %current,
            "The stored state moved after its approval rules were compared; refusing to overwrite it"
        );
        return Err(GuardianError::CommitmentMismatch {
            expected: current,
            actual: head.commitment.clone(),
        });
    }

    // A state older than the stored one may replace it only while the stored
    // state never landed (an optimistic-mode state ahead of the chain): once
    // the chain reached it, the older state is a stale wallet's, and storing
    // it would roll back a promoted delta, a guardian switch included.
    if let Some(stored) = &stored_base
        && let (Some(stored_nonce), Some(submitted_nonce)) = (stored.nonce, head.nonce)
        && submitted_nonce < stored_nonce
        && stored_state_landed(state, &params.account_id, &stored.commitment).await?
    {
        tracing::warn!(
            account_id = %params.account_id,
            stored = %stored.commitment,
            stored_nonce,
            submitted = %head.commitment,
            submitted_nonce,
            "Refusing to re-onboard with a state older than the landed stored state"
        );
        return Err(GuardianError::CommitmentMismatch {
            expected: stored.commitment.clone(),
            actual: head.commitment.clone(),
        });
    }

    let created_at = existing
        .as_ref()
        .map(|m| m.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let account_state = StateObject {
        account_id: params.account_id.clone(),
        state_json: params.initial_state,
        commitment: head.commitment,
        nonce: head.nonce,
        created_at: created_at.clone(),
        updated_at: now.clone(),
        auth_scheme: params.auth.auth_scheme_label().to_string(),
    };

    let write = match &stored_base {
        Some(stored) => {
            state
                .storage
                .replace_state(&account_state, &stored.commitment)
                .await
        }
        None => state
            .storage
            .submit_state(&account_state)
            .await
            .map(|()| StateReplace::Replaced),
    }
    .map_err(|e| {
        tracing::error!(
            account_id = %params.account_id,
            error = %e,
            "Failed to submit initial state"
        );
        GuardianError::StorageError(format!("Failed to submit initial state: {e}"))
    })?;
    if let StateReplace::StateMoved { current } = write {
        tracing::warn!(
            account_id = %params.account_id,
            current = %current,
            submitted = %account_state.commitment,
            "The stored state moved while re-onboarding; refusing to overwrite it"
        );
        return Err(GuardianError::CommitmentMismatch {
            expected: current,
            actual: account_state.commitment,
        });
    }

    // configure_account is an admin/setup path and intentionally NOT
    // gated by the pause chokepoint. Pause must not block account
    // reconfiguration — do not add the chokepoint here. Pause state
    // is carried forward from `existing` so a new storage backend
    // cannot accidentally clear it (the field is only mutated by
    // `set_pause`/`clear_pause`).
    if let Some(existing) = &existing {
        let metadata_entry = AccountMetadata {
            account_id: params.account_id.clone(),
            auth: params.auth,
            network_config,
            created_at,
            updated_at: now,
            has_pending_candidate: existing.has_pending_candidate,
            paused_at: existing.paused_at,
            paused_reason: existing.paused_reason.clone(),
            // `set` never touches released state; the explicit
            // `clear_released_if_state` below performs the reactivation.
            released_at: existing.released_at,
        };

        state.metadata.set(metadata_entry).await.map_err(|e| {
            tracing::error!(
                account_id = %params.account_id,
                error = %e,
                "Failed to store metadata"
            );
            GuardianError::StorageError(format!("Failed to store metadata: {e}"))
        })?;
    }

    // Deliberate asymmetry with pause: `released` means "a guardian
    // switch moved this account away from this server", and this very
    // path just re-validated (validate_guardian_commitment above) that
    // the submitted state binds the account to this server again — so
    // re-onboarding is exactly the reactivation event. Pause, an
    // operator decision, stays in force across reconfiguration.
    //
    // Only a state the chain vouched for reactivates the account. The clear
    // runs after `submit_state`, conditional on the stored state, atomically,
    // so whichever state is stored last decides: a release proved against an
    // older state is undone here, and a switch delta that replaced this state
    // after `submit_state` keeps its release. A state the stored rules
    // vouched for was checked against an account that was not released; a
    // release written since (the release sweep proving a switch against the
    // previous state) is undone only once the submitted state proves to be
    // the account's on-chain state, and stands otherwise.
    let chain_vouched = match vouch {
        Vouch::Chain => true,
        Vouch::StoredRules { .. } | Vouch::NoStoredState if created => false,
        Vouch::StoredRules { .. } | Vouch::NoStoredState => {
            let current = state.metadata.get(&params.account_id).await.map_err(|e| {
                GuardianError::StorageError(format!("Failed to re-read account metadata: {e}"))
            })?;
            if current.is_some_and(|metadata| metadata.released_at.is_some()) {
                tracing::warn!(
                    account_id = %params.account_id,
                    submitted = %account_state.commitment,
                    "The account was released while it was reconfigured without a chain read"
                );
                ensure_on_chain_state(state, &params.account_id, &account_state.commitment).await?;
                true
            } else {
                false
            }
        }
    };
    if !created && chain_vouched {
        let cleared = state
            .metadata
            .clear_released_if_state(
                &params.account_id,
                &account_state.commitment,
                state.storage.as_ref(),
            )
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.account_id,
                    error = %e,
                    "Failed to clear released state during re-onboarding"
                );
                GuardianError::StorageError(format!("Failed to clear released state: {e}"))
            })?;
        match cleared {
            ClearTransition::Cleared => tracing::info!(
                account_id = %params.account_id,
                "Reactivated released account via /configure re-onboarding"
            ),
            ClearTransition::NotReleased => {}
            ClearTransition::StateMoved => tracing::warn!(
                account_id = %params.account_id,
                "A delta replaced the re-onboarded state before its release was cleared; \
                 the release of that later state stands"
            ),
        }
    }

    // Count only first-time creations, including the retry that finishes an
    // unfinished claim: /configure also serves reconfiguration of existing
    // accounts.
    let created = created || unfinished_claim;
    if created {
        metrics::counter!(
            crate::metrics::names::ACCOUNTS_CREATED_TOTAL,
            crate::metrics::names::LABEL_KIND =>
                crate::metrics::labels::AccountKind::Miden.as_str()
        )
        .increment(1);
    }

    tracing::info!(reconfiguration = !created, "Account configured");

    Ok(ConfigureAccountResult {
        account_id: params.account_id,
        ack_pubkey: state.ack.pubkey(&guardian_scheme),
        ack_commitment: state.ack.commitment(&guardian_scheme),
    })
}

/// `commitment:scheme` for each cosigner, for the scheme-mismatch error.
fn describe_cosigners(auth: &Auth) -> String {
    let cosigners: Vec<String> = auth
        .cosigners()
        .into_iter()
        .map(|cosigner| format!("{}:{}", cosigner.commitment, cosigner.scheme))
        .collect();
    format!("[{}]", cosigners.join(", "))
}

fn reject_disallowed_scheme(state: &AppState, scheme: SignatureScheme) -> Result<()> {
    let allowed = state.ack.account_schemes();
    if allowed.allows(scheme) {
        return Ok(());
    }
    tracing::warn!(
        scheme = scheme.as_str(),
        allowed_schemes = %allowed,
        "Rejected account registration: signature scheme not allowed"
    );
    Err(GuardianError::SignatureSchemeNotAllowed {
        scheme: scheme.as_str().to_string(),
        allowed_schemes: allowed.names(),
    })
}

/// The stored state's commitment and nonce, or `None` when the account has
/// no stored state. A row written before the server stored nonces carries
/// none: it is decoded from the stored state, and the commitment is taken
/// from that same read so both describe one state. A stored state that does
/// not decode cannot be a promoted delta's; its nonce stays unknown, so a
/// corrupt row can still be replaced by re-onboarding.
async fn stored_head(state: &AppState, account_id: &str) -> Result<Option<StateHead>> {
    let storage_error = |e: String| {
        tracing::error!(
            account_id = %account_id,
            error = %e,
            "Failed to read the stored state in configure_account"
        );
        GuardianError::StorageError(format!("Failed to read the stored state: {e}"))
    };
    let stored = match state.storage.pull_state_head(account_id).await {
        Ok(stored) => stored,
        Err(e) if crate::storage::is_storage_not_found(&e) => return Ok(None),
        Err(e) => return Err(storage_error(e)),
    };
    if stored.nonce.is_some() {
        return Ok(Some(stored));
    }
    let full = state
        .storage
        .pull_state(account_id)
        .await
        .map_err(storage_error)?;
    let nonce = match state
        .network_client
        .get_state_head(account_id, &full.state_json)
    {
        Ok(decoded) => decoded.nonce,
        Err(e) => {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "Stored state does not decode; re-onboarding replaces it without the stale-state check"
            );
            None
        }
    };
    Ok(Some(StateHead {
        commitment: full.commitment,
        nonce,
    }))
}

/// What vouches for a configuration's approval rules.
enum Vouch {
    /// The chain: the submitted state must be the account's on-chain state.
    Chain,
    /// The stored state with `commitment`, whose approval rules the
    /// submitted state keeps.
    StoredRules { commitment: String },
    /// Nothing needs to: a new account, or one with no stored state yet
    /// (an unfinished first configuration).
    NoStoredState,
}

/// Vouches for `submitted_state` by the stored state when it keeps the
/// stored state's approval rules, and by the chain otherwise. A stored
/// state whose rules cannot be read keeps none.
async fn vouch_by_stored_rules(
    state: &AppState,
    account_id: &str,
    submitted_state: &serde_json::Value,
) -> Result<Vouch> {
    let stored = match state.storage.pull_state(account_id).await {
        Ok(stored) => stored,
        Err(e) if crate::storage::is_storage_not_found(&e) => return Ok(Vouch::NoStoredState),
        Err(e) => {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to read the stored state in configure_account"
            );
            return Err(GuardianError::StorageError(format!(
                "Failed to read the stored state: {e}"
            )));
        }
    };
    let submitted = state
        .network_client
        .approval_rules(submitted_state)
        .map_err(|e| {
            GuardianError::InvalidInput(format!(
                "Could not read the approval rules of initial_state: {e}"
            ))
        })?;
    match state.network_client.approval_rules(&stored.state_json) {
        Ok(rules) if rules == submitted => Ok(Vouch::StoredRules {
            commitment: stored.commitment,
        }),
        Ok(_) => Ok(Vouch::Chain),
        Err(e) => {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "Stored state's approval rules do not decode; reconfiguration requires the on-chain state"
            );
            Ok(Vouch::Chain)
        }
    }
}

/// Requires `commitment` to be the account's current on-chain state. An
/// earlier on-chain state does not qualify: its signer set may have changed
/// since, and since the chain has moved past it, waiting does not help. A
/// history hit is checked against the head once more, since the transaction
/// that ends at `commitment` may have landed after the first head read.
async fn ensure_on_chain_state(state: &AppState, account_id: &str, commitment: &str) -> Result<()> {
    let unavailable = |e: String| {
        GuardianError::RpcUnavailable(format!(
            "Could not check the submitted state against the chain: {e}"
        ))
    };
    let verification = state
        .network_client
        .verify_commitment(account_id, commitment, RpcReadMode::Configured)
        .await
        .map_err(unavailable)?;
    let on_chain = match verification {
        StateVerification::Match => return Ok(()),
        StateVerification::Absent => None,
        StateVerification::Mismatch { on_chain } => Some(on_chain),
    };
    if let Some(on_chain) = on_chain
        && state
            .network_client
            .find_transaction_ending_at(account_id, commitment, 0, RpcReadMode::Configured)
            .await
            .map_err(unavailable)?
            .found_in_block
            .is_some()
    {
        if let StateVerification::Match = state
            .network_client
            .verify_commitment(account_id, commitment, RpcReadMode::Configured)
            .await
            .map_err(unavailable)?
        {
            return Ok(());
        }
        tracing::warn!(
            account_id = %account_id,
            submitted = %commitment,
            on_chain = %on_chain,
            "Reconfiguration state is behind the account's on-chain state"
        );
        return Err(GuardianError::CommitmentMismatch {
            expected: on_chain,
            actual: commitment.to_string(),
        });
    }
    tracing::warn!(
        account_id = %account_id,
        submitted = %commitment,
        "Reconfiguration state is not the account's on-chain state"
    );
    Err(GuardianError::StateNotOnChain {
        submitted: commitment.to_string(),
    })
}

/// Whether the account's chain reached `commitment`: the chain sits at it,
/// or a committed transaction ended at it. A chain that cannot be read
/// refuses the request rather than risk storing an older state over one
/// the chain already holds.
async fn stored_state_landed(state: &AppState, account_id: &str, commitment: &str) -> Result<bool> {
    let unavailable = |e: String| {
        GuardianError::RpcUnavailable(format!(
            "Could not check whether the stored state is on chain: {e}"
        ))
    };
    match state
        .network_client
        .verify_commitment(account_id, commitment, RpcReadMode::Configured)
        .await
        .map_err(unavailable)?
    {
        StateVerification::Match => Ok(true),
        StateVerification::Absent => Ok(false),
        StateVerification::Mismatch { .. } => Ok(state
            .network_client
            .find_transaction_ending_at(account_id, commitment, 0, RpcReadMode::Configured)
            .await
            .map_err(unavailable)?
            .found_in_block
            .is_some()),
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::network::{ApprovalRules, AuthBinding};
    use crate::state_object::StateHead;
    use crate::storage::StorageBackend;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    /// The head the mock network client reports for the configured state.
    fn head(commitment: &str, nonce: u64) -> StateHead {
        StateHead {
            commitment: commitment.to_string(),
            nonce: Some(nonce),
        }
    }

    async fn create_test_app_state(
        network_client: MockNetworkClient,
        storage_backend: MockStorageBackend,
        metadata_store: MockMetadataStore,
    ) -> AppState {
        let storage = Arc::new(storage_backend) as Arc<dyn StorageBackend>;

        let keystore_dir =
            std::env::temp_dir().join(format!("test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");

        let ack = AckRegistry::new(keystore_dir)
            .await
            .expect("Failed to create ack registry");

        AppState {
            miden_sessions: std::sync::Arc::new(crate::session::MidenSessions::default()),
            storage,
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(network_client),
            ack,
            canonicalization: None, // Optimistic mode for tests
            release_sweep: None,
            clock: Arc::new(crate::clock::SystemClock),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    #[tokio::test]
    async fn test_configure_account_success() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let verified_signer_commitment = commitment_hex.clone();
        let other_signer_commitment = format!("0x{}", "ab".repeat(32));
        let configured_commitments =
            vec![other_signer_commitment, verified_signer_commitment.clone()];

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: configured_commitments.clone(),
            })))
            .with_get_state_head(Ok(head("0x1234", 3)));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let observed_storage = storage_backend.clone();

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));
        let observed_metadata = metadata_store.clone();

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        // Use a valid account JSON fixture
        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: configured_commitments,
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
        // The state is stored with the head the network client decoded.
        let stored = observed_storage.get_submit_state_calls();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].commitment, "0x1234");
        assert_eq!(stored[0].nonce, Some(3));
        let ack_pubkey = result.ack_pubkey;
        let ack_commitment = result.ack_commitment;
        assert!(!ack_pubkey.is_empty(), "ack_pubkey should not be empty");
        assert!(
            ack_pubkey.starts_with("0x"),
            "ack_pubkey should be hex format"
        );
        assert!(
            ack_commitment.starts_with("0x"),
            "ack_commitment should be hex format"
        );
        assert_eq!(
            observed_metadata.get_update_timestamp_cas_calls(),
            vec![(
                account_id_hex.to_string(),
                verified_signer_commitment,
                timestamp,
            )]
        );
    }

    #[tokio::test]
    async fn configure_rejects_new_account_whose_scheme_is_not_allowed() {
        use crate::config::account_schemes::AllowedAccountSchemes;
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let mut state = create_test_app_state(
            MockNetworkClient::new(),
            MockStorageBackend::new(),
            MockMetadataStore::new().with_get(Ok(None)),
        )
        .await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        match configure_account(&state, params).await {
            Err(GuardianError::SignatureSchemeNotAllowed {
                scheme,
                allowed_schemes,
            }) => {
                assert_eq!(scheme, "falcon");
                assert_eq!(allowed_schemes, vec!["ecdsa".to_string()]);
            }
            other => panic!("expected SignatureSchemeNotAllowed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn configure_keeps_serving_existing_accounts_whose_scheme_is_no_longer_allowed() {
        use crate::config::account_schemes::AllowedAccountSchemes;
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state(account_id_hex, "0x1234")))
            .with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let mut state =
            create_test_app_state(network_client, storage_backend, metadata_store).await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        let result = configure_account(&state, params).await;
        assert!(
            result.is_ok(),
            "existing Falcon account must reconfigure: {result:?}"
        );
    }

    fn configured(account_id_hex: &str, auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    /// Reconfigures an account stored with `stored` to `auth` on an
    /// ECDSA-only server, signed by the Falcon key `signer` returns.
    async fn reconfigure_on_an_ecdsa_only_server(
        account_id_hex: &str,
        stored: Auth,
        auth: Auth,
        signer: (String, String, i64),
    ) -> (
        Result<ConfigureAccountResult>,
        MockStorageBackend,
        MockMetadataStore,
    ) {
        use crate::config::account_schemes::AllowedAccountSchemes;

        let (pubkey_hex, signature_hex, timestamp) = signer;
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state(account_id_hex, "0x1234")))
            .with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(configured(account_id_hex, stored))))
            .with_set(Ok(()));
        let mut state = create_test_app_state(
            network_client,
            storage_backend.clone(),
            metadata_store.clone(),
        )
        .await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let initial_state: serde_json::Value =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };
        (
            configure_account(&state, params).await,
            storage_backend,
            metadata_store,
        )
    }

    /// The scheme policy holds for every scheme a reconfiguration adds:
    /// registering under an allowed scheme and then reconfiguring the same
    /// account under another, alone or next to the first, is refused
    /// before anything is written.
    #[tokio::test]
    async fn reconfiguration_to_a_scheme_the_account_does_not_use_is_held_to_the_policy() {
        use crate::metadata::auth::MidenCosigner;
        use crate::testing::helpers::{TestEcdsaSigner, generate_falcon_signature};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let ecdsa = TestEcdsaSigner::new();
        let stored = Auth::MidenEcdsa {
            cosigner_commitments: vec![ecdsa.commitment_hex.clone()],
        };
        let (pubkey_hex, falcon_commitment, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let switched = Auth::MidenFalconRpo {
            cosigner_commitments: vec![falcon_commitment.clone()],
        };
        let widened = Auth::Miden {
            cosigners: vec![
                MidenCosigner {
                    commitment: ecdsa.commitment_hex.clone(),
                    scheme: SignatureScheme::Ecdsa,
                },
                MidenCosigner {
                    commitment: falcon_commitment,
                    scheme: SignatureScheme::Falcon,
                },
            ],
            guardian_scheme: SignatureScheme::Ecdsa,
        };

        for auth in [switched, widened] {
            let (result, storage, metadata) = reconfigure_on_an_ecdsa_only_server(
                account_id_hex,
                stored.clone(),
                auth.clone(),
                (pubkey_hex.clone(), signature_hex.clone(), timestamp),
            )
            .await;
            match result {
                Err(GuardianError::SignatureSchemeNotAllowed {
                    scheme,
                    allowed_schemes,
                }) => {
                    assert_eq!(scheme, "falcon", "{auth:?}");
                    assert_eq!(allowed_schemes, vec!["ecdsa".to_string()]);
                }
                other => panic!("expected SignatureSchemeNotAllowed for {auth:?}, got {other:?}"),
            }
            assert!(storage.get_submit_state_calls().is_empty());
            assert!(storage.get_replace_state_calls().is_empty());
            assert!(metadata.get_set_calls().is_empty());
        }
    }

    /// A mixed account keeps re-onboarding under the schemes it already
    /// uses after the policy narrows to one of them.
    #[tokio::test]
    async fn reconfiguration_under_the_schemes_an_account_already_uses_is_not_held_to_the_policy() {
        use crate::metadata::auth::MidenCosigner;
        use crate::testing::helpers::{TestEcdsaSigner, generate_falcon_signature};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let ecdsa = TestEcdsaSigner::new();
        let (pubkey_hex, falcon_commitment, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let mixed = Auth::Miden {
            cosigners: vec![
                MidenCosigner {
                    commitment: ecdsa.commitment_hex.clone(),
                    scheme: SignatureScheme::Ecdsa,
                },
                MidenCosigner {
                    commitment: falcon_commitment,
                    scheme: SignatureScheme::Falcon,
                },
            ],
            guardian_scheme: SignatureScheme::Ecdsa,
        };

        let (result, _storage, metadata) = reconfigure_on_an_ecdsa_only_server(
            account_id_hex,
            mixed.clone(),
            mixed.clone(),
            (pubkey_hex, signature_hex, timestamp),
        )
        .await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(metadata.get_set_calls().len(), 1);
        assert_eq!(metadata.get_set_calls()[0].auth, mixed);
    }

    #[tokio::test]
    async fn retry_of_an_unfinished_first_configuration_is_held_to_the_new_account_scheme_policy() {
        use crate::config::account_schemes::AllowedAccountSchemes;
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let claimed_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        let storage_backend = MockStorageBackend::new()
            .with_pull_state(Err("Failed to pull state: Record not found".to_string()));
        let metadata_store = MockMetadataStore::new().with_get(Ok(Some(claimed_metadata)));

        let mut state =
            create_test_app_state(MockNetworkClient::new(), storage_backend, metadata_store).await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        match configure_account(&state, params).await {
            Err(GuardianError::SignatureSchemeNotAllowed { scheme, .. }) => {
                assert_eq!(scheme, "falcon");
            }
            other => panic!(
                "metadata without a stored state is an unfinished first configuration, \
                 held to the new-account scheme policy: got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn configure_rejects_timestamp_outside_the_skew_window() {
        let state = create_test_app_state(
            MockNetworkClient::new(),
            MockStorageBackend::new(),
            MockMetadataStore::new(),
        )
        .await;
        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!("0x{}", "aa".repeat(32))],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature("0x00".into(), "0x00".into(), 0),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationFailed(_))
        ));
    }

    #[tokio::test]
    async fn test_configure_account_success_for_ecdsa() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::auth_request_payload::AuthRequestPayload;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let signer = TestEcdsaSigner::new();

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x1234", 3)));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let auth = Auth::MidenEcdsa {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        };
        let request_body = serde_json::json!({
            "account_id": account_id_hex,
            "auth": auth.clone(),
            "network_config": crate::metadata::NetworkConfig::miden_default(),
            "initial_state": initial_state.clone(),
        });
        let request_payload = AuthRequestPayload::from_json_serializable(&request_body).unwrap();
        let (signature_hex, timestamp) = signer.sign_request(account_id_hex, &request_payload);

        let credential =
            Credentials::signature(signer.pubkey_hex.clone(), signature_hex, timestamp)
                .with_request_payload(request_payload);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
        let ack_pubkey = result.ack_pubkey;
        let ack_commitment = result.ack_commitment;
        assert!(ack_pubkey.starts_with("0x"));
        assert!(ack_commitment.starts_with("0x"));
        assert_eq!(ack_commitment.len(), 66);
        assert!(ack_pubkey.len() > 66);
    }

    fn mixed_auth(falcon: &str, ecdsa: &str, guardian_scheme: SignatureScheme) -> Auth {
        Auth::Miden {
            cosigners: vec![
                crate::metadata::auth::MidenCosigner {
                    commitment: falcon.to_string(),
                    scheme: SignatureScheme::Falcon,
                },
                crate::metadata::auth::MidenCosigner {
                    commitment: ecdsa.to_string(),
                    scheme: SignatureScheme::Ecdsa,
                },
            ],
            guardian_scheme,
        }
    }

    /// A mixed account registers with its per-signer auth, the ECDSA
    /// cosigner signing, and is acknowledged with the GUARDIAN's scheme.
    #[tokio::test]
    async fn configure_registers_a_mixed_scheme_account() {
        use crate::testing::helpers::{TestEcdsaSigner, TestSigner};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let auth = mixed_auth(
            &falcon.commitment_hex,
            &ecdsa.commitment_hex,
            SignatureScheme::Ecdsa,
        );

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(auth.clone())))
            .with_get_state_head(Ok(head("0x1234", 3)));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let observed_storage = storage_backend.clone();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));
        let observed_metadata = metadata_store.clone();
        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let (signature_hex, timestamp) =
            ecdsa.sign_with_timestamp(account_id_hex, chrono::Utc::now().timestamp_millis());
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: auth.clone(),
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(ecdsa.pubkey_hex.clone(), signature_hex, timestamp),
        };

        let result = configure_account(&state, params)
            .await
            .expect("mixed account registers");
        assert_eq!(
            result.ack_commitment,
            state.ack.commitment(&SignatureScheme::Ecdsa)
        );
        assert_eq!(result.ack_pubkey, state.ack.pubkey(&SignatureScheme::Ecdsa));
        assert_eq!(observed_metadata.get_create_calls()[0].auth, auth);
        assert_eq!(
            observed_storage.get_submit_state_calls()[0].auth_scheme,
            "mixed"
        );
    }

    #[tokio::test]
    async fn configure_rejects_a_mixed_account_using_a_disallowed_scheme() {
        use crate::config::account_schemes::AllowedAccountSchemes;
        use crate::testing::helpers::{TestEcdsaSigner, TestSigner};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        // Every cosigner ECDSA but the GUARDIAN Falcon counts too.
        let ecdsa_cosigners_falcon_guardian = Auth::Miden {
            cosigners: vec![crate::metadata::auth::MidenCosigner {
                commitment: ecdsa.commitment_hex.clone(),
                scheme: SignatureScheme::Ecdsa,
            }],
            guardian_scheme: SignatureScheme::Falcon,
        };
        for auth in [
            mixed_auth(
                &falcon.commitment_hex,
                &ecdsa.commitment_hex,
                SignatureScheme::Ecdsa,
            ),
            ecdsa_cosigners_falcon_guardian,
        ] {
            let mut state = create_test_app_state(
                MockNetworkClient::new(),
                MockStorageBackend::new(),
                MockMetadataStore::new().with_get(Ok(None)),
            )
            .await;
            state.ack = state
                .ack
                .clone()
                .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());
            let (signature_hex, timestamp) =
                ecdsa.sign_with_timestamp(account_id_hex, chrono::Utc::now().timestamp_millis());
            let params = ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth,
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state: serde_json::json!({}),
                credential: Credentials::signature(
                    ecdsa.pubkey_hex.clone(),
                    signature_hex,
                    timestamp,
                ),
            };
            match configure_account(&state, params).await {
                Err(GuardianError::SignatureSchemeNotAllowed {
                    scheme,
                    allowed_schemes,
                }) => {
                    assert_eq!(scheme, "falcon");
                    assert_eq!(allowed_schemes, vec!["ecdsa".to_string()]);
                }
                other => panic!("expected SignatureSchemeNotAllowed, got {other:?}"),
            }
        }

        // An all-ECDSA per-signer account passes the same policy.
        let mut state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            MockStorageBackend::new().with_submit_state(Ok(())),
            MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(())),
        )
        .await;
        state.ack = state
            .ack
            .clone()
            .with_account_schemes(AllowedAccountSchemes::parse("ecdsa").unwrap());
        let (signature_hex, timestamp) =
            ecdsa.sign_with_timestamp(account_id_hex, chrono::Utc::now().timestamp_millis());
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::Miden {
                cosigners: vec![crate::metadata::auth::MidenCosigner {
                    commitment: ecdsa.commitment_hex.clone(),
                    scheme: SignatureScheme::Ecdsa,
                }],
                guardian_scheme: SignatureScheme::Ecdsa,
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(ecdsa.pubkey_hex.clone(), signature_hex, timestamp),
        };
        configure_account(&state, params)
            .await
            .expect("an all-ECDSA account is allowed");
    }

    /// A single-scheme declaration of a state that mixes schemes is refused
    /// with the schemes named, and nothing is written.
    #[tokio::test]
    async fn configure_rejects_a_single_scheme_declaration_of_a_mixed_state() {
        use crate::testing::helpers::{TestEcdsaSigner, TestSigner};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let stored = mixed_auth(
            &falcon.commitment_hex,
            &ecdsa.commitment_hex,
            SignatureScheme::Falcon,
        );
        let storage_backend = MockStorageBackend::new();
        let observed_storage = storage_backend.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_should_update_auth(Ok(Some(stored))),
            storage_backend,
            MockMetadataStore::new().with_get(Ok(None)),
        )
        .await;
        let (signature_hex, timestamp) = falcon.sign(account_id_hex);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![
                    falcon.commitment_hex.clone(),
                    ecdsa.commitment_hex.clone(),
                ],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({}),
            credential: Credentials::signature(falcon.pubkey_hex.clone(), signature_hex, timestamp),
        };
        match configure_account(&state, params).await {
            Err(GuardianError::InvalidInput(message)) => {
                assert!(
                    message.contains("cosigner schemes do not match"),
                    "{message}"
                );
                assert!(
                    message.contains(&format!("{}:ecdsa", ecdsa.commitment_hex)),
                    "{message}"
                );
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        assert!(observed_storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_configure_account_already_exists_reconfigures() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));

        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok(), "Reconfiguration should succeed");
        let result = result.unwrap();
        assert_eq!(result.account_id, account_id_hex);
    }

    /// Who reconfigures an existing account: one of its stored cosigners, a
    /// stored cosigner of an account released by a guardian switch, or a key
    /// the stored auth does not list.
    #[derive(Clone, Copy)]
    enum Reconfigurer {
        StoredCosigner,
        StoredCosignerChangingTheRules,
        StoredCosignerRacingAPromotion,
        ReleasedStoredCosigner,
        NewSigner,
    }

    /// Reconfigures an existing account whose submitted state the chain
    /// answers `chain` for.
    async fn reconfigure_by(
        reconfigurer: Reconfigurer,
        chain: StateVerification,
    ) -> (
        Result<ConfigureAccountResult>,
        MockNetworkClient,
        MockStorageBackend,
        MockMetadataStore,
    ) {
        use crate::testing::helpers::{TestSigner, generate_falcon_signature};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let (stored_cosigner, released_at) = match reconfigurer {
            Reconfigurer::StoredCosigner
            | Reconfigurer::StoredCosignerChangingTheRules
            | Reconfigurer::StoredCosignerRacingAPromotion => (commitment_hex.clone(), None),
            Reconfigurer::ReleasedStoredCosigner => {
                (commitment_hex.clone(), Some(chrono::Utc::now()))
            }
            Reconfigurer::NewSigner => (TestSigner::new().commitment_hex, None),
        };
        let mut network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)))
            .with_verify_commitment(Ok(chain));
        if let Reconfigurer::StoredCosignerChangingTheRules = reconfigurer {
            network_client = network_client
                .with_approval_rules(Ok(ApprovalRules {
                    binding: AuthBinding::default(),
                    thresholds: vec!["2".to_string()],
                }))
                .with_approval_rules(Ok(ApprovalRules {
                    binding: AuthBinding::default(),
                    thresholds: vec!["1".to_string()],
                }));
        }
        let mut storage_backend = MockStorageBackend::new()
            .with_submit_state(Ok(()))
            .with_pull_state(Ok(stored_state(account_id_hex, "0x1234")));
        if let Reconfigurer::StoredCosignerRacingAPromotion = reconfigurer {
            storage_backend = storage_backend.with_pull_state_head(head("0x9999", 2));
        }
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(AccountMetadata {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![stored_cosigner],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at,
            })))
            .with_set(Ok(()));
        let state = create_test_app_state(
            network_client.clone(),
            storage_backend.clone(),
            metadata_store.clone(),
        )
        .await;
        let initial_state: serde_json::Value =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let result = configure_account(
            &state,
            ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![commitment_hex],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state,
                credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
            },
        )
        .await;
        (result, network_client, storage_backend, metadata_store)
    }

    /// A key the stored auth does not list reconfigures from the chain's
    /// state only.
    #[tokio::test]
    async fn configure_by_a_new_signer_requires_the_on_chain_state() {
        for chain in [
            StateVerification::Mismatch {
                on_chain: "0x1234".to_string(),
            },
            StateVerification::Absent,
        ] {
            let (result, _, storage, _) = reconfigure_by(Reconfigurer::NewSigner, chain).await;
            assert!(
                matches!(&result, Err(error @ GuardianError::StateNotOnChain { .. }) if error.retryable()),
                "{result:?}"
            );
            assert!(storage.get_submit_state_calls().is_empty());
            assert!(storage.get_replace_state_calls().is_empty());
        }
    }

    /// A key the stored auth does not list, such as one an account added
    /// on another GUARDIAN, reconfigures with the on-chain state.
    #[tokio::test]
    async fn configure_by_a_new_signer_accepts_the_on_chain_state() {
        let (result, network, _, _) =
            reconfigure_by(Reconfigurer::NewSigner, StateVerification::Match).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            network.get_verify_commitment_calls(),
            vec![(
                "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d".to_string(),
                "0x5678".to_string()
            )]
        );
    }

    /// A released account's stored signers may have changed on the GUARDIAN
    /// it switched to, so even a stored cosigner reconfigures it from the
    /// chain's state only.
    #[tokio::test]
    async fn configure_of_a_released_account_requires_the_on_chain_state() {
        let (result, _, storage, _) = reconfigure_by(
            Reconfigurer::ReleasedStoredCosigner,
            StateVerification::Mismatch {
                on_chain: "0x1234".to_string(),
            },
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::StateNotOnChain { .. })),
            "{result:?}"
        );
        assert!(storage.get_submit_state_calls().is_empty());
        assert!(storage.get_replace_state_calls().is_empty());

        let (result, network, _, _) = reconfigure_by(
            Reconfigurer::ReleasedStoredCosigner,
            StateVerification::Match,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(network.get_verify_commitment_calls().len(), 1);
    }

    /// A stored cosigner may not change who approves or how many approvals
    /// a transaction needs with a state the chain does not hold: a single
    /// cosigner could otherwise register a signer set of only itself.
    #[tokio::test]
    async fn configure_by_a_stored_cosigner_changing_the_rules_requires_the_on_chain_state() {
        let (result, _, storage, _) = reconfigure_by(
            Reconfigurer::StoredCosignerChangingTheRules,
            StateVerification::Mismatch {
                on_chain: "0x1234".to_string(),
            },
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::StateNotOnChain { .. })),
            "{result:?}"
        );
        assert!(storage.get_submit_state_calls().is_empty());
        assert!(storage.get_replace_state_calls().is_empty());

        let (result, network, _, _) = reconfigure_by(
            Reconfigurer::StoredCosignerChangingTheRules,
            StateVerification::Match,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(network.get_verify_commitment_calls().len(), 1);
    }

    /// The stored rules vouch only for the state they were read from. A state
    /// promoted between that read and the write, such as a landed removal of
    /// the requesting cosigner, is not replaced under the older rules.
    #[tokio::test]
    async fn configure_refuses_to_replace_a_state_promoted_after_the_rules_were_compared() {
        let (result, network, storage, _) = reconfigure_by(
            Reconfigurer::StoredCosignerRacingAPromotion,
            StateVerification::Match,
        )
        .await;
        assert!(
            matches!(
                &result,
                Err(GuardianError::CommitmentMismatch { expected, actual })
                    if expected == "0x9999" && actual == "0x5678"
            ),
            "{result:?}"
        );
        assert!(network.get_verify_commitment_calls().is_empty());
        assert!(storage.get_submit_state_calls().is_empty());
        assert!(storage.get_replace_state_calls().is_empty());
    }

    /// A state the chain has already moved past is refused as a mismatch,
    /// not as a state still on its way: waiting does not help.
    #[tokio::test]
    async fn configure_refuses_a_state_behind_the_chain_as_a_mismatch() {
        let result = reconfigure_with_a_history_hit(StateVerification::Mismatch {
            on_chain: "0x9999".to_string(),
        })
        .await;
        assert!(
            matches!(
                &result,
                Err(GuardianError::CommitmentMismatch { expected, actual })
                    if expected == "0x9999" && actual == "0x5678"
            ),
            "{result:?}"
        );
    }

    /// The transaction ending at the submitted state can land between the
    /// head read and the history search, which then finds it: the state is
    /// the head by then, so the reconfiguration is accepted.
    #[tokio::test]
    async fn configure_accepts_a_state_that_landed_during_the_history_search() {
        let result = reconfigure_with_a_history_hit(StateVerification::Match).await;
        assert!(result.is_ok(), "{result:?}");
    }

    /// A key the stored auth does not list reconfigures with a state the
    /// first head read does not find but the history search does; the
    /// second head read answers `second_read`.
    async fn reconfigure_with_a_history_hit(
        second_read: StateVerification,
    ) -> Result<ConfigureAccountResult> {
        use crate::testing::helpers::{TestSigner, generate_falcon_signature};

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)))
            .with_verify_commitment(Ok(second_read))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0x9999".to_string(),
            }))
            .with_find_transaction_ending_at(Ok(crate::network::TransactionSearch {
                found_in_block: Some(12),
                resume_from_block: 13,
            }));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(configured(
                account_id_hex,
                Auth::MidenFalconRpo {
                    cosigner_commitments: vec![TestSigner::new().commitment_hex],
                },
            ))))
            .with_set(Ok(()));
        let state = create_test_app_state(
            network_client,
            MockStorageBackend::new().with_submit_state(Ok(())),
            metadata_store,
        )
        .await;
        configure_account(
            &state,
            ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![commitment_hex],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state: serde_json::from_str(include_str!(
                    "../testing/fixtures/account.json"
                ))
                .unwrap(),
                credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
            },
        )
        .await
    }

    /// A stored cosigner re-onboards under the stored approval rules as
    /// before, from a state ahead of the chain included, without a chain
    /// read.
    #[tokio::test]
    async fn configure_lets_a_stored_cosigner_reconfigure_without_a_chain_read() {
        let (result, network, _, _) = reconfigure_by(
            Reconfigurer::StoredCosigner,
            StateVerification::Mismatch {
                on_chain: "0x1234".to_string(),
            },
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert!(network.get_verify_commitment_calls().is_empty());
    }

    #[tokio::test]
    async fn configure_rejects_replayed_reconfiguration_before_writes() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let existing = AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(existing)))
            .with_update_timestamp_cas(Ok(false));
        let observed = metadata.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            MockStorageBackend::new(),
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationReplay)
        ));
        assert_eq!(
            observed.get_update_timestamp_cas_calls(),
            vec![(account_id.to_string(), commitment, timestamp)]
        );
        assert!(observed.get_set_calls().is_empty());
    }

    /// A duplicate of a first configuration (a client retry or a replay)
    /// that loses the claim to the configuration it duplicates is judged as
    /// a reconfiguration of the account it lost to: its timestamp, already
    /// consumed by the winner, is refused before it writes anything, so it
    /// cannot store the initial state over the state the account has moved
    /// to since.
    #[tokio::test]
    async fn duplicate_first_configuration_is_refused_before_writes() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let winner = AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            has_pending_candidate: true,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        };
        // Reads pop LIFO: the first read sees no account, the re-read after
        // the lost claim sees the winner's.
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(winner)))
            .with_get(Ok(None))
            .with_create(Ok(false))
            .with_update_timestamp_cas(Ok(false));
        let observed_metadata = metadata.clone();
        let storage = MockStorageBackend::new();
        let observed_storage = storage.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            storage,
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationReplay)
        ));
        assert_eq!(observed_metadata.get_create_calls().len(), 1);
        assert_eq!(
            observed_metadata.get_update_timestamp_cas_calls(),
            vec![(account_id.to_string(), commitment, timestamp)]
        );
        assert!(observed_storage.get_submit_state_calls().is_empty());
        assert!(observed_metadata.get_set_calls().is_empty());
    }

    /// The configuration that wins the claim consumes its timestamp before
    /// it stores the initial state, and creates the account's metadata
    /// instead of writing it with the generic upsert.
    #[tokio::test]
    async fn first_configuration_claims_then_consumes_before_storing_state() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let metadata = MockMetadataStore::new()
            .with_get(Ok(None))
            .with_create(Ok(true))
            .with_update_timestamp_cas(Ok(false));
        let observed_metadata = metadata.clone();
        let storage = MockStorageBackend::new();
        let observed_storage = storage.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            storage,
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        assert!(matches!(
            configure_account(&state, params).await,
            Err(GuardianError::AuthenticationReplay)
        ));
        let created = observed_metadata.get_create_calls();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].account_id, account_id);
        assert!(!created[0].has_pending_candidate);
        assert!(
            observed_storage.get_submit_state_calls().is_empty(),
            "a refused timestamp must stop the first configuration before its state write"
        );
        assert!(observed_metadata.get_set_calls().is_empty());
    }

    fn claimed_metadata(account_id: &str, commitment: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment.to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
            has_pending_candidate: true,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    /// The retry that finishes an unfinished first configuration is the
    /// account's creation, so it is counted as one.
    #[tokio::test]
    async fn retry_of_an_unfinished_first_configuration_counts_as_a_creation() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            MockStorageBackend::new()
                .with_pull_state(Err("Failed to pull state: Record not found".to_string())),
            MockMetadataStore::new().with_get(Ok(Some(claimed_metadata(account_id, &commitment)))),
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        let recorder = crate::metrics::recorder::build_recorder();
        let handle = recorder.handle();
        let result = {
            let _guard = metrics::set_default_local_recorder(&recorder);
            configure_account(&state, params).await
        };

        assert!(
            result.is_ok(),
            "the retry must finish the account: {result:?}"
        );
        let rendered = handle.render();
        assert!(
            rendered.contains(&format!(
                "{}{{kind=\"miden\"}} 1",
                crate::metrics::names::ACCOUNTS_CREATED_TOTAL
            )),
            "the finishing retry must count one creation:\n{rendered}"
        );
    }

    /// A first configuration that loses the claim to a different signer
    /// passes its own replay check and goes ahead as a reconfiguration of
    /// the account it lost to: it writes metadata over the winner's row,
    /// keeping the winner's creation time and in-flight candidate, rather
    /// than treating the account as new.
    #[tokio::test]
    async fn losing_first_configuration_from_another_signer_reconfigures_the_winners_account() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(account_id);
        let winner = AccountMetadata {
            created_at: "2023-06-01T00:00:00Z".into(),
            ..claimed_metadata(account_id, &commitment)
        };
        // Reads pop LIFO: the first read sees no account, the re-read after
        // the lost claim sees the winner's.
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(winner)))
            .with_get(Ok(None))
            .with_create(Ok(false))
            .with_update_timestamp_cas(Ok(true));
        let observed_metadata = metadata.clone();
        let storage = MockStorageBackend::new();
        let observed_storage = storage.clone();
        let state = create_test_app_state(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_get_state_head(Ok(head("0x1234", 3))),
            storage,
            metadata,
        )
        .await;
        let initial_state =
            serde_json::from_str(include_str!("../testing/fixtures/account.json")).unwrap();
        let params = ConfigureAccountParams {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential: Credentials::signature(pubkey, signature, timestamp),
        };

        let result = configure_account(&state, params).await;
        assert!(result.is_ok(), "{result:?}");

        let stored = observed_storage.get_submit_state_calls();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].created_at, "2023-06-01T00:00:00Z");
        let set = observed_metadata.get_set_calls();
        assert_eq!(set.len(), 1, "the loser reconfigures the winner's row");
        assert_eq!(set[0].created_at, "2023-06-01T00:00:00Z");
        assert!(set[0].has_pending_candidate);
    }

    /// Regression: reconfiguring a paused account must NOT clear
    /// `paused_at` / `paused_reason`. Pause state can only be
    /// transitioned by `set_pause`/`clear_pause` (FR-019).
    #[tokio::test]
    async fn test_configure_account_preserves_existing_pause_state() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        use chrono::TimeZone;
        let paused_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
            .unwrap();
        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: Some(paused_at),
            paused_reason: Some("compliance".to_string()),
            released_at: None,
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state =
            create_test_app_state(network_client, storage_backend, metadata_store.clone()).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        configure_account(&state, params)
            .await
            .expect("Reconfiguration should succeed");

        let set_calls = metadata_store.get_set_calls();
        assert_eq!(set_calls.len(), 1);
        assert_eq!(set_calls[0].paused_at, Some(paused_at));
        assert_eq!(set_calls[0].paused_reason.as_deref(), Some("compliance"));
        assert!(
            metadata_store
                .clear_released_calls
                .lock()
                .unwrap()
                .is_empty(),
            "only a state the chain vouched for clears a release"
        );
    }

    /// The release sweep released the account between this request's first
    /// metadata read, which saw it active, and its state write. A stored
    /// cosigner's state was vouched for by the stored rules, not the chain,
    /// so the release is undone only when the submitted state proves to be
    /// the account's on-chain state; otherwise it stands and the request is
    /// refused.
    #[tokio::test]
    async fn configure_without_a_chain_read_checks_a_release_written_after_its_first_read() {
        for (chain, reactivated) in [
            (
                StateVerification::Mismatch {
                    on_chain: "0x1234".to_string(),
                },
                false,
            ),
            (StateVerification::Match, true),
        ] {
            let (result, metadata) = reconfigure_released_mid_request(chain).await;
            let clears = metadata.clear_released_calls.lock().unwrap().clone();
            if reactivated {
                assert!(result.is_ok(), "{result:?}");
                assert_eq!(
                    clears,
                    vec![(
                        "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d".to_string(),
                        "0x5678".to_string()
                    )]
                );
            } else {
                assert!(
                    matches!(&result, Err(error @ GuardianError::StateNotOnChain { .. }) if error.retryable()),
                    "{result:?}"
                );
                assert!(clears.is_empty());
            }
        }
    }

    /// A stored cosigner reconfigures an active account that the release
    /// sweep releases before the state write; the chain answers `chain` for
    /// the submitted state.
    async fn reconfigure_released_mid_request(
        chain: StateVerification,
    ) -> (Result<ConfigureAccountResult>, MockMetadataStore) {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let existing_metadata = configured(
            account_id_hex,
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
        );
        let released_since = AccountMetadata {
            released_at: Some(chrono::Utc::now()),
            ..existing_metadata.clone()
        };
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)))
            .with_verify_commitment(Ok(chain));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(released_since)))
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()))
            .with_clear_released(Ok(ClearTransition::Cleared));
        let state = create_test_app_state(
            network_client,
            MockStorageBackend::new().with_submit_state(Ok(())),
            metadata_store.clone(),
        )
        .await;
        let result = configure_account(
            &state,
            ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![commitment_hex],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state: serde_json::from_str(include_str!(
                    "../testing/fixtures/account.json"
                ))
                .unwrap(),
                credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
            },
        )
        .await;
        (result, metadata_store)
    }

    /// A state the chain vouched for still undoes a release written between
    /// the request's first metadata read and its state write.
    #[tokio::test]
    async fn configure_from_the_on_chain_state_clears_a_release_written_after_its_first_read() {
        let (result, _, _, metadata) =
            reconfigure_by(Reconfigurer::NewSigner, StateVerification::Match).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            metadata.clear_released_calls.lock().unwrap().clone(),
            vec![(
                "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d".to_string(),
                "0x5678".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn test_configure_account_reonboarding_clears_released_state() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        use chrono::TimeZone;
        let released_at = chrono::Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: Some(released_at),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()));

        let state =
            create_test_app_state(network_client, storage_backend, metadata_store.clone()).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        configure_account(&state, params)
            .await
            .expect("Re-onboarding a released account should succeed");

        // Re-onboarding (with the guardian binding re-validated) is the
        // reactivation event: released state must be explicitly cleared.
        assert_eq!(
            metadata_store.clear_released_calls.lock().unwrap().clone(),
            vec![(account_id_hex.to_string(), "0x5678".to_string())],
            "re-onboarding must clear the released state"
        );
    }

    fn released_account(account_id: &str, cosigner: &str) -> AccountMetadata {
        use chrono::TimeZone;
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![cosigner.to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: Some(chrono::Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap()),
        }
    }

    fn stored_state(account_id: &str, commitment: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            state_json: serde_json::json!({}),
            commitment: commitment.to_string(),
            nonce: Some(2),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            auth_scheme: "falcon".to_string(),
        }
    }

    #[tokio::test]
    async fn reonboarding_replaces_the_stored_state_it_read() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend =
            MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id_hex, "0x1234")));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(released_account(account_id_hex, &commitment_hex))))
            .with_set(Ok(()));

        let state = create_test_app_state(
            network_client,
            storage_backend.clone(),
            metadata_store.clone(),
        )
        .await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::from_str(account_json).unwrap(),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        configure_account(&state, params)
            .await
            .expect("re-onboarding over an unchanged stored state succeeds");

        let replaced = storage_backend.get_replace_state_calls();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].0.commitment, "0x5678");
        assert_eq!(replaced[0].1, "0x1234");
        assert_eq!(
            metadata_store.clear_released_calls.lock().unwrap().clone(),
            vec![(account_id_hex.to_string(), "0x5678".to_string())]
        );
    }

    #[tokio::test]
    async fn reonboarding_never_overwrites_a_state_promoted_after_its_read() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state(account_id_hex, "0x1234")))
            .with_replace_state(Ok(crate::storage::StateReplace::StateMoved {
                current: "0x9abc".to_string(),
            }));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(released_account(account_id_hex, &commitment_hex))))
            .with_set(Ok(()));

        let state = create_test_app_state(
            network_client,
            storage_backend.clone(),
            metadata_store.clone(),
        )
        .await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::from_str(account_json).unwrap(),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        match configure_account(&state, params).await {
            Err(GuardianError::CommitmentMismatch { expected, actual }) => {
                assert_eq!(expected, "0x9abc");
                assert_eq!(actual, "0x5678");
            }
            other => panic!("expected CommitmentMismatch, got {other:?}"),
        }
        assert!(storage_backend.get_submit_state_calls().is_empty());
        assert!(
            metadata_store
                .clear_released_calls
                .lock()
                .unwrap()
                .is_empty(),
            "a refused re-onboarding must not clear the release"
        );
    }

    async fn reonboard_with_older_state(
        network_client: MockNetworkClient,
    ) -> (Result<ConfigureAccountResult>, MockStorageBackend) {
        let (result, storage, _) = reonboard_with(
            network_client,
            MockStorageBackend::new().with_pull_state(Ok(stored_state(
                "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d",
                "0x1234",
            ))),
            true,
            Vec::new(),
        )
        .await;
        (result, storage)
    }

    /// Re-onboard an existing, released account with a state at nonce 1
    /// (`0x0101`); `fresh` is whether the replay floor accepts the request.
    /// `stored_decodes` answer the stored-state decodes, which run after
    /// the submitted state's.
    async fn reonboard_with(
        network_client: MockNetworkClient,
        storage_backend: MockStorageBackend,
        fresh: bool,
        stored_decodes: Vec<std::result::Result<StateHead, String>>,
    ) -> (
        Result<ConfigureAccountResult>,
        MockStorageBackend,
        MockNetworkClient,
    ) {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = stored_decodes
            .into_iter()
            .rev()
            .fold(network_client, |client, decoded| {
                client.with_get_state_head(decoded)
            })
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x0101", 1)))
            .with_verify_commitment(Ok(StateVerification::Match));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(released_account(account_id_hex, &commitment_hex))))
            .with_set(Ok(()))
            .with_update_timestamp_cas(Ok(fresh));

        let state = create_test_app_state(
            network_client.clone(),
            storage_backend.clone(),
            metadata_store,
        )
        .await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::from_str(account_json).unwrap(),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };
        (
            configure_account(&state, params).await,
            storage_backend,
            network_client,
        )
    }

    fn assert_refused_as_stale(
        result: Result<ConfigureAccountResult>,
        storage: &MockStorageBackend,
    ) {
        match result {
            Err(GuardianError::CommitmentMismatch { expected, actual }) => {
                assert_eq!(expected, "0x1234");
                assert_eq!(actual, "0x0101");
            }
            other => panic!("expected CommitmentMismatch, got {other:?}"),
        }
        assert!(storage.get_replace_state_calls().is_empty());
        assert!(storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn a_stored_row_without_a_nonce_is_decoded_for_the_stale_check() {
        // A row written before the server stored nonces: the stale-state
        // check decodes the stored state instead of skipping it.
        let mut legacy = stored_state("0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d", "0x1234");
        legacy.nonce = None;
        let (result, storage, network) = reonboard_with(
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match)),
            MockStorageBackend::new()
                .with_pull_state(Ok(legacy.clone()))
                .with_pull_state(Ok(legacy)),
            true,
            vec![Ok(head("0x1234", 2))],
        )
        .await;
        assert_refused_as_stale(result, &storage);
        assert_eq!(network.get_state_head_calls().len(), 2);
    }

    #[tokio::test]
    async fn an_unauthenticated_request_never_reads_the_stored_state() {
        // A nonce-less stored row needs a full read and a decode; a request
        // whose credential does not validate must not reach either.
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let mut legacy = stored_state(account_id_hex, "0x1234");
        legacy.nonce = None;
        let storage_backend = MockStorageBackend::new().with_pull_state(Ok(legacy));
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Err("credential does not validate".to_string()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(released_account(account_id_hex, &commitment_hex))));
        let state = create_test_app_state(
            network_client.clone(),
            storage_backend.clone(),
            metadata_store,
        )
        .await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::from_str(account_json).unwrap(),
            credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
        };

        assert!(configure_account(&state, params).await.is_err());
        assert_eq!(
            storage_backend.pending_pull_state_responses(),
            1,
            "the stored state is never read"
        );
        assert!(network_client.get_state_head_calls().is_empty());
    }

    #[tokio::test]
    async fn a_replayed_stale_request_is_refused_before_any_chain_read() {
        let (result, storage, network) = reonboard_with(
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match)),
            MockStorageBackend::new().with_pull_state(Ok(stored_state(
                "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d",
                "0x1234",
            ))),
            false,
            Vec::new(),
        )
        .await;
        assert!(matches!(result, Err(GuardianError::AuthenticationReplay)));
        assert!(network.get_verify_commitment_calls().is_empty());
        assert!(network.get_find_transaction_ending_at_calls().is_empty());
        assert!(storage.get_replace_state_calls().is_empty());
    }

    #[tokio::test]
    async fn an_older_state_is_refused_when_the_history_cannot_be_searched() {
        // The head check succeeds but shows the chain elsewhere; whether
        // the stored state landed earlier needs the history, which fails.
        let (result, storage) = reonboard_with_older_state(
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "0x9999".to_string(),
                }))
                .with_find_transaction_ending_at(Err("history unavailable".to_string())),
        )
        .await;
        match result {
            Err(e @ GuardianError::RpcUnavailable(_)) => assert!(e.retryable()),
            other => panic!("expected RpcUnavailable, got {other:?}"),
        }
        assert!(storage.get_replace_state_calls().is_empty());
        assert!(storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn a_stored_state_that_does_not_decode_can_still_be_replaced() {
        // Intended recovery: a corrupt stored row has no knowable nonce, so
        // the stale-state check cannot apply and re-onboarding replaces it
        // (still only over the commitment it read).
        let mut corrupt = stored_state("0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d", "0x1234");
        corrupt.nonce = None;
        let (result, storage, network) = reonboard_with(
            MockNetworkClient::new(),
            MockStorageBackend::new()
                .with_pull_state(Ok(corrupt.clone()))
                .with_pull_state(Ok(corrupt)),
            true,
            vec![Err("stored state does not decode".to_string())],
        )
        .await;
        result.expect("re-onboarding replaces a stored state that does not decode");
        assert_eq!(
            network.get_verify_commitment_calls(),
            vec![(
                "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d".to_string(),
                "0x0101".to_string()
            )],
            "only the released account's submitted state is checked against the chain"
        );
        let replaced = storage.get_replace_state_calls();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].1, "0x1234");
    }

    #[tokio::test]
    async fn a_legacy_row_is_judged_by_the_state_its_full_read_returns() {
        // The head read of a nonce-less row saw `0x5555`; a promotion moved
        // the row to `0x1234` before the full read. Nonce and commitment
        // both come from the full read, so the stale check and the replace
        // judge the same state.
        let mut promoted = stored_state("0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d", "0x1234");
        promoted.nonce = None;
        let (result, storage, network) = reonboard_with(
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match)),
            MockStorageBackend::new()
                .with_pull_state(Ok(promoted))
                .with_pull_state_head(StateHead {
                    commitment: "0x5555".to_string(),
                    nonce: None,
                }),
            true,
            vec![Ok(head("0x1234", 2))],
        )
        .await;
        assert_refused_as_stale(result, &storage);
        assert_eq!(
            network.get_verify_commitment_calls()[1].1,
            "0x1234",
            "after the released account's submitted state, the chain is asked about the state the full read returned"
        );
    }

    #[tokio::test]
    async fn an_older_state_never_replaces_a_stored_state_the_chain_holds() {
        let (result, storage) = reonboard_with_older_state(
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match)),
        )
        .await;
        assert_refused_as_stale(result, &storage);
    }

    #[tokio::test]
    async fn an_older_state_never_replaces_a_stored_state_the_chain_moved_past() {
        let (result, storage) = reonboard_with_older_state(
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "0x9999".to_string(),
                }))
                .with_find_transaction_ending_at(Ok(crate::network::TransactionSearch {
                    found_in_block: Some(40),
                    resume_from_block: 41,
                })),
        )
        .await;
        assert_refused_as_stale(result, &storage);
    }

    #[tokio::test]
    async fn an_older_state_replaces_a_stored_state_that_never_landed() {
        // Optimistic mode: the stored state is ahead of the chain and never
        // landed, so re-onboarding with the chain's older state recovers it.
        let (result, storage) = reonboard_with_older_state(
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "0x0101".to_string(),
                }))
                .with_find_transaction_ending_at(Ok(crate::network::TransactionSearch {
                    found_in_block: None,
                    resume_from_block: 41,
                })),
        )
        .await;
        result.expect("re-onboarding over a state that never landed succeeds");
        let replaced = storage.get_replace_state_calls();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].0.commitment, "0x0101");
    }

    #[tokio::test]
    async fn an_older_state_is_refused_when_the_chain_cannot_be_read() {
        let (result, storage) = reonboard_with_older_state(
            MockNetworkClient::new().with_verify_commitment(Err("node unreachable".to_string())),
        )
        .await;
        match result {
            Err(e @ GuardianError::RpcUnavailable(_)) => assert!(e.retryable()),
            other => panic!("expected RpcUnavailable, got {other:?}"),
        }
        assert!(storage.get_replace_state_calls().is_empty());
        assert!(storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_configure_account_reonboarding_fails_closed_when_clear_released_fails() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        use chrono::TimeZone;
        let released_at = chrono::Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        let existing_metadata = AccountMetadata {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: Some(released_at),
        };

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Ok(head("0x5678", 3)));
        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new()
            .with_get(Ok(Some(existing_metadata)))
            .with_set(Ok(()))
            .with_clear_released(Err("disk on fire".to_string()));

        let state =
            create_test_app_state(network_client, storage_backend, metadata_store.clone()).await;

        let account_json = include_str!("../testing/fixtures/account.json");
        let initial_state: serde_json::Value = serde_json::from_str(account_json).unwrap();
        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);
        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state,
            credential,
        };

        // Fail-closed: the account stays released (mutations still refused)
        // and the caller sees the storage failure so the wallet retries
        // the re-onboarding.
        let err = configure_account(&state, params)
            .await
            .expect_err("clear_released failure must surface");
        assert!(
            matches!(err, GuardianError::StorageError(_)),
            "expected StorageError, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_configure_account_network_error() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_get_state_head(Err("Network connection failed".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::NetworkError(_) => {}
            e => panic!("Expected NetworkError, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_configure_account_unauthorized_guardian_commitment() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_validate_guardian_commitment(Err(
                "Miden Standards slot 'miden::standards::auth::guardian::pub_key' mismatch"
                    .to_string(),
            ));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex.clone(), signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthorizationFailed(msg) => {
                assert!(msg.contains("Unauthorized account configuration"));
                assert!(msg.contains("miden::standards::auth::guardian::pub_key"));
            }
            e => panic!("Expected AuthorizationFailed, got: {:?}", e),
        }

        assert!(
            storage_backend.get_submit_state_calls().is_empty(),
            "state should not be persisted on unauthorized configuration"
        );
    }

    /// A multisig approver without a supported scheme in the state is
    /// refused before the account is claimed or anything is written: the
    /// chain could never verify that approver's signatures.
    #[tokio::test]
    async fn test_configure_account_rejects_a_missing_approver_scheme() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_validate_multisig_auth(Err("Missing or unsupported entry 1 in slot \
                 'miden::standards::auth::multisig::approver_schemes' for approver 0x01"
                .to_string()));
        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));
        let state = create_test_app_state(
            network_client,
            storage_backend.clone(),
            metadata_store.clone(),
        )
        .await;

        let result = configure_account(
            &state,
            ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![commitment_hex],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state: serde_json::json!({"balance": 100}),
                credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
            },
        )
        .await;

        match result {
            Err(GuardianError::InvalidInput(message)) => {
                assert!(message.starts_with("Invalid multisig auth in initial_state"));
                assert!(message.contains("approver_schemes"), "{message}");
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
        assert!(metadata_store.get_create_calls().is_empty());
        assert!(storage_backend.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_configure_account_rejects_mismatched_cosigner_commitments() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, _commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!("0x{}", "aa".repeat(32))],
            })));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!("0x{}", "bb".repeat(32))],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        match result.unwrap_err() {
            GuardianError::InvalidInput(msg) => {
                assert!(msg.contains("cosigner_commitments"), "got: {msg}");
            }
            e => panic!("Expected InvalidInput, got: {:?}", e),
        }

        assert!(
            storage_backend.get_submit_state_calls().is_empty(),
            "state should not be persisted on mismatched cosigner commitments"
        );
    }

    /// The declared list must match the state's signer set exactly:
    /// appending an extra (non-signer) commitment to an otherwise
    /// correct list is rejected — the injected key would otherwise be
    /// authorized for every later request against this account.
    #[tokio::test]
    async fn test_configure_account_rejects_injected_extra_cosigner() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone()],
            })));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex, "0xinjected_commitment".to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(
            matches!(result, Err(GuardianError::InvalidInput(_))),
            "expected InvalidInput, got: {result:?}"
        );
        assert!(
            storage_backend.get_submit_state_calls().is_empty(),
            "state should not be persisted when an extra cosigner is injected"
        );
    }

    #[tokio::test]
    async fn test_configure_account_cosigner_extraction_error() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Err("failed to deserialize account".to_string()));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(
            matches!(result, Err(GuardianError::NetworkError(_))),
            "expected NetworkError, got: {result:?}"
        );
    }

    /// The comparison is deliberately order-sensitive: the signer map's
    /// index order is the canonical order, and the SDK emits it verbatim.
    #[tokio::test]
    async fn test_rejects_reordered_commitments() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);
        let other_hex = format!("0x{}", "aa".repeat(32));

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex.clone(), other_hex.clone()],
            })));

        let storage_backend = MockStorageBackend::new();
        let metadata_store = MockMetadataStore::new().with_get(Ok(None));

        let state =
            create_test_app_state(network_client, storage_backend.clone(), metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![other_hex, commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(
            matches!(result, Err(GuardianError::InvalidInput(_))),
            "same set in a different order must be rejected, got: {result:?}"
        );
        assert!(storage_backend.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_rejects_non_canonical_empty_and_duplicate_commitment_lists() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let canonical = format!("0x{}", "ab".repeat(32));

        let cases: Vec<(Vec<String>, &str)> = vec![
            (vec![], "empty list"),
            (
                vec![canonical.clone(), canonical.clone()],
                "duplicate entry",
            ),
            (vec![format!("0x{}", "AB".repeat(32))], "uppercase hex"),
            (vec!["ab".repeat(32)], "missing 0x prefix"),
            (vec![format!("0x{}", "ab".repeat(31))], "wrong length"),
            (vec!["0xnot_hex".to_string()], "non-hex characters"),
        ];

        for (list, label) in cases {
            let (pubkey_hex, _, signature_hex, timestamp) =
                generate_falcon_signature(account_id_hex);
            let state = create_test_app_state(
                MockNetworkClient::new(),
                MockStorageBackend::new(),
                MockMetadataStore::new(),
            )
            .await;

            let params = ConfigureAccountParams {
                account_id: account_id_hex.to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: list,
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                initial_state: serde_json::json!({"balance": 100}),
                credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
            };

            let result = configure_account(&state, params).await;
            assert!(
                matches!(result, Err(GuardianError::InvalidInput(_))),
                "{label}: expected InvalidInput, got: {result:?}"
            );
        }
    }

    /// `should_update_auth` returning `None` means the state carries no
    /// extractable signer set; the declared list is then accepted as-is,
    /// matching canonicalization semantics. A real guarded multisig, which
    /// `validate_multisig_auth` requires, always carries one.
    #[tokio::test]
    async fn test_configure_account_skips_validation_without_extractable_signers() {
        use crate::testing::helpers::generate_falcon_signature;

        let account_id_hex = "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d";
        let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
            generate_falcon_signature(account_id_hex);

        let network_client = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_should_update_auth(Ok(None))
            .with_get_state_head(Ok(head("0x1234", 3)));

        let storage_backend = MockStorageBackend::new().with_submit_state(Ok(()));
        let metadata_store = MockMetadataStore::new().with_get(Ok(None)).with_set(Ok(()));

        let state = create_test_app_state(network_client, storage_backend, metadata_store).await;

        let credential = Credentials::signature(pubkey_hex, signature_hex, timestamp);

        let params = ConfigureAccountParams {
            account_id: account_id_hex.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment_hex],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            initial_state: serde_json::json!({"balance": 100}),
            credential,
        };

        let result = configure_account(&state, params).await;

        assert!(result.is_ok(), "got: {result:?}");
    }
}
