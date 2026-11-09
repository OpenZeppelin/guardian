//! Assembling what a module receives: account views, projection and proposal context.

use chrono::{DateTime, Utc};
use guardian_module_sdk::prost_types::Timestamp;
use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{AccountView, HookRequest, RegistrationSubject, TransactionSubject};

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::metadata::NetworkConfig;
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
        self.insert(state.commitment, view.clone());
        Ok(view)
    }

    fn insert(&self, commitment: &str, view: AccountView) {
        if let Ok(mut entries) = self.entries.lock() {
            let (views, order) = &mut *entries;
            if views.insert(commitment.to_string(), view).is_none() {
                order.push_back(commitment.to_string());
                if order.len() > VIEW_CACHE_CAPACITY
                    && let Some(oldest) = order.pop_front()
                {
                    views.remove(&oldest);
                }
            }
        }
    }

    /// Keeps `view` for `commitment` when it was built from an account already decoded, so request
    /// assembly does not decode it again. A failed extraction is not kept: assembly then retries
    /// it and the failure applies to the modules.
    pub fn seed(&self, commitment: &str, view: Result<AccountView, String>) {
        if let Ok(view) = view {
            self.insert(commitment, view);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.0.len())
    }
}

/// The scheme every signer shares, or `mixed` when the signers use more than one.
fn signers_scheme(schemes: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let mut distinct: Vec<&String> = schemes.values().collect();
    distinct.sort();
    distinct.dedup();
    match distinct.as_slice() {
        [] => Err("account view has no signer schemes".into()),
        [scheme] => Ok((*scheme).clone()),
        _ => Ok("mixed".into()),
    }
}

/// The account's view, its `auth_scheme` summarizing the signers' schemes. A multisig account's
/// signers and schemes are decoded from its state; a single-key account (no `default_threshold`)
/// takes its one signer and that signer's scheme from its verified auth configuration.
fn view(
    network: &dyn NetworkClient,
    views: &ViewCache,
    state: StateRef<'_>,
    auth: &Auth,
) -> Result<AccountView, String> {
    let mut view = views.get_or_build(network, state)?;
    if view.default_threshold.is_none() {
        match auth.cosigner_commitments().as_slice() {
            [signer] => {
                let scheme = auth.signer_scheme(signer).ok_or_else(|| {
                    format!("single-key signer {signer} has no registered scheme")
                })?;
                view.signer_commitments = vec![signer.clone()];
                view.signer_schemes = [(signer.clone(), scheme.as_str().to_string())].into();
            }
            signers => {
                return Err(format!(
                    "single-key account registered with {} signers",
                    signers.len()
                ));
            }
        }
    }
    view.auth_scheme = signers_scheme(&view.signer_schemes)?;
    Ok(view)
}

/// The account's network within its family, from its registered configuration; modules are not
/// offered EVM accounts.
fn network_id(config: &NetworkConfig) -> Result<&'static str, String> {
    match config {
        NetworkConfig::Miden { network_type } => Ok(network_type.as_str()),
        NetworkConfig::Evm { .. } => Err("policy modules do not evaluate EVM accounts".into()),
    }
}

