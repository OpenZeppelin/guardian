//! Reference policy: per-asset amount cap, asset and recipient allowlists, signer lists.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::Deserialize;

use crate::proto::{asset, hook_request};
use crate::{
    AccountView, Decision, DescribeRequest, Hook, HookRequest, MidenTransaction, Module,
    ModuleError, ModuleInfo, PROTOCOL_VERSION, ParamsVerdict, TransactionSubject,
};

pub const KIND: &str = "basic-policy";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerCheck {
    #[default]
    Added,
    All,
}

/// Which code `allowed_code_commitments` checks: only code an account registers with or changes
/// to (`changed`), or the code it runs at every gate hook (`all`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeCheck {
    #[default]
    Changed,
    All,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub max_amount_per_tx: BTreeMap<String, String>,
    #[serde(default)]
    pub asset_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub recipient_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub recipient_denylist: Option<Vec<String>>,
    #[serde(default)]
    pub deny_unknown_recipients: bool,
    #[serde(default)]
    pub signer_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub signer_denylist: Option<Vec<String>>,
    #[serde(default)]
    pub signer_check: Option<SignerCheck>,
    #[serde(default)]
    pub allowed_code_commitments: Option<Vec<String>>,
    #[serde(default)]
    pub code_check: Option<CodeCheck>,
}

struct Policy {
    caps: BTreeMap<String, u128>,
    assets: Option<BTreeSet<String>>,
    recipients: Option<BTreeSet<String>>,
    blocked_recipients: Option<BTreeSet<String>>,
    deny_unknown_recipients: bool,
    signers: Option<SignerList>,
    signer_check: SignerCheck,
    code: Option<BTreeSet<String>>,
    code_check: CodeCheck,
}

enum SignerList {
    Allow(BTreeSet<String>),
    Deny(BTreeSet<String>),
}

impl SignerList {
    fn permits(&self, signer: &str) -> bool {
        match self {
            SignerList::Allow(list) => list.contains(signer),
            SignerList::Deny(list) => !list.contains(signer),
        }
    }
}

fn normalized(values: &[String]) -> BTreeSet<String> {
    values.iter().map(|v| v.to_ascii_lowercase()).collect()
}

/// Ids in the hex form the Guardian reports them in (`0x` and hex digits), lowercased. Any other
/// spelling, a bech32 address or a placeholder left in an example catalog for one, would never
/// match a request, so a denylist or a cap would silently not apply; it is refused instead.
fn ids(field: &str, values: &[String]) -> Result<BTreeSet<String>, String> {
    values.iter().map(|value| id(field, value)).collect()
}

fn id(field: &str, value: &str) -> Result<String, String> {
    match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(rest) if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(value.to_ascii_lowercase())
        }
        _ => Err(format!(
            "{field}: {value:?} is not an id in 0x hex form (a bech32 address is not accepted)"
        )),
    }
}

impl TryFrom<Params> for Policy {
    type Error = String;

    fn try_from(params: Params) -> Result<Self, String> {
        let mut caps = BTreeMap::new();
        for (faucet, amount) in params.max_amount_per_tx {
            let faucet = id("max_amount_per_tx", &faucet)?;
            let amount = amount
                .parse::<u128>()
                .map_err(|_| format!("max_amount_per_tx for {faucet} is not a decimal amount"))?;
            if caps.insert(faucet.clone(), amount).is_some() {
                return Err(format!(
                    "max_amount_per_tx names {faucet} more than once (ids are compared in lowercase)"
                ));
            }
        }
        if params.deny_unknown_recipients && params.recipient_denylist.is_none() {
            return Err(
                "deny_unknown_recipients applies to recipient_denylist, which is not set".into(),
            );
        }
        if params.signer_check.is_some()
            && params.signer_allowlist.is_none()
            && params.signer_denylist.is_none()
        {
            return Err(
                "signer_check applies to signer_allowlist or signer_denylist, neither of which is set"
                    .into(),
            );
        }
        if params.code_check.is_some() && params.allowed_code_commitments.is_none() {
            return Err("code_check applies to allowed_code_commitments, which is not set".into());
        }
        let signers = match (params.signer_allowlist, params.signer_denylist) {
            (Some(_), Some(_)) => {
                return Err("signer_allowlist and signer_denylist are mutually exclusive".into());
            }
            (Some(list), None) => Some(SignerList::Allow(ids("signer_allowlist", &list)?)),
            (None, Some(list)) => Some(SignerList::Deny(ids("signer_denylist", &list)?)),
            (None, None) => None,
        };
        Ok(Self {
            caps,
            assets: params
                .asset_allowlist
                .as_deref()
                .map(|list| ids("asset_allowlist", list))
                .transpose()?,
            recipients: params
                .recipient_allowlist
                .as_deref()
                .map(|list| ids("recipient_allowlist", list))
                .transpose()?,
            blocked_recipients: params
                .recipient_denylist
                .as_deref()
                .map(|list| ids("recipient_denylist", list))
                .transpose()?,
            deny_unknown_recipients: params.deny_unknown_recipients,
            signers,
            signer_check: params.signer_check.unwrap_or_default(),
            code: params
                .allowed_code_commitments
                .as_deref()
                .map(|list| ids("allowed_code_commitments", list))
                .transpose()?,
            code_check: params.code_check.unwrap_or_default(),
        })
    }
}

