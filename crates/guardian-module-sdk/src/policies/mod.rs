//! First-party policies, one concern each, so an instance is configured with only the params of
//! the rule it enforces:
//!
//! | Kind | Judges | Hooks |
//! |---|---|---|
//! | [`spending-limit`](spending_limit) | Which assets a transaction moves and how much of each it sends | proposal creation, transaction approval |
//! | [`recipient-policy`](recipient_policy) | Who a transaction pays | proposal creation, transaction approval |
//! | [`signer-policy`](signer_policy) | Who may sign for the account | registration, proposal creation, transaction approval |
//! | [`code-policy`](code_policy) | Which account code the account may run | registration, proposal creation, transaction approval |
//!
//! Each kind refuses params that belong to another, and supports only the hooks where its rule
//! can refuse something, so a catalog that subscribes it to another hook does not start. An
//! instance with no rule configured allows without reading the request; one with a rule that
//! cannot read what it needs returns an error, so the instance's `on_error` applies. Every
//! denial carries a fixed reason code and a fixed client message that names the rule, never a
//! list entry or a limit; the details go only to the logged message.

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;

use crate::proto::hook_request;
use crate::{
    AccountView, Decision, Hook, HookRequest, MidenTransaction, ModuleError, ModuleInfo,
    PROTOCOL_VERSION, ParamsVerdict,
};

pub mod code_policy;
pub mod recipient_policy;
pub mod signer_policy;
pub mod spending_limit;

pub use code_policy::CodePolicy;
pub use recipient_policy::RecipientPolicy;
pub use signer_policy::SignerPolicy;
pub use spending_limit::SpendingLimit;

/// Every first-party policy kind, as a catalog names it.
pub const KINDS: [&str; 4] = [
    spending_limit::KIND,
    recipient_policy::KIND,
    signer_policy::KIND,
    code_policy::KIND,
];

/// What an id names, which fixes its length in the hex form the Guardian reports it in.
#[derive(Clone, Copy)]
pub(crate) enum IdKind {
    /// A faucet or a recipient: an account id.
    Account,
    /// A signer commitment or a code commitment: a word.
    Commitment,
}

impl IdKind {
    fn hex_digits(self) -> usize {
        match self {
            IdKind::Account => 30,
            IdKind::Commitment => 64,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            IdKind::Account => "an account id",
            IdKind::Commitment => "a commitment",
        }
    }
}

/// Ids in the hex form the Guardian reports them in (`0x` and the hex digits of an account id or
/// a commitment), lowercased. Any other spelling would never match a request, so a list or a
/// cap would silently not apply; it is refused instead: a bech32 address, a placeholder left in
/// an example catalog, an id cut short when it was pasted, or an account id where a commitment
/// belongs.
pub(crate) fn ids(
    field: &str,
    kind: IdKind,
    values: &[String],
) -> Result<BTreeSet<String>, String> {
    values.iter().map(|value| id(field, kind, value)).collect()
}

pub(crate) fn id(field: &str, kind: IdKind, value: &str) -> Result<String, String> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .filter(|rest| rest.bytes().all(|b| b.is_ascii_hexdigit()));
    match digits {
        Some(rest) if rest.len() == kind.hex_digits() => Ok(value.to_ascii_lowercase()),
        Some(rest) if !rest.is_empty() => Err(format!(
            "{field}: {value:?} has {} hex digits, but {} has {}",
            rest.len(),
            kind.noun(),
            kind.hex_digits()
        )),
        _ => Err(format!(
            "{field}: {value:?} is not an id in 0x hex form (a bech32 address is not accepted)"
        )),
    }
}

pub(crate) fn normalized(values: &[String]) -> BTreeSet<String> {
    values.iter().map(|v| v.to_ascii_lowercase()).collect()
}

/// A rule's denial. Each denies with a fixed reason code and a fixed client message that names
/// the rule, never a list entry or a limit, so a denial discloses no params.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Rule {
    RecipientAllowlist,
    RecipientDenylist,
    AssetAllowlist,
    AmountCap,
    Signers,
    Code,
}

impl Rule {
    pub(crate) fn deny(self, logged: String) -> Decision {
        let (reason_code, client_message) = match self {
            Rule::RecipientAllowlist => (
                "recipient_not_allowed",
                "A payment goes to a recipient this Guardian does not accept.",
            ),
            Rule::RecipientDenylist => (
                "recipient_blocked",
                "A payment goes to a recipient this Guardian blocks.",
            ),
            Rule::AssetAllowlist => (
                "asset_not_allowed",
                "The transaction moves an asset this Guardian does not accept.",
            ),
            Rule::AmountCap => (
                "amount_cap_exceeded",
                "The transaction sends more than this Guardian allows per transaction.",
            ),
            Rule::Signers => (
                "signer_not_allowed",
                "The account would have a signer this Guardian does not accept.",
            ),
            Rule::Code => (
                "code_not_allowed",
                "The account would run code this Guardian does not accept.",
            ),
        };
        Decision::deny(reason_code, logged).with_client_message(client_message)
    }
}

