use chrono::Duration;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::evm::proposal::{
    EVM_PROPOSAL_KIND, EvmProposal, EvmProposalSignature, ExecutableEvmProposal,
    NormalizedEvmProposalInput, compare_u256_decimal, normalize_hash, normalize_proposal_id,
    normalize_signature,
};
use crate::metadata::AccountMetadata;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::Auth;
use crate::metadata::network::{evm_account_id, normalize_evm_address};
use crate::state::AppState;

#[derive(Clone, Debug)]
pub struct RegisterEvmAccountParams {
    pub chain_id: u64,
    pub account_address: String,
    pub multisig_validator_address: String,
    pub session_address: String,
}

#[derive(Clone, Debug)]
pub struct RegisterEvmAccountResult {
    pub account_id: String,
    pub chain_id: u64,
    pub account_address: String,
    pub multisig_validator_address: String,
    pub signers: Vec<String>,
    pub threshold: usize,
}

#[derive(Clone, Debug)]
pub struct CreateEvmProposalParams {
    pub account_id: String,
    pub user_op_hash: String,
    pub payload: String,
    pub nonce: String,
    pub ttl_seconds: u64,
    pub signature: String,
    pub session_address: String,
}

#[derive(Clone, Debug)]
pub struct ApproveEvmProposalParams {
    pub account_id: String,
    pub proposal_id: String,
    pub signature: String,
    pub session_address: String,
}

pub async fn register_account(
    state: &AppState,
    params: RegisterEvmAccountParams,
) -> Result<RegisterEvmAccountResult> {
    let account_address =
        normalize_evm_address(&params.account_address).map_err(GuardianError::InvalidInput)?;
    let multisig_validator_address = normalize_evm_address(&params.multisig_validator_address)
        .map_err(GuardianError::InvalidInput)?;
    let account_id = evm_account_id(params.chain_id, &account_address);
    let network_config = NetworkConfig::Evm {
        chain_id: params.chain_id,
        account_address: account_address.clone(),
        multisig_validator_address: multisig_validator_address.clone(),
    }
    .validate_for_account(&account_id)
    .map_err(GuardianError::InvalidNetworkConfig)?;
    let NetworkConfig::Evm {
        chain_id,
        account_address,
        multisig_validator_address,
    } = network_config.clone()
    else {
        return Err(GuardianError::InvalidNetworkConfig(
            "expected EVM network config".to_string(),
        ));
    };
    let session_address = normalize_session_address(&params.session_address)?;
    let chain = state
        .evm
        .chains
        .get(chain_id)
        .cloned()
        .ok_or(GuardianError::UnsupportedEvmChain { chain_id })?;
    let contracts = crate::evm::contracts::EvmContractReader::new(chain);
    contracts
        .ensure_validator_installed(&account_address, &multisig_validator_address)
        .await?;
    let snapshot = contracts
        .signer_snapshot(&account_address, &multisig_validator_address)
        .await?;
    if !snapshot
        .signers
        .iter()
        .any(|signer| signer.eq_ignore_ascii_case(&session_address))
    {
        return Err(GuardianError::SignerNotAuthorized(session_address));
    }

    let now = state.clock.now_rfc3339();
    let existing = state.metadata.get(&account_id).await.map_err(|e| {
        tracing::error!(
            account_id = %account_id,
            error = %e,
            "Failed to check existing EVM account"
        );
        GuardianError::StorageError(format!("Failed to check existing account: {e}"))
    })?;
    let created_at = existing
        .as_ref()
        .map(|m| m.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let signers = snapshot.signers;
    state
        .metadata
        .set(AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::EvmEcdsa {
                signers: signers.clone(),
            },
            network_config,
            created_at,
            updated_at: now,
            has_pending_candidate: false,
            last_auth_timestamp: existing.and_then(|m| m.last_auth_timestamp),
        })
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to store EVM metadata"
            );
            GuardianError::StorageError(format!("Failed to store metadata: {e}"))
        })?;

    Ok(RegisterEvmAccountResult {
        account_id,
        chain_id,
        account_address,
        multisig_validator_address,
        signers,
        threshold: snapshot.threshold,
    })
}