fn parse_policy(params: &[u8]) -> Result<Policy, String> {
    let params: Params = if params.is_empty() {
        Params::default()
    } else {
        serde_json::from_slice(params).map_err(|e| e.to_string())?
    };
    Policy::try_from(params)
}

/// A `basic-policy` rule. Each denies with a fixed reason code and a fixed client message that
/// names the rule, never a list entry or a limit, so a denial discloses no params; the details
/// go only to the logged message.
#[derive(Debug, Clone, Copy)]
enum Rule {
    RecipientAllowlist,
    RecipientDenylist,
    AssetAllowlist,
    AmountCap,
    Signers,
    Code,
}

impl Rule {
    fn deny(self, logged: String) -> Decision {
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

impl Policy {
    fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        if let Some(signers) = &self.signers {
            let after = request
                .after
                .as_ref()
                .ok_or_else(|| ModuleError::InvalidRequest("request has no account view".into()))?;
            if let Some(signer) = self
                .checked_signers(request.before.as_ref(), after)
                .into_iter()
                .find(|signer| !signers.permits(signer))
            {
                return Ok(Rule::Signers.deny(format!("signer {signer} is not permitted")));
            }
        }
        if let Some(allowed) = &self.code
            && let Some(code) = self.checked_code(request)?
            && !allowed.contains(&code.to_ascii_lowercase())
        {
            return Ok(Rule::Code.deny(format!("code {code} is not allowed")));
        }
        match &request.subject {
            Some(hook_request::Subject::Transaction(tx)) => self.evaluate_transaction(tx),
            Some(hook_request::Subject::Registration(_)) => Ok(Decision::allow()),
            None => Err(ModuleError::InvalidRequest("request has no subject".into())),
        }
    }

    /// The code commitment `allowed_code_commitments` must list for this request, if any: under
    /// `changed` the code an account registers with or a transaction replaces its code with,
    /// under `all` the code the account runs after the request, at every hook.
    fn checked_code(&self, request: &HookRequest) -> Result<Option<String>, ModuleError> {
        let after_code = || {
            request
                .after
                .as_ref()
                .map(|after| after.code_commitment.clone())
                .ok_or_else(|| ModuleError::InvalidRequest("request has no account view".into()))
        };
        match (self.code_check, &request.subject) {
            (CodeCheck::All, _)
            | (CodeCheck::Changed, Some(hook_request::Subject::Registration(_))) => {
                after_code().map(Some)
            }
            (CodeCheck::Changed, Some(hook_request::Subject::Transaction(tx))) => Ok(tx
                .miden()
                .and_then(|projection| projection.new_code_commitment.clone())),
            (CodeCheck::Changed, None) => Ok(None),
        }
    }

    fn checked_signers(&self, before: Option<&AccountView>, after: &AccountView) -> Vec<String> {
        let existing: BTreeSet<String> = match (self.signer_check, before) {
            (SignerCheck::Added, Some(before)) => normalized(&before.signer_commitments),
            _ => BTreeSet::new(),
        };
        normalized(&after.signer_commitments)
            .into_iter()
            .filter(|signer| !existing.contains(signer))
            .collect()
    }

    fn evaluate_transaction(&self, tx: &TransactionSubject) -> Result<Decision, ModuleError> {
        let Some(projection) = tx.miden() else {
            return Err(ModuleError::InvalidRequest(
                "transaction has no Miden projection to judge".into(),
            ));
        };
        Ok(self.judge(projection))
    }

    fn judge(&self, projection: &MidenTransaction) -> Decision {
        let payments = || projection.output_notes.iter().filter(|note| !note.tx_fee);

        if let Some(allowed) = &self.assets {
            let sent = payments().flat_map(|n| n.assets.iter());
            let received = projection.input_notes.iter().flat_map(|n| n.assets.iter());
            if let Some(faucet) = sent
                .chain(received)
                .map(|a| a.faucet_id.to_ascii_lowercase())
                .find(|faucet| !allowed.contains(faucet))
            {
                return Rule::AssetAllowlist.deny(format!("asset {faucet} is not allowed"));
            }
        }

        if let Some(allowed) = &self.recipients {
            for note in payments() {
                let recipient = match (&note.p2id_target, note.partial) {
                    (Some(target), false) => target.to_ascii_lowercase(),
                    _ => {
                        return Rule::RecipientAllowlist
                            .deny(format!("note {} has no checkable recipient", note.note_id));
                    }
                };
                if !allowed.contains(&recipient) {
                    return Rule::RecipientAllowlist
                        .deny(format!("recipient {recipient} is not allowed"));
                }
            }
        }

        if let Some(blocked) = &self.blocked_recipients {
            for note in payments() {
                match (&note.p2id_target, note.partial) {
                    (Some(target), false) if blocked.contains(&target.to_ascii_lowercase()) => {
                        return Rule::RecipientDenylist
                            .deny(format!("recipient {target} is blocked"));
                    }
                    (Some(_), false) => {}
                    _ if self.deny_unknown_recipients => {
                        return Rule::RecipientDenylist
                            .deny(format!("note {} has no checkable recipient", note.note_id));
                    }
                    _ => {}
                }
            }
        }

        if !self.caps.is_empty() {
            let mut sent: BTreeMap<String, u128> = BTreeMap::new();
            for asset in payments().flat_map(|n| n.assets.iter()) {
                if let Some(asset::Value::FungibleAmount(amount)) = &asset.value {
                    let amount = amount.parse::<u128>().unwrap_or(u128::MAX);
                    let total = sent
                        .entry(asset.faucet_id.to_ascii_lowercase())
                        .or_default();
                    *total = total.saturating_add(amount);
                }
            }
            for (faucet, cap) in &self.caps {
                if sent.get(faucet).is_some_and(|total| total > cap) {
                    return Rule::AmountCap
                        .deny(format!("sent {} of {faucet}, cap {cap}", sent[faucet]));
                }
            }
        }

        Decision::allow()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BasicPolicy;

#[async_trait]
impl Module for BasicPolicy {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(ModuleInfo {
            name: KIND.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: vec![
                Hook::BeforeTransactionApproved as i32,
                Hook::BeforeProposalCreated as i32,
                Hook::BeforeAccountRegistered as i32,
            ],
            ..Default::default()
        })
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(match parse_policy(params) {
            Ok(_) => ParamsVerdict::ok(),
            Err(message) => ParamsVerdict::invalid(message),
        })
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        parse_policy(&request.params)
            .map_err(ModuleError::InvalidRequest)?
            .evaluate(request)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        Asset, MidenTransaction, OutputNote, RegistrationSubject, TransactionProjection,
        VaultChange,
    };

    const USDC: &str = "0xaa00";
    const ETH: &str = "0xbb00";

    fn fungible(faucet: &str, amount: u64) -> Asset {
        Asset {
            faucet_id: faucet.into(),
            value: Some(asset::Value::FungibleAmount(amount.to_string())),
        }
    }

    fn p2id(target: &str, assets: Vec<Asset>) -> OutputNote {
        OutputNote {
            note_id: format!("0xnote{target}"),
            assets,
            p2id_target: Some(target.into()),
            ..Default::default()
        }
    }

    fn view(signers: &[&str]) -> AccountView {
        AccountView {
            signer_commitments: signers.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn tx_request(params: serde_json::Value, projection: MidenTransaction) -> HookRequest {
        HookRequest {
            params: serde_json::to_vec(&params).unwrap(),
            before: Some(view(&["0x51"])),
            after: Some(view(&["0x51"])),
            subject: Some(hook_request::Subject::Transaction(TransactionSubject {
                projection: Some(TransactionProjection::miden(projection)),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    async fn decide(request: &HookRequest) -> Decision {
        BasicPolicy.evaluate(request).await.unwrap()
    }

    fn cap() -> serde_json::Value {
        json!({ "max_amount_per_tx": { USDC: "1000" } })
    }

    #[tokio::test]
    async fn allows_a_payment_under_the_cap() {
        let projection = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(USDC, 500)])],
            ..Default::default()
        };
        assert!(decide(&tx_request(cap(), projection)).await.allows());
    }

    #[tokio::test]
    async fn sums_split_payments_against_the_cap() {
        let projection = MidenTransaction {
            output_notes: vec![
                p2id("0xa1", vec![fungible(USDC, 600)]),
                p2id("0xa2", vec![fungible(USDC, 600)]),
            ],
            ..Default::default()
        };
        let decision = decide(&tx_request(cap(), projection)).await;
        assert!(!decision.allows());
        assert_eq!(decision.reason_code, "amount_cap_exceeded");
    }

    #[tokio::test]
    async fn incoming_amounts_do_not_offset_outgoing() {
        let projection = MidenTransaction {
            vault_changes: vec![VaultChange {
                asset: Some(fungible(USDC, 500)),
                direction: crate::Direction::Out as i32,
            }],
            input_notes: vec![crate::InputNote {
                note_id: "0x14".into(),
                assets: vec![fungible(USDC, 1000)],
            }],
            output_notes: vec![p2id("0xa1", vec![fungible(USDC, 1500)])],
            ..Default::default()
        };
        assert!(!decide(&tx_request(cap(), projection)).await.allows());
    }

    #[tokio::test]
    async fn uncapped_assets_and_nfts_are_not_counted() {
        let nft = Asset {
            faucet_id: USDC.into(),
            value: Some(asset::Value::NftId("0x4f7".into())),
        };
        let projection = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(ETH, 5000), nft])],
            ..Default::default()
        };
        assert!(decide(&tx_request(cap(), projection)).await.allows());
    }

    fn fee_note(amount: u64) -> OutputNote {
        OutputNote {
            note_id: "0xfee".into(),
            assets: vec![fungible(USDC, amount)],
            tx_fee: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_fee_note_is_not_a_payment() {
        let projection = MidenTransaction {
            output_notes: vec![fee_note(5000), p2id("0xa1", vec![fungible(USDC, 500)])],
            ..Default::default()
        };
        assert!(
            decide(&tx_request(cap(), projection.clone()))
                .await
                .allows()
        );

        let params = json!({ "recipient_allowlist": ["0xa1"], "asset_allowlist": [ETH, USDC] });
        assert!(decide(&tx_request(params, projection)).await.allows());

        let only_fee = MidenTransaction {
            output_notes: vec![fee_note(5000)],
            ..Default::default()
        };
        assert!(
            decide(&tx_request(json!({ "asset_allowlist": [ETH] }), only_fee))
                .await
                .allows()
        );
    }

    #[tokio::test]
    async fn recipient_allowlist_denies_partial_and_unlisted_notes() {
        let params = json!({ "recipient_allowlist": ["0xa1"] });
        let listed = MidenTransaction {
            output_notes: vec![p2id("0xA1", vec![fungible(USDC, 1)])],
            ..Default::default()
        };
        assert!(decide(&tx_request(params.clone(), listed)).await.allows());

        let mut partial_note = p2id("0xa1", vec![fungible(USDC, 1)]);
        partial_note.partial = true;
        let partial = MidenTransaction {
            output_notes: vec![partial_note],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(params.clone(), partial))
                .await
                .reason_code,
            "recipient_not_allowed"
        );

        let unlisted = MidenTransaction {
            output_notes: vec![p2id("0xa9", vec![fungible(USDC, 1)])],
            ..Default::default()
        };
        assert!(!decide(&tx_request(params, unlisted)).await.allows());
    }

    #[tokio::test]
    async fn asset_allowlist_covers_sent_and_received_assets() {
        let params = json!({ "asset_allowlist": [USDC] });
        let projection = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(ETH, 1)])],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(params, projection)).await.reason_code,
            "asset_not_allowed"
        );
    }

    #[tokio::test]
    async fn recipient_denylist_blocks_listed_recipients_only() {
        let params = json!({ "recipient_denylist": ["0xBAD"] });
        let blocked = MidenTransaction {
            output_notes: vec![fee_note(1), p2id("0xbad", vec![fungible(USDC, 1)])],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(params.clone(), blocked))
                .await
                .reason_code,
            "recipient_blocked"
        );

        let mut partial = p2id("0xa1", vec![fungible(USDC, 1)]);
        partial.partial = true;
        let unknown = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(USDC, 1)]), partial],
            ..Default::default()
        };
        assert!(decide(&tx_request(params, unknown.clone())).await.allows());

        let strict = json!({ "recipient_denylist": ["0xbad"], "deny_unknown_recipients": true });
        assert_eq!(
            decide(&tx_request(strict, unknown)).await.reason_code,
            "recipient_blocked",
            "an unknown recipient refused by the denylist rule reports that rule"
        );
    }

    #[tokio::test]
    async fn a_denial_names_its_rule_to_the_client_and_discloses_no_param() {
        let projection = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(USDC, 5_000)])],
            ..Default::default()
        };
        let decision = decide(&tx_request(cap(), projection)).await;
        assert_eq!(decision.reason_code, "amount_cap_exceeded");
        assert!(!decision.client_message.is_empty());
        assert!(decision.client_message.len() <= 256);
        for detail in ["1000", "5000", USDC, "0xa1"] {
            assert!(
                !decision.client_message.contains(detail),
                "{detail} leaked into {:?}",
                decision.client_message
            );
        }
        assert!(
            decision.message.contains("1000"),
            "details stay in the logged message"
        );
    }

    const REVIEWED: &str = "0xc0de";
    const UNREVIEWED: &str = "0xbad";

    fn code_rule(code_check: &str) -> serde_json::Value {
        json!({ "allowed_code_commitments": [REVIEWED], "code_check": code_check })
    }

    fn running(mut request: HookRequest, code: &str) -> HookRequest {
        if let Some(after) = request.after.as_mut() {
            after.code_commitment = code.into();
        }
        request
    }

    fn registration(params: serde_json::Value, code: &str) -> HookRequest {
        running(
            HookRequest {
                params: serde_json::to_vec(&params).unwrap(),
                after: Some(view(&["0x51"])),
                subject: Some(hook_request::Subject::Registration(
                    RegistrationSubject::default(),
                )),
                ..Default::default()
            },
            code,
        )
    }

    fn replacing_code_with(code: Option<&str>) -> MidenTransaction {
        MidenTransaction {
            new_code_commitment: code.map(str::to_string),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn changed_code_check_judges_registration_and_code_replacement_only() {
        let rule = code_rule("changed");
        assert!(decide(&registration(rule.clone(), REVIEWED)).await.allows());
        let refused = decide(&registration(rule.clone(), UNREVIEWED)).await;
        assert_eq!(refused.reason_code, "code_not_allowed");
        assert!(!refused.client_message.contains(UNREVIEWED));

        let unchanged = running(
            tx_request(rule.clone(), replacing_code_with(None)),
            UNREVIEWED,
        );
        assert!(
            decide(&unchanged).await.allows(),
            "code an account already runs is kept until it changes"
        );
        let upgrade = tx_request(rule.clone(), replacing_code_with(Some(UNREVIEWED)));
        assert_eq!(decide(&upgrade).await.reason_code, "code_not_allowed");
        let reviewed = tx_request(rule, replacing_code_with(Some(REVIEWED)));
        assert!(decide(&reviewed).await.allows());
    }

    #[tokio::test]
    async fn all_code_check_judges_the_code_an_account_runs_at_every_hook() {
        let rule = code_rule("all");
        let unchanged = running(
            tx_request(rule.clone(), replacing_code_with(None)),
            UNREVIEWED,
        );
        assert_eq!(decide(&unchanged).await.reason_code, "code_not_allowed");
        let upgraded = running(
            tx_request(rule, replacing_code_with(Some(REVIEWED))),
            REVIEWED,
        );
        assert!(
            decide(&upgraded).await.allows(),
            "an account on unlisted code can still upgrade to listed code"
        );
    }

    #[tokio::test]
    async fn an_unknown_code_check_is_refused_when_the_params_are_written() {
        let verdict = BasicPolicy
            .validate_params(
                &serde_json::to_vec(&json!({
                    "allowed_code_commitments": [REVIEWED],
                    "code_check": "sometimes"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(!verdict.ok);
    }

    #[tokio::test]
    async fn a_transaction_without_a_miden_projection_is_not_judged() {
        let mut request = tx_request(cap(), MidenTransaction::default());
        if let Some(hook_request::Subject::Transaction(subject)) = &mut request.subject {
            subject.projection = None;
        }
        assert!(matches!(
            BasicPolicy.evaluate(&request).await,
            Err(ModuleError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn asset_allowlist_covers_consumed_notes() {
        let projection = MidenTransaction {
            input_notes: vec![crate::InputNote {
                note_id: "0x14".into(),
                assets: vec![fungible(ETH, 1)],
            }],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(
                json!({ "asset_allowlist": [USDC] }),
                projection
            ))
            .await
            .reason_code,
            "asset_not_allowed"
        );
    }

    #[tokio::test]
    async fn forged_metadata_does_not_change_the_decision() {
        let projection = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(USDC, 5000)])],
            ..Default::default()
        };
        let mut request = tx_request(cap(), projection);
        if let Some(hook_request::Subject::Transaction(tx)) = &mut request.subject {
            tx.unverified_metadata =
                serde_json::to_vec(&json!({ "proposal_type": "change_threshold", "amount": "1" }))
                    .unwrap();
        }
        assert!(!decide(&request).await.allows());
    }

    fn signer_request(
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

    #[tokio::test]
    async fn added_mode_checks_only_new_signers() {
        let params = json!({ "signer_allowlist": ["0x52"] });
        let request = signer_request(params.clone(), Some(&["0x51"]), &["0x51", "0x52"]);
        assert!(decide(&request).await.allows());

        let request = signer_request(params, Some(&["0x51"]), &["0x51", "0x53"]);
        assert_eq!(decide(&request).await.reason_code, "signer_not_allowed");
    }

    #[tokio::test]
    async fn all_mode_checks_every_signer() {
        let params = json!({ "signer_check": "all", "signer_allowlist": ["0x52"] });
        let request = signer_request(params, Some(&["0x51"]), &["0x51", "0x52"]);
        assert!(!decide(&request).await.allows());
    }

    #[tokio::test]
    async fn registration_checks_every_initial_signer() {
        let params = json!({ "signer_denylist": ["0xbad"] });
        let request = signer_request(params, None, &["0x51", "0xBAD"]);
        assert!(!decide(&request).await.allows());
    }

    #[tokio::test]
    async fn missing_account_view_is_an_error_not_an_allow() {
        let mut request = signer_request(json!({ "signer_allowlist": ["0x51"] }), None, &[]);
        request.after = None;
        assert!(BasicPolicy.evaluate(&request).await.is_err());
    }

    #[tokio::test]
    async fn validate_params_rejects_bad_input() {
        for params in [
            json!({ "recipient_denylist": ["mtst1qrkd5x7qy0mw0yqzlvqlqd3w9cafjkw"] }),
            json!({ "max_amount_per_tx": { "faucet": "10" } }),
            json!({ "signer_denylist": ["0x"] }),
            json!({ "unknown": 1 }),
            json!({ "max_amount_per_tx": { USDC: "ten" } }),
            json!({ "signer_allowlist": [], "signer_denylist": [] }),
            json!({ "max_amount_per_tx": { "0xREPLACE_WITH_FAUCET_ID": "5" } }),
            json!({ "max_amount_per_tx": { "0xAA": "5", "0xaa": "9" } }),
            json!({ "deny_unknown_recipients": true }),
            json!({ "signer_check": "all" }),
            json!({ "code_check": "all" }),
        ] {
            let verdict = BasicPolicy
                .validate_params(&serde_json::to_vec(&params).unwrap())
                .await
                .unwrap();
            assert!(!verdict.ok, "{params}");
        }
    }

    #[tokio::test]
    async fn evaluating_twice_gives_the_same_decision() {
        let projection = MidenTransaction {
            output_notes: vec![p2id("0xa1", vec![fungible(USDC, 900)])],
            ..Default::default()
        };
        let request = tx_request(cap(), projection);
        assert_eq!(decide(&request).await, decide(&request).await);
    }
}