/// The params as a kind reads them: absent bytes are no params, anything else must be a JSON
/// object of that kind's fields only.
pub(crate) fn parse<P: DeserializeOwned + Default>(params: &[u8]) -> Result<P, String> {
    if params.is_empty() {
        Ok(P::default())
    } else {
        serde_json::from_slice(params).map_err(|e| e.to_string())
    }
}

pub(crate) fn verdict<T>(parsed: Result<T, String>) -> ParamsVerdict {
    match parsed {
        Ok(_) => ParamsVerdict::ok(),
        Err(message) => ParamsVerdict::invalid(message),
    }
}

pub(crate) fn info(kind: &str, hooks: &[Hook]) -> ModuleInfo {
    ModuleInfo {
        name: kind.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        hooks: hooks.iter().map(|hook| *hook as i32).collect(),
        ..Default::default()
    }
}

/// The transaction a request asks about, in its Miden projection. A request that carries no
/// transaction, or no projection this module can read, is not judged: it is an error, so the
/// instance's `on_error` applies, never an allow.
pub(crate) fn transaction(request: &HookRequest) -> Result<&MidenTransaction, ModuleError> {
    match &request.subject {
        Some(hook_request::Subject::Transaction(tx)) => tx.miden().ok_or_else(|| {
            ModuleError::InvalidRequest("transaction has no Miden projection to judge".into())
        }),
        Some(hook_request::Subject::Registration(_)) => Err(ModuleError::InvalidRequest(
            "this policy judges transactions, not registrations".into(),
        )),
        None => Err(ModuleError::InvalidRequest("request has no subject".into())),
    }
}

/// The account as it will be if every module allows; a request without it is not judged.
pub(crate) fn after(request: &HookRequest) -> Result<&AccountView, ModuleError> {
    request
        .after
        .as_ref()
        .ok_or_else(|| ModuleError::InvalidRequest("request has no account view".into()))
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::proto::{asset, hook_request};
    use crate::{
        AccountView, Asset, HookRequest, MidenTransaction, OutputNote, RegistrationSubject,
        TransactionProjection, TransactionSubject,
    };

    pub const USDC: &str = "0x00000000000000000000000000aa00";
    pub const ETH: &str = "0x00000000000000000000000000bb00";
    pub const RECIPIENT: &str = "0x0000000000000000000000000000a1";
    pub const SIGNER: &str = "0x0000000000000000000000000000000000000000000000000000000000000051";

    pub fn fungible(faucet: &str, amount: u64) -> Asset {
        Asset {
            faucet_id: faucet.into(),
            value: Some(asset::Value::FungibleAmount(amount.to_string())),
        }
    }

    pub fn p2id(target: &str, assets: Vec<Asset>) -> OutputNote {
        OutputNote {
            note_id: format!("0xnote{target}"),
            assets,
            p2id_target: Some(target.into()),
            ..Default::default()
        }
    }

    pub fn fee_note(amount: u64) -> OutputNote {
        OutputNote {
            note_id: "0xfee".into(),
            assets: vec![fungible(USDC, amount)],
            tx_fee: true,
            ..Default::default()
        }
    }

    pub fn view(signers: &[&str]) -> AccountView {
        AccountView {
            signer_commitments: signers.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    pub fn tx_request(params: serde_json::Value, projection: MidenTransaction) -> HookRequest {
        HookRequest {
            params: serde_json::to_vec(&params).unwrap(),
            before: Some(view(&[SIGNER])),
            after: Some(view(&[SIGNER])),
            subject: Some(hook_request::Subject::Transaction(TransactionSubject {
                projection: Some(TransactionProjection::miden(projection)),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    pub fn registration_request(
        params: serde_json::Value,
        before: Option<&[&str]>,
        after: &[&str],
    ) -> HookRequest {
        HookRequest {
            params: serde_json::to_vec(&params).unwrap(),
            before: before.map(view),
            after: Some(view(after)),
            subject: Some(hook_request::Subject::Registration(
                RegistrationSubject::default(),
            )),
            ..Default::default()
        }
    }

    pub fn without_projection(mut request: HookRequest) -> HookRequest {
        if let Some(hook_request::Subject::Transaction(subject)) = &mut request.subject {
            subject.projection = None;
        }
        request
    }
}