pub async fn create_proposal(
    state: &AppState,
    params: CreateEvmProposalParams,
) -> Result<EvmProposal> {
    let metadata = load_evm_metadata(state, &params.account_id).await?;
    let (chain_id, account_address, validator_address) =
        evm_network_parts(&metadata.network_config)?;
    let session_address = normalize_session_address(&params.session_address)?;

    let signature = EvmProposalSignature {
        signer: session_address.clone(),
        signature: normalize_signature(&params.signature)?,
        signed_at: state.clock.now().timestamp_millis(),
    };
    let input = NormalizedEvmProposalInput::new(
        chain_id,
        &account_address,
        &validator_address,
        &params.user_op_hash,
        params.payload,
        &params.nonce,
        &session_address,
        signature,
        params.ttl_seconds,
    )?;

    let chain = state.evm.chains.get(input.chain_id).cloned().ok_or(
        GuardianError::UnsupportedEvmChain {
            chain_id: input.chain_id,
        },
    )?;
    let contracts = crate::evm::contracts::EvmContractReader::new(chain);
    contracts
        .ensure_validator_installed(&input.smart_account_address, &input.validator_address)
        .await?;
    let snapshot = contracts
        .signer_snapshot(&input.smart_account_address, &input.validator_address)
        .await?;
    if !snapshot
        .signers
        .iter()
        .any(|signer| signer.eq_ignore_ascii_case(&input.proposer))
    {
        return Err(GuardianError::SignerNotAuthorized(input.proposer));
    }

    let recovered =
        crate::evm::contracts::verify_proposal_signature(&input, &input.signature.signature)?;
    if recovered != input.proposer {
        return Err(GuardianError::InvalidProposalSignature(
            "initial proposal signature signer does not match proposer".to_string(),
        ));
    }

    let proposal_id = crate::evm::contracts::compute_proposal_id(&input)?;
    if let Ok(existing_delta) = state
        .storage
        .pull_delta_proposal(&params.account_id, &proposal_id)
        .await
    {
        let existing = EvmProposal::from_delta(&proposal_id, &existing_delta)?;
        if proposal_is_inactive(state, &existing).await? {
            state
                .storage
                .delete_delta_proposal(&params.account_id, &proposal_id)
                .await
                .map_err(GuardianError::StorageError)?;
        } else {
            return Ok(existing);
        }
    }

    let now = state.clock.now();
    let proposal = EvmProposal {
        proposal_id: proposal_id.clone(),
        account_id: params.account_id.clone(),
        chain_id: input.chain_id,
        smart_account_address: input.smart_account_address,
        validator_address: input.validator_address,
        user_op_hash: input.hash,
        payload: input.payload,
        nonce: input.nonce.decimal,
        nonce_key: input.nonce.nonce_key,
        proposer: input.proposer,
        signer_snapshot: snapshot.signers,
        threshold: snapshot.threshold,
        signatures: vec![input.signature],
        created_at: now.timestamp_millis(),
        expires_at: (now + Duration::seconds(input.ttl_seconds as i64)).timestamp_millis(),
    };
    let delta = proposal.clone().into_delta();
    state
        .storage
        .submit_delta_proposal(&proposal_id, &delta)
        .await
        .map_err(GuardianError::StorageError)?;

    Ok(proposal)
}

pub async fn list_proposals(
    state: &AppState,
    account_id: &str,
    session_address: &str,
) -> Result<Vec<EvmProposal>> {
    load_evm_metadata(state, account_id).await?;
    let session_address = normalize_session_address(session_address)?;
    let mut active = Vec::new();
    for delta in state
        .storage
        .pull_all_delta_proposals(account_id)
        .await
        .map_err(GuardianError::StorageError)?
    {
        if !is_evm_delta(&delta) {
            continue;
        }
        let proposal = EvmProposal::from_stored_delta(&delta)?;
        if proposal_is_inactive(state, &proposal).await? {
            state
                .storage
                .delete_delta_proposal(account_id, &proposal.proposal_id)
                .await
                .map_err(GuardianError::StorageError)?;
            continue;
        }
        ensure_snapshot_signer(&proposal, &session_address)?;
        active.push(proposal);
    }
    active.sort_by_key(|proposal| proposal.created_at);
    Ok(active)
}

pub async fn get_proposal(
    state: &AppState,
    account_id: &str,
    commitment: &str,
    session_address: &str,
) -> Result<EvmProposal> {
    let proposal = load_active_proposal(state, account_id, commitment).await?;
    let session_address = normalize_session_address(session_address)?;
    ensure_snapshot_signer(&proposal, &session_address)?;
    Ok(proposal)
}

pub async fn approve_proposal(
    state: &AppState,
    params: ApproveEvmProposalParams,
) -> Result<EvmProposal> {
    let proposal_id = normalize_proposal_id(&params.proposal_id)?;
    let signer = normalize_session_address(&params.session_address)?;
    let mut proposal = load_active_proposal(state, &params.account_id, &proposal_id).await?;
    ensure_snapshot_signer(&proposal, &signer)?;
    if proposal.has_signature_from(&signer) {
        return Err(GuardianError::ProposalAlreadySigned { signer_id: signer });
    }

    let (_, hash_bytes) = normalize_hash(&proposal.user_op_hash)?;
    let signature = normalize_signature(&params.signature)?;
    let recovered = crate::evm::contracts::recover_hash_address(&hash_bytes, &signature)?;
    if recovered != signer {
        return Err(GuardianError::InvalidProposalSignature(
            "approval signature signer does not match signer".to_string(),
        ));
    }

    proposal.signatures.push(EvmProposalSignature {
        signer,
        signature,
        signed_at: state.clock.now().timestamp_millis(),
    });
    let delta = proposal.clone().into_delta();
    state
        .storage
        .update_delta_proposal(&proposal_id, &delta)
        .await
        .map_err(GuardianError::StorageError)?;
    Ok(proposal)
}

