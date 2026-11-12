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
/// a commitment), lowercased; an account may also be named by its bech32 address, which is read
/// as its hex id. Any other spelling would never match a request, so a list or a cap would
/// silently not apply; it is refused instead: a placeholder left in an example catalog, an id
/// cut short when it was pasted, a bech32 address that is not an account's, or an account id
/// where a commitment belongs.
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
    match (digits, kind) {
        (Some(rest), _) if rest.len() == kind.hex_digits() => Ok(value.to_ascii_lowercase()),
        (Some(rest), _) if !rest.is_empty() => Err(format!(
            "{field}: {value:?} has {} hex digits, but {} has {}",
            rest.len(),
            kind.noun(),
            kind.hex_digits()
        )),
        (None, IdKind::Account) => account_address(value).map_err(|refused| {
            let why = match refused {
                NotAnAccount::NotBech32 => {
                    "is neither an account id in 0x hex form nor a bech32 account address".into()
                }
                NotAnAccount::Routing => {
                    "has a routing part after `_` that is not valid bech32m".into()
                }
                NotAnAccount::Type(address_type) => {
                    format!("is a bech32 address of type {address_type}, not of an account id")
                }
                NotAnAccount::Length(bytes) => format!(
                    "is a bech32 account address of {bytes} bytes, but an account id has {}",
                    IdKind::Account.hex_digits() / 2
                ),
            };
            format!("{field}: {value:?} {why}")
        }),
        _ => Err(format!(
            "{field}: {value:?} is not {} in 0x hex form",
            kind.noun()
        )),
    }
}

/// The address type byte of a Miden account address.
const ACCOUNT_ADDRESS: u8 = 232;

/// The prefix Miden checksums an address's routing part under (it is written without it).
const ROUTING_PREFIX: &str = "mrp1";

/// Why a string is not a Miden bech32m account address.
enum NotAnAccount {
    /// Not a bech32m string at all.
    NotBech32,
    /// The part after `_` is empty or fails its checksum, which Miden refuses too.
    Routing,
    /// An address of another type.
    Type(u8),
    /// An account address whose id has this many bytes.
    Length(usize),
}

/// The hex id of the account a Miden bech32m address names, `<id>[_<routing params>]`. The
/// routing part is checked as Miden checks it, then set aside: it says how to reach the
/// account, not which account it is.
fn account_address(value: &str) -> Result<String, NotAnAccount> {
    fn checked(text: &str) -> Option<bech32::primitives::decode::CheckedHrpstring<'_>> {
        bech32::primitives::decode::CheckedHrpstring::new::<bech32::Bech32m>(text).ok()
    }
    let (identifier, routing) = value
        .split_once('_')
        .map_or((value, None), |(identifier, routing)| {
            (identifier, Some(routing))
        });
    let bytes: Vec<u8> = checked(identifier)
        .ok_or(NotAnAccount::NotBech32)?
        .byte_iter()
        .collect();
    if let Some(routing) = routing {
        checked(&format!("{ROUTING_PREFIX}{routing}")).ok_or(NotAnAccount::Routing)?;
    }
    match bytes.split_first() {
        Some((&ACCOUNT_ADDRESS, id)) if id.len() * 2 == IdKind::Account.hex_digits() => {
            Ok(format!(
                "0x{}",
                id.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ))
        }
        Some((&ACCOUNT_ADDRESS, id)) => Err(NotAnAccount::Length(id.len())),
        Some((&address_type, _)) => Err(NotAnAccount::Type(address_type)),
        None => Err(NotAnAccount::NotBech32),
    }
}

/// Where a kind's params name accounts: entries of a list, or the keys of a map.
pub(crate) enum Accounts {
    List(&'static str),
    Keys(&'static str),
}

/// `params` with every account named by its bech32 address rewritten to its hex id, so stored
/// params, their history and their disclosure show the id requests carry; `None` when no entry
/// needed it, so params already in hex are stored as sent.
fn hex_accounts(params: &[u8], accounts: &[Accounts]) -> Option<serde_json::Value> {
    let hex = |value: &str| account_address(value).ok();
    let mut value: serde_json::Value = serde_json::from_slice(params).ok()?;
    let mut changed = false;
    for field in accounts {
        match field {
            Accounts::List(name) => {
                let entries = value
                    .get_mut(*name)
                    .and_then(serde_json::Value::as_array_mut);
                for entry in entries.into_iter().flatten() {
                    if let Some(id) = entry.as_str().and_then(hex) {
                        *entry = id.into();
                        changed = true;
                    }
                }
            }
            Accounts::Keys(name) => {
                let Some(map) = value
                    .get_mut(*name)
                    .and_then(serde_json::Value::as_object_mut)
                else {
                    continue;
                };
                let renamed: Vec<(String, String)> = map
                    .keys()
                    .filter_map(|key| hex(key).map(|id| (key.clone(), id)))
                    .collect();
                for (key, id) in renamed {
                    if let Some(entry) = map.remove(&key) {
                        map.insert(id, entry);
                        changed = true;
                    }
                }
            }
        }
    }
    changed.then_some(value)
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

/// The verdict on `params` as a kind parsed them; valid params that name an account in
/// `accounts` by its bech32 address come back normalized to the hex id.
pub(crate) fn verdict<T>(
    params: &[u8],
    parsed: Result<T, String>,
    accounts: &[Accounts],
) -> ParamsVerdict {
    match parsed {
        Ok(_) => hex_accounts(params, accounts)
            .map(|normalized| ParamsVerdict::normalized(&normalized))
            .unwrap_or_else(ParamsVerdict::ok),
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
    /// An account id and its bech32 addresses, as `miden_protocol` encodes them: on devnet, and
    /// on testnet with basic-wallet routing params.
    pub const ADDRESSED: &str = "0x1fc2e85e8e2c2511561d4c1a78b478";
    pub const DEVNET_ADDRESS: &str = "mdev1aq0u96z73ckz2y2kr4xp57950qrn4xjc";
    pub const ROUTED_ADDRESS: &str = "mtst1aq0u96z73ckz2y2kr4xp57950qmexkt5_qr7qqq9wr6w";

    /// A valid bech32m address of `address_type` (232 is an account) over `id_bytes` zero bytes.
    pub fn address_of(address_type: u8, id_bytes: usize) -> String {
        let mut payload = vec![address_type];
        payload.extend(vec![0u8; id_bytes]);
        bech32::encode::<bech32::Bech32m>(bech32::Hrp::parse("mdev").unwrap(), &payload).unwrap()
    }

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
