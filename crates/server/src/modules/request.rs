//! Assembling what a module receives: account views, projection and proposal context.

use chrono::{DateTime, Utc};
use guardian_module_sdk::prost_types::Timestamp;
use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{AccountView, HookRequest, RegistrationSubject, TransactionSubject};

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::metadata::auth::Auth;
use crate::network::NetworkClient;

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalContext {
    pub proposer: String,
    pub metadata: serde_json::Value,
}

/// The proposer and metadata of a stored proposal that is still pending.
pub fn proposal_context(proposal: &DeltaObject) -> Option<ProposalContext> {
    match &proposal.status {
        DeltaStatus::Pending { proposer_id, .. } => Some(ProposalContext {
            proposer: proposer_id.clone(),
            metadata: proposal
                .delta_payload
                .get("metadata")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        }),
        _ => None,
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

/// An account state and the commitment the Guardian computed for it (never one a client sent).
#[derive(Clone, Copy)]
pub struct StateRef<'a> {
    pub commitment: &'a str,
    pub json: &'a serde_json::Value,
}

const VIEW_CACHE_CAPACITY: usize = 4096;

/// Account views by state commitment. Decoding an account is the main cost of a module request,
/// and states repeat: a chained push starts from the previous push's result, and a proposal's
/// approval sees the same before and after states as its creation. A view depends only on the
/// state, so the commitment is a sufficient key; the parts taken from the registered auth
/// configuration are filled in after the lookup.
#[derive(Default)]
pub struct ViewCache {
    entries: std::sync::Mutex<(
        std::collections::HashMap<String, AccountView>,
        std::collections::VecDeque<String>,
    )>,
}

impl ViewCache {
    fn get_or_build(
        &self,
        network: &dyn NetworkClient,
        state: StateRef<'_>,
    ) -> Result<AccountView, String> {
        if let Ok(entries) = self.entries.lock()
            && let Some(view) = entries.0.get(state.commitment)
        {
            return Ok(view.clone());
        }
        let view = network.module_account_view(state.json)?;
        if let Ok(mut entries) = self.entries.lock() {
            let (views, order) = &mut *entries;
            if views
                .insert(state.commitment.to_string(), view.clone())
                .is_none()
            {
                order.push_back(state.commitment.to_string());
                if order.len() > VIEW_CACHE_CAPACITY
                    && let Some(oldest) = order.pop_front()
                {
                    views.remove(&oldest);
                }
            }
        }
        Ok(view)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.0.len())
    }
}

/// The account's view with the scheme from its verified auth configuration. A single-key
/// account (no `default_threshold`) takes its one signer from that configuration too.
fn view(
    network: &dyn NetworkClient,
    views: &ViewCache,
    state: StateRef<'_>,
    auth: &Auth,
) -> Result<AccountView, String> {
    let mut view = views.get_or_build(network, state)?;
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
    pub before: StateRef<'a>,
    pub after: StateRef<'a>,
    pub tx_summary: &'a serde_json::Value,
    pub proposal: Option<ProposalContext>,
    pub accounting_time: DateTime<Utc>,
}

pub fn transaction_request(
    network: &dyn NetworkClient,
    views: &ViewCache,
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
        before: Some(view(network, views, input.before, input.auth)?),
        after: Some(view(network, views, input.after, input.auth)?),
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
    pub stored_state: Option<StateRef<'a>>,
    pub initial_state: StateRef<'a>,
    pub accounting_time: DateTime<Utc>,
}

pub fn registration_request(
    network: &dyn NetworkClient,
    views: &ViewCache,
    input: RegistrationInput<'_>,
) -> Result<HookRequest, String> {
    Ok(HookRequest {
        network: "miden".into(),
        account_id: input.account_id.to_string(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        before: input
            .stored_state
            .map(|state| view(network, views, state, input.auth))
            .transpose()?,
        after: Some(view(network, views, input.initial_state, input.auth)?),
        subject: Some(hook_request::Subject::Registration(
            RegistrationSubject::default(),
        )),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mocks::MockNetworkClient;

    fn view_with(signer: &str) -> AccountView {
        AccountView {
            signer_commitments: vec![signer.into()],
            default_threshold: Some(1),
            ..Default::default()
        }
    }

    fn state(commitment: &str) -> StateRef<'_> {
        static STATE: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        StateRef {
            commitment,
            json: STATE.get_or_init(|| serde_json::json!({})),
        }
    }

    #[test]
    fn a_state_seen_before_is_not_decoded_again() {
        let network =
            MockNetworkClient::new().with_module_views(view_with("0xfirst"), Default::default());
        let cache = ViewCache::default();
        assert_eq!(
            cache.get_or_build(&network, state("0xc1")).unwrap(),
            view_with("0xfirst")
        );
        *network.module_account_view.lock().unwrap() = Some(view_with("0xsecond"));
        assert_eq!(
            cache.get_or_build(&network, state("0xc1")).unwrap(),
            view_with("0xfirst"),
            "same commitment, cached view"
        );
        assert_eq!(
            cache.get_or_build(&network, state("0xc2")).unwrap(),
            view_with("0xsecond"),
            "new commitment, fresh decode"
        );
    }

    #[test]
    fn the_cache_is_bounded() {
        let network =
            MockNetworkClient::new().with_module_views(view_with("0xs"), Default::default());
        let cache = ViewCache::default();
        for n in 0..VIEW_CACHE_CAPACITY + 10 {
            cache
                .get_or_build(&network, state(&format!("0x{n}")))
                .unwrap();
        }
        assert_eq!(cache.len(), VIEW_CACHE_CAPACITY);
    }

    #[test]
    fn a_failed_decode_is_not_cached() {
        let network = MockNetworkClient::new();
        let cache = ViewCache::default();
        assert!(cache.get_or_build(&network, state("0xc1")).is_err());
        assert_eq!(cache.len(), 0);
    }
}
