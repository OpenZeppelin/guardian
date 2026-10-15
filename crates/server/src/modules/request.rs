//! Assembling what a module receives: account views, projection and proposal context.

use chrono::{DateTime, Utc};
use guardian_module_sdk::prost_types::Timestamp;
use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{AccountView, HookRequest, RegistrationSubject, TransactionSubject};

use crate::delta_object::DeltaStatus;
use crate::error::GuardianError;
use crate::metadata::auth::Auth;
use crate::network::NetworkClient;
use crate::state::AppState;
use crate::storage::is_storage_not_found;

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalContext {
    pub proposer: String,
    pub metadata: serde_json::Value,
}

/// The stored proposal for this exact transaction, if any. Absence and storage failure are
/// distinct: a failure must not read as "no proposer".
pub async fn proposal_context(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    delta_payload: &serde_json::Value,
) -> Result<Option<ProposalContext>, GuardianError> {
    let Ok(proposal_id) = state
        .network_client
        .delta_proposal_id(account_id, nonce, delta_payload)
    else {
        return Ok(None);
    };
    match state
        .storage
        .pull_delta_proposal(account_id, &proposal_id)
        .await
    {
        Ok(proposal) => Ok(match proposal.status {
            DeltaStatus::Pending { proposer_id, .. } => Some(ProposalContext {
                proposer: proposer_id,
                metadata: proposal
                    .delta_payload
                    .get("metadata")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            }),
            _ => None,
        }),
        Err(error) if is_storage_not_found(&error) => Ok(None),
        Err(error) => Err(GuardianError::StorageError(format!(
            "proposal lookup failed: {error}"
        ))),
    }
}

fn scheme_name(scheme: &guardian_shared::SignatureScheme) -> &'static str {
    match scheme {
        guardian_shared::SignatureScheme::Falcon => "falcon",
        guardian_shared::SignatureScheme::Ecdsa => "ecdsa",
    }
}

fn timestamp(at: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: at.timestamp(),
        nanos: at.timestamp_subsec_nanos() as i32,
    }
}

/// The account's view with the scheme from its verified auth configuration. A single-key
/// account (no `default_threshold`) takes its one signer from that configuration too.
fn view(
    network: &dyn NetworkClient,
    state_json: &serde_json::Value,
    auth: &Auth,
) -> Result<AccountView, String> {
    let mut view = network.module_account_view(state_json)?;
    view.auth_scheme = scheme_name(&auth.scheme()).to_string();
    if view.default_threshold.is_none() {
        match auth.cosigner_commitments() {
            [signer] => view.signer_commitments = vec![signer.clone()],
            signers => {
                return Err(format!(
                    "single-key account registered with {} signers",
                    signers.len()
                ));
            }
        }
    }
    Ok(view)
}

pub struct TransactionInput<'a> {
    pub account_id: &'a str,
    pub submitter: &'a str,
    pub auth: &'a Auth,
    pub before_state: &'a serde_json::Value,
    pub after_state: &'a serde_json::Value,
    pub tx_summary: &'a serde_json::Value,
    pub proposal: Option<ProposalContext>,
    pub accounting_time: DateTime<Utc>,
}

pub fn transaction_request(
    network: &dyn NetworkClient,
    input: TransactionInput<'_>,
) -> Result<HookRequest, String> {
    let transaction = network.module_transaction(input.tx_summary)?;
    let (proposer, unverified_metadata) = match input.proposal {
        Some(context) => (
            context.proposer,
            serde_json::to_vec(&context.metadata).map_err(|e| e.to_string())?,
        ),
        None => (String::new(), Vec::new()),
    };
    Ok(HookRequest {
        network: "miden".into(),
        account_id: input.account_id.to_string(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        before: Some(view(network, input.before_state, input.auth)?),
        after: Some(view(network, input.after_state, input.auth)?),
        subject: Some(hook_request::Subject::Transaction(TransactionSubject {
            projection: Some(transaction.projection),
            raw_tx_summary: transaction.raw_tx_summary,
            proposer,
            unverified_metadata,
            ..Default::default()
        })),
        ..Default::default()
    })
}

pub struct RegistrationInput<'a> {
    pub account_id: &'a str,
    pub submitter: &'a str,
    pub auth: &'a Auth,
    pub stored_state: Option<&'a serde_json::Value>,
    pub initial_state: &'a serde_json::Value,
    pub accounting_time: DateTime<Utc>,
}

pub fn registration_request(
    network: &dyn NetworkClient,
    input: RegistrationInput<'_>,
) -> Result<HookRequest, String> {
    Ok(HookRequest {
        network: "miden".into(),
        account_id: input.account_id.to_string(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        before: input
            .stored_state
            .map(|state| view(network, state, input.auth))
            .transpose()?,
        after: Some(view(network, input.initial_state, input.auth)?),
        subject: Some(hook_request::Subject::Registration(
            RegistrationSubject::default(),
        )),
        ..Default::default()
    })
}