pub struct TransactionInput<'a> {
    pub account_id: &'a str,
    pub submitter: &'a str,
    pub submitter_session: bool,
    pub auth: &'a Auth,
    pub network_config: &'a NetworkConfig,
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
        network_id: network_id(input.network_config)?.into(),
        account_id: input.account_id.to_string(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        submitter_session: input.submitter_session,
        before: Some(view(network, views, input.before, input.auth)?),
        after: Some(view(network, views, input.after, input.auth)?),
        subject: Some(hook_request::Subject::Transaction(TransactionSubject {
            projection: Some(guardian_module_sdk::TransactionProjection::miden(
                transaction.projection,
            )),
            raw: Some(guardian_module_sdk::RawTransaction {
                encoding: guardian_module_sdk::RawTransaction::MIDEN_TRANSACTION_SUMMARY.into(),
                data: transaction.raw_tx_summary,
            }),
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
    pub submitter_session: bool,
    pub auth: &'a Auth,
    pub network_config: &'a NetworkConfig,
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
        network_id: network_id(input.network_config)?.into(),
        account_id: input.account_id.to_string(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        submitter_session: input.submitter_session,
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

    fn multisig_view(schemes: &[(&str, &str)]) -> AccountView {
        AccountView {
            signer_commitments: schemes.iter().map(|(signer, _)| (*signer).into()).collect(),
            signer_schemes: schemes
                .iter()
                .map(|(signer, scheme)| ((*signer).into(), (*scheme).into()))
                .collect(),
            default_threshold: Some(1),
            ..Default::default()
        }
    }

    fn built(decoded: AccountView, auth: &Auth) -> Result<AccountView, String> {
        let network = MockNetworkClient::new().with_module_views(decoded, Default::default());
        view(&network, &ViewCache::default(), state("0xc1"), auth)
    }

    #[test]
    fn auth_scheme_summarizes_the_decoded_signer_schemes() {
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xa".into()],
        };
        let shared = built(multisig_view(&[("0xa", "ecdsa"), ("0xb", "ecdsa")]), &auth).unwrap();
        assert_eq!(
            shared.auth_scheme, "ecdsa",
            "read from state, not the registration"
        );
        let mixed = built(multisig_view(&[("0xa", "falcon"), ("0xb", "ecdsa")]), &auth).unwrap();
        assert_eq!(mixed.auth_scheme, "mixed");
        assert_eq!(
            mixed.signer_schemes.get("0xb").map(String::as_str),
            Some("ecdsa")
        );
    }

    #[test]
    fn a_single_key_view_takes_its_signer_and_scheme_from_the_registration() {
        let auth = Auth::MidenEcdsa {
            cosigner_commitments: vec!["0xsolo".into()],
        };
        let single = built(AccountView::default(), &auth).unwrap();
        assert_eq!(single.signer_commitments, vec!["0xsolo".to_string()]);
        assert_eq!(
            single.signer_schemes.get("0xsolo").map(String::as_str),
            Some("ecdsa")
        );
        assert_eq!(single.auth_scheme, "ecdsa");
    }

    #[test]
    fn requests_name_the_accounts_network_and_refuse_evm_accounts() {
        use crate::metadata::network::MidenNetworkType;
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xa".into()],
        };
        let network = MockNetworkClient::new()
            .with_module_views(multisig_view(&[("0xa", "falcon")]), Default::default());
        let request = |config: &NetworkConfig| {
            registration_request(
                &network,
                &ViewCache::default(),
                RegistrationInput {
                    account_id: "0xacc",
                    submitter: "0xa",
                    submitter_session: false,
                    auth: &auth,
                    network_config: config,
                    stored_state: None,
                    initial_state: state("0xc1"),
                    accounting_time: Utc::now(),
                },
            )
        };
        let testnet = request(&NetworkConfig::Miden {
            network_type: MidenNetworkType::Testnet,
        })
        .unwrap();
        assert_eq!(testnet.network, "miden");
        assert_eq!(testnet.network_id, "testnet");
        assert!(
            request(&NetworkConfig::Evm {
                chain_id: 1,
                account_address: "0x1".into(),
                multisig_validator_address: "0x2".into(),
            })
            .is_err()
        );
    }

    #[test]
    fn a_seeded_view_is_used_without_decoding_and_a_failed_one_is_not_kept() {
        let network = MockNetworkClient::new();
        let cache = ViewCache::default();
        cache.seed("0xc1", Ok(view_with("0xseeded")));
        assert_eq!(
            cache.get_or_build(&network, state("0xc1")).unwrap(),
            view_with("0xseeded")
        );
        cache.seed("0xc2", Err("undecodable".into()));
        assert!(cache.get_or_build(&network, state("0xc2")).is_err());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_failed_decode_is_not_cached() {
        let network = MockNetworkClient::new();
        let cache = ViewCache::default();
        assert!(cache.get_or_build(&network, state("0xc1")).is_err());
        assert_eq!(cache.len(), 0);
    }
}
