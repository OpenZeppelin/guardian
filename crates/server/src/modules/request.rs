//! Assembling what a module receives: account views, projection and proposal context. Assembly
//! decodes nothing: the views and projections come from the decode each request already makes
//! inside the reconstruction task, so the async workers only copy finished parts.

use chrono::{DateTime, Utc};
use guardian_module_sdk::prost_types::Timestamp;
use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{AccountView, HookRequest, RegistrationSubject, TransactionSubject};

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::metadata::NetworkConfig;
use crate::metadata::auth::Auth;
use crate::network::{ModuleAccount, ModuleAccountInputs, ModuleInputs};

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
fn view(account: Result<ModuleAccount, String>, auth: &Auth) -> Result<AccountView, String> {
    let mut view = account?.view;
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

/// Modules are not offered EVM accounts. The network a module is told is the one this Guardian
/// serves, never the `network_type` an account was registered with, which its registrant chose.
fn ensure_miden(config: &NetworkConfig) -> Result<(), String> {
    match config {
        NetworkConfig::Miden { .. } => Ok(()),
        NetworkConfig::Evm { .. } => Err("policy modules do not evaluate EVM accounts".into()),
    }
}

pub struct TransactionInput<'a> {
    pub network_id: &'static str,
    pub account_id: &'a str,
    pub submitter: &'a str,
    pub submitter_session: bool,
    pub auth: &'a Auth,
    pub network_config: &'a NetworkConfig,
    pub inputs: ModuleInputs,
    pub proposal: Option<ProposalContext>,
    pub accounting_time: DateTime<Utc>,
}

pub fn transaction_request(input: TransactionInput<'_>) -> Result<HookRequest, String> {
    ensure_miden(input.network_config)?;
    let ModuleInputs {
        before,
        after,
        transaction,
    } = input.inputs;
    let transaction = transaction?;
    let (proposer, unverified_metadata) = match input.proposal {
        Some(context) => (
            context.proposer,
            serde_json::to_vec(&context.metadata).map_err(|e| e.to_string())?,
        ),
        None => (String::new(), Vec::new()),
    };
    Ok(HookRequest {
        network: "miden".into(),
        network_id: input.network_id.into(),
        account_id: input.account_id.to_ascii_lowercase(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        submitter_session: input.submitter_session,
        before: Some(view(before, input.auth)?),
        after: Some(view(after, input.auth)?),
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
    pub network_id: &'static str,
    pub account_id: &'a str,
    pub submitter: &'a str,
    pub submitter_session: bool,
    pub auth: &'a Auth,
    pub network_config: &'a NetworkConfig,
    /// The view of the state already stored for the account, when there is one.
    pub stored: Option<Result<ModuleAccount, String>>,
    /// The view and the whole state of the initial state the request registers.
    pub initial: ModuleAccountInputs,
    pub accounting_time: DateTime<Utc>,
}

pub fn registration_request(input: RegistrationInput<'_>) -> Result<HookRequest, String> {
    ensure_miden(input.network_config)?;
    Ok(HookRequest {
        network: "miden".into(),
        network_id: input.network_id.into(),
        account_id: input.account_id.to_ascii_lowercase(),
        accounting_time: Some(timestamp(input.accounting_time)),
        submitter: input.submitter.to_string(),
        submitter_session: input.submitter_session,
        before: input
            .stored
            .map(|stored| view(stored, input.auth))
            .transpose()?,
        after: Some(view(input.initial.view, input.auth)?),
        subject: Some(hook_request::Subject::Registration(RegistrationSubject {
            initial_state: Some(guardian_module_sdk::AccountStateProjection::miden(
                input.initial.state?,
            )),
        })),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        view(
            Ok(ModuleAccount {
                view: decoded,
                nonce: 0,
            }),
            auth,
        )
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

    fn registration(
        config: &NetworkConfig,
        auth: &Auth,
        initial: ModuleAccountInputs,
    ) -> Result<HookRequest, String> {
        registration_request(RegistrationInput {
            network_id: "mainnet",
            account_id: "0xacc",
            submitter: "0xa",
            submitter_session: false,
            auth,
            network_config: config,
            stored: None,
            initial,
            accounting_time: Utc::now(),
        })
    }

    fn decoded(view: AccountView) -> ModuleAccountInputs {
        ModuleAccountInputs {
            view: Ok(ModuleAccount { view, nonce: 0 }),
            state: Ok(guardian_module_sdk::MidenAccountState::default()),
        }
    }

    #[test]
    fn requests_name_the_guardians_network_and_refuse_evm_accounts() {
        use crate::metadata::network::MidenNetworkType;
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xa".into()],
        };
        let registered_as_testnet = registration(
            &NetworkConfig::Miden {
                network_type: MidenNetworkType::Testnet,
            },
            &auth,
            decoded(multisig_view(&[("0xa", "falcon")])),
        )
        .unwrap();
        assert_eq!(registered_as_testnet.network, "miden");
        assert_eq!(
            registered_as_testnet.network_id, "mainnet",
            "the Guardian's network, not the one the account was registered with"
        );
        assert_eq!(
            registered_as_testnet
                .subject
                .and_then(|subject| match subject {
                    hook_request::Subject::Registration(registration) => registration.initial_state,
                    hook_request::Subject::Transaction(_) => None,
                })
                .and_then(|state| state.network),
            Some(
                guardian_module_sdk::proto::account_state_projection::Network::Miden(
                    guardian_module_sdk::MidenAccountState::default()
                )
            ),
            "the registration carries the account's state"
        );
        assert!(
            registration(
                &NetworkConfig::Evm {
                    chain_id: 1,
                    account_address: "0x1".into(),
                    multisig_validator_address: "0x2".into(),
                },
                &auth,
                decoded(multisig_view(&[("0xa", "falcon")])),
            )
            .is_err()
        );
    }

    #[test]
    fn modules_receive_the_account_id_in_lowercase_whatever_spelling_registered_it() {
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xa".into()],
        };
        let request = registration_request(RegistrationInput {
            network_id: "mainnet",
            account_id: "0xAcC",
            submitter: "0xa",
            submitter_session: false,
            auth: &auth,
            network_config: &NetworkConfig::miden_default(),
            stored: None,
            initial: decoded(multisig_view(&[("0xa", "falcon")])),
            accounting_time: Utc::now(),
        })
        .unwrap();
        assert_eq!(request.account_id, "0xacc");
    }

    #[test]
    fn an_input_that_could_not_be_extracted_fails_the_request() {
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xa".into()],
        };
        let config = NetworkConfig::miden_default();
        let undecodable_state = ModuleAccountInputs {
            state: Err("state does not decode".into()),
            ..decoded(multisig_view(&[("0xa", "falcon")]))
        };
        assert_eq!(
            registration(&config, &auth, undecodable_state).unwrap_err(),
            "state does not decode"
        );
        let transaction = transaction_request(TransactionInput {
            network_id: "mainnet",
            account_id: "0xacc",
            submitter: "0xa",
            submitter_session: false,
            auth: &auth,
            network_config: &config,
            inputs: ModuleInputs {
                before: Err("base does not decode".into()),
                after: Ok(ModuleAccount {
                    view: multisig_view(&[("0xa", "falcon")]),
                    nonce: 1,
                }),
                transaction: Ok(Default::default()),
            },
            proposal: None,
            accounting_time: Utc::now(),
        });
        assert_eq!(transaction.unwrap_err(), "base does not decode");
    }
}
