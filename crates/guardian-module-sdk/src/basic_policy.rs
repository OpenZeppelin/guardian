//! Reference policy: per-asset amount cap, asset and recipient allowlists, signer lists.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::Deserialize;

use crate::proto::{asset, hook_request};
use crate::{
    AccountView, Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo,
    PROTOCOL_VERSION, ParamsVerdict, TransactionSubject,
};

pub const KIND: &str = "basic-policy";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerCheck {
    #[default]
    Added,
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
    pub signer_check: SignerCheck,
}

struct Policy {
    caps: BTreeMap<String, u128>,
    assets: Option<BTreeSet<String>>,
    recipients: Option<BTreeSet<String>>,
    blocked_recipients: Option<BTreeSet<String>>,
    deny_unknown_recipients: bool,
    signers: Option<SignerList>,
    signer_check: SignerCheck,
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

impl TryFrom<Params> for Policy {
    type Error = String;

    fn try_from(params: Params) -> Result<Self, String> {
        let caps = params
            .max_amount_per_tx
            .into_iter()
            .map(|(faucet, amount)| {
                amount
                    .parse::<u128>()
                    .map(|amount| (faucet.to_ascii_lowercase(), amount))
                    .map_err(|_| format!("max_amount_per_tx for {faucet} is not a decimal amount"))
            })
            .collect::<Result<_, _>>()?;
        let signers = match (params.signer_allowlist, params.signer_denylist) {
            (Some(_), Some(_)) => {
                return Err("signer_allowlist and signer_denylist are mutually exclusive".into());
            }
            (Some(list), None) => Some(SignerList::Allow(normalized(&list))),
            (None, Some(list)) => Some(SignerList::Deny(normalized(&list))),
            (None, None) => None,
        };
        Ok(Self {
            caps,
            assets: params.asset_allowlist.as_deref().map(normalized),
            recipients: params.recipient_allowlist.as_deref().map(normalized),
            blocked_recipients: params.recipient_denylist.as_deref().map(normalized),
            deny_unknown_recipients: params.deny_unknown_recipients,
            signers,
            signer_check: params.signer_check,
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
                return Ok(Decision::deny(
                    "signer_not_allowed",
                    format!("signer {signer} is not permitted"),
                ));
            }
        }
        match &request.subject {
            Some(hook_request::Subject::Transaction(tx)) => Ok(self.evaluate_transaction(tx)),
            Some(hook_request::Subject::Registration(_)) => Ok(Decision::allow()),
            None => Err(ModuleError::InvalidRequest("request has no subject".into())),
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

    fn evaluate_transaction(&self, tx: &TransactionSubject) -> Decision {
        let Some(projection) = &tx.projection else {
            return Decision::deny("projection_missing", "transaction has no projection");
        };

        let payments = || projection.output_notes.iter().filter(|note| !note.tx_fee);

        if let Some(allowed) = &self.assets {
            let sent = payments().flat_map(|n| n.assets.iter());
            let received = projection.input_notes.iter().flat_map(|n| n.assets.iter());
            if let Some(faucet) = sent
                .chain(received)
                .map(|a| a.faucet_id.to_ascii_lowercase())
                .find(|faucet| !allowed.contains(faucet))
            {
                return Decision::deny(
                    "asset_not_allowed",
                    format!("asset {faucet} is not allowed"),
                );
            }
        }

        if let Some(allowed) = &self.recipients {
            for note in payments() {
                let recipient = match (&note.p2id_target, note.partial) {
                    (Some(target), false) => target.to_ascii_lowercase(),
                    _ => {
                        return Decision::deny(
                            "recipient_not_allowed",
                            format!("note {} has no checkable recipient", note.note_id),
                        );
                    }
                };
                if !allowed.contains(&recipient) {
                    return Decision::deny(
                        "recipient_not_allowed",
                        format!("recipient {recipient} is not allowed"),
                    );
                }
            }
        }

        if let Some(blocked) = &self.blocked_recipients {
            for note in payments() {
                match (&note.p2id_target, note.partial) {
                    (Some(target), false) if blocked.contains(&target.to_ascii_lowercase()) => {
                        return Decision::deny(
                            "recipient_blocked",
                            format!("recipient {target} is blocked"),
                        );
                    }
                    (Some(_), false) => {}
                    _ if self.deny_unknown_recipients => {
                        return Decision::deny(
                            "recipient_not_allowed",
                            format!("note {} has no checkable recipient", note.note_id),
                        );
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
                    return Decision::deny(
                        "amount_cap_exceeded",
                        format!("sent {} of {faucet}, cap {cap}", sent[faucet]),
                    );
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
    use crate::{Asset, OutputNote, RegistrationSubject, TxProjection, VaultChange};

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

    fn tx_request(params: serde_json::Value, projection: TxProjection) -> HookRequest {
        HookRequest {
            params: serde_json::to_vec(&params).unwrap(),
            before: Some(view(&["0xs1"])),
            after: Some(view(&["0xs1"])),
            subject: Some(hook_request::Subject::Transaction(TransactionSubject {
                projection: Some(projection),
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
        let projection = TxProjection {
            output_notes: vec![p2id("0xr1", vec![fungible(USDC, 500)])],
            ..Default::default()
        };
        assert!(decide(&tx_request(cap(), projection)).await.allow);
    }

    #[tokio::test]
    async fn sums_split_payments_against_the_cap() {
        let projection = TxProjection {
            output_notes: vec![
                p2id("0xr1", vec![fungible(USDC, 600)]),
                p2id("0xr2", vec![fungible(USDC, 600)]),
            ],
            ..Default::default()
        };
        let decision = decide(&tx_request(cap(), projection)).await;
        assert!(!decision.allow);
        assert_eq!(decision.reason_code, "amount_cap_exceeded");
    }

    #[tokio::test]
    async fn incoming_amounts_do_not_offset_outgoing() {
        let projection = TxProjection {
            vault_changes: vec![VaultChange {
                asset: Some(fungible(USDC, 500)),
                direction: crate::Direction::Out as i32,
            }],
            input_notes: vec![crate::InputNote {
                note_id: "0xin".into(),
                assets: vec![fungible(USDC, 1000)],
            }],
            output_notes: vec![p2id("0xr1", vec![fungible(USDC, 1500)])],
            ..Default::default()
        };
        assert!(!decide(&tx_request(cap(), projection)).await.allow);
    }

    #[tokio::test]
    async fn uncapped_assets_and_nfts_are_not_counted() {
        let nft = Asset {
            faucet_id: USDC.into(),
            value: Some(asset::Value::NftId("0xnft".into())),
        };
        let projection = TxProjection {
            output_notes: vec![p2id("0xr1", vec![fungible(ETH, 5000), nft])],
            ..Default::default()
        };
        assert!(decide(&tx_request(cap(), projection)).await.allow);
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
        let projection = TxProjection {
            output_notes: vec![fee_note(5000), p2id("0xr1", vec![fungible(USDC, 500)])],
            ..Default::default()
        };
        assert!(decide(&tx_request(cap(), projection.clone())).await.allow);

        let params = json!({ "recipient_allowlist": ["0xr1"], "asset_allowlist": [ETH, USDC] });
        assert!(decide(&tx_request(params, projection)).await.allow);

        let only_fee = TxProjection {
            output_notes: vec![fee_note(5000)],
            ..Default::default()
        };
        assert!(
            decide(&tx_request(json!({ "asset_allowlist": [ETH] }), only_fee))
                .await
                .allow
        );
    }

    #[tokio::test]
    async fn recipient_allowlist_denies_partial_and_unlisted_notes() {
        let params = json!({ "recipient_allowlist": ["0xr1"] });
        let listed = TxProjection {
            output_notes: vec![p2id("0xR1", vec![fungible(USDC, 1)])],
            ..Default::default()
        };
        assert!(decide(&tx_request(params.clone(), listed)).await.allow);

        let mut partial_note = p2id("0xr1", vec![fungible(USDC, 1)]);
        partial_note.partial = true;
        let partial = TxProjection {
            output_notes: vec![partial_note],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(params.clone(), partial))
                .await
                .reason_code,
            "recipient_not_allowed"
        );

        let unlisted = TxProjection {
            output_notes: vec![p2id("0xr9", vec![fungible(USDC, 1)])],
            ..Default::default()
        };
        assert!(!decide(&tx_request(params, unlisted)).await.allow);
    }

    #[tokio::test]
    async fn asset_allowlist_covers_sent_and_received_assets() {
        let params = json!({ "asset_allowlist": [USDC] });
        let projection = TxProjection {
            output_notes: vec![p2id("0xr1", vec![fungible(ETH, 1)])],
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
        let blocked = TxProjection {
            output_notes: vec![fee_note(1), p2id("0xbad", vec![fungible(USDC, 1)])],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(params.clone(), blocked))
                .await
                .reason_code,
            "recipient_blocked"
        );

        let mut partial = p2id("0xr1", vec![fungible(USDC, 1)]);
        partial.partial = true;
        let unknown = TxProjection {
            output_notes: vec![p2id("0xr1", vec![fungible(USDC, 1)]), partial],
            ..Default::default()
        };
        assert!(decide(&tx_request(params, unknown.clone())).await.allow);

        let strict = json!({ "recipient_denylist": ["0xbad"], "deny_unknown_recipients": true });
        assert_eq!(
            decide(&tx_request(strict, unknown)).await.reason_code,
            "recipient_not_allowed"
        );
    }

    #[tokio::test]
    async fn asset_allowlist_covers_consumed_notes() {
        let projection = TxProjection {
            input_notes: vec![crate::InputNote {
                note_id: "0xin".into(),
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
        let projection = TxProjection {
            output_notes: vec![p2id("0xr1", vec![fungible(USDC, 5000)])],
            ..Default::default()
        };
        let mut request = tx_request(cap(), projection);
        if let Some(hook_request::Subject::Transaction(tx)) = &mut request.subject {
            tx.unverified_metadata =
                serde_json::to_vec(&json!({ "proposal_type": "change_threshold", "amount": "1" }))
                    .unwrap();
        }
        assert!(!decide(&request).await.allow);
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
        let params = json!({ "signer_allowlist": ["0xs2"] });
        let request = signer_request(params.clone(), Some(&["0xs1"]), &["0xs1", "0xs2"]);
        assert!(decide(&request).await.allow);

        let request = signer_request(params, Some(&["0xs1"]), &["0xs1", "0xs3"]);
        assert_eq!(decide(&request).await.reason_code, "signer_not_allowed");
    }

    #[tokio::test]
    async fn all_mode_checks_every_signer() {
        let params = json!({ "signer_check": "all", "signer_allowlist": ["0xs2"] });
        let request = signer_request(params, Some(&["0xs1"]), &["0xs1", "0xs2"]);
        assert!(!decide(&request).await.allow);
    }

    #[tokio::test]
    async fn registration_checks_every_initial_signer() {
        let params = json!({ "signer_denylist": ["0xbad"] });
        let request = signer_request(params, None, &["0xs1", "0xBAD"]);
        assert!(!decide(&request).await.allow);
    }

    #[tokio::test]
    async fn missing_account_view_is_an_error_not_an_allow() {
        let mut request = signer_request(json!({ "signer_allowlist": ["0xs1"] }), None, &[]);
        request.after = None;
        assert!(BasicPolicy.evaluate(&request).await.is_err());
    }

    #[tokio::test]
    async fn validate_params_rejects_bad_input() {
        for params in [
            json!({ "unknown": 1 }),
            json!({ "max_amount_per_tx": { USDC: "ten" } }),
            json!({ "signer_allowlist": [], "signer_denylist": [] }),
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
        let projection = TxProjection {
            output_notes: vec![p2id("0xr1", vec![fungible(USDC, 900)])],
            ..Default::default()
        };
        let request = tx_request(cap(), projection);
        assert_eq!(decide(&request).await, decide(&request).await);
    }
}