pub async fn executable_proposal(
    state: &AppState,
    account_id: &str,
    commitment: &str,
    session_address: &str,
) -> Result<ExecutableEvmProposal> {
    let proposal = get_proposal(state, account_id, commitment, session_address).await?;
    if !proposal.is_executable() {
        return Err(GuardianError::InsufficientSignatures {
            required: proposal.threshold,
            got: proposal.signature_count(),
        });
    }
    Ok(proposal.executable())
}

pub async fn cancel_proposal(
    state: &AppState,
    account_id: &str,
    commitment: &str,
    session_address: &str,
) -> Result<()> {
    let proposal_id = normalize_proposal_id(commitment)?;
    let session_address = normalize_session_address(session_address)?;
    let proposal = load_active_proposal(state, account_id, &proposal_id).await?;
    if !proposal.proposer.eq_ignore_ascii_case(&session_address) {
        return Err(GuardianError::AuthorizationFailed(
            "Only the proposal creator can cancel this EVM proposal".to_string(),
        ));
    }
    state
        .storage
        .delete_delta_proposal(account_id, &proposal_id)
        .await
        .map_err(GuardianError::StorageError)?;
    Ok(())
}

async fn load_evm_metadata(state: &AppState, account_id: &str) -> Result<AccountMetadata> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to check account: {e}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;
    if !metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "miden".to_string(),
            operation: "evm_proposal".to_string(),
        });
    }
    Ok(metadata)
}

async fn load_active_proposal(
    state: &AppState,
    account_id: &str,
    proposal_id: &str,
) -> Result<EvmProposal> {
    let proposal_id = normalize_proposal_id(proposal_id)?;
    let delta = state
        .storage
        .pull_delta_proposal(account_id, &proposal_id)
        .await
        .map_err(|_| GuardianError::ProposalNotFound {
            account_id: account_id.to_string(),
            commitment: proposal_id.clone(),
        })?;
    let proposal = EvmProposal::from_delta(&proposal_id, &delta)?;
    if proposal_is_inactive(state, &proposal).await? {
        state
            .storage
            .delete_delta_proposal(account_id, &proposal_id)
            .await
            .map_err(GuardianError::StorageError)?;
        return Err(GuardianError::ProposalNotFound {
            account_id: account_id.to_string(),
            commitment: proposal_id,
        });
    }
    Ok(proposal)
}

async fn proposal_is_inactive(state: &AppState, proposal: &EvmProposal) -> Result<bool> {
    if proposal.expires_at <= state.clock.now().timestamp_millis() {
        return Ok(true);
    }

    let chain = state.evm.chains.get(proposal.chain_id).cloned().ok_or(
        GuardianError::UnsupportedEvmChain {
            chain_id: proposal.chain_id,
        },
    )?;
    let contracts = crate::evm::contracts::EvmContractReader::new(chain);
    let current = contracts
        .entrypoint_nonce(&proposal.smart_account_address, &proposal.nonce_key)
        .await?;
    Ok(compare_u256_decimal(&proposal.nonce, &current)? == std::cmp::Ordering::Less)
}

fn evm_network_parts(network_config: &NetworkConfig) -> Result<(u64, String, String)> {
    match network_config {
        NetworkConfig::Evm {
            chain_id,
            account_address,
            multisig_validator_address,
        } => Ok((
            *chain_id,
            account_address.clone(),
            multisig_validator_address.clone(),
        )),
        NetworkConfig::Miden { .. } => Err(GuardianError::UnsupportedForNetwork {
            network: "miden".to_string(),
            operation: "evm_proposal".to_string(),
        }),
    }
}

fn normalize_session_address(address: &str) -> Result<String> {
    normalize_evm_address(address).map_err(GuardianError::InvalidInput)
}

fn ensure_snapshot_signer(proposal: &EvmProposal, signer: &str) -> Result<()> {
    if proposal.has_signer(signer) {
        Ok(())
    } else {
        Err(GuardianError::SignerNotAuthorized(signer.to_string()))
    }
}

fn is_evm_delta(delta: &DeltaObject) -> bool {
    delta
        .delta_payload
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == EVM_PROPOSAL_KIND)
}
