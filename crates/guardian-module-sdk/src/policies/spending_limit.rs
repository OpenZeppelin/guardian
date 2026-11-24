//! `spending-limit`: which assets a transaction may move, and how much of each it may send.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::Deserialize;

use super::{Accounts, IdKind, Rule};
use crate::proto::asset;
use crate::{
    Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo, ParamsVerdict,
};

pub const KIND: &str = "spending-limit";

const HOOKS: [Hook; 2] = [Hook::BeforeProposalCreated, Hook::BeforeTransactionApproved];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// The most a transaction may send of each listed asset, faucet id to a decimal amount. The
    /// payment notes of one transaction are summed per asset; the fee note does not count, and
    /// an asset that is not listed is not capped.
    #[serde(default)]
    pub max_per_transaction: BTreeMap<String, String>,
    /// The assets a transaction may send in payment notes or receive from the notes it
    /// consumes. Unset, any asset; empty, none.
    #[serde(default)]
    pub allowed_assets: Option<Vec<String>>,
}

struct Rules {
    caps: BTreeMap<String, u128>,
    assets: Option<BTreeSet<String>>,
}

impl TryFrom<Params> for Rules {
    type Error = String;

    fn try_from(params: Params) -> Result<Self, String> {
        let mut caps = BTreeMap::new();
        for (faucet, amount) in params.max_per_transaction {
            let faucet = super::id("max_per_transaction", IdKind::Account, &faucet)?;
            let amount = amount
                .parse::<u128>()
                .map_err(|_| format!("max_per_transaction for {faucet} is not a decimal amount"))?;
            if caps.insert(faucet.clone(), amount).is_some() {
                return Err(format!(
                    "max_per_transaction names {faucet} more than once (ids are compared in lowercase)"
                ));
            }
        }
        Ok(Self {
            caps,
            assets: params
                .allowed_assets
                .as_deref()
                .map(|list| super::ids("allowed_assets", IdKind::Account, list))
                .transpose()?,
        })
    }
}

impl Rules {
    fn parse(params: &[u8]) -> Result<Self, String> {
        super::parse::<Params>(params).and_then(Self::try_from)
    }

    fn judge(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        if self.caps.is_empty() && self.assets.is_none() {
            return Ok(Decision::allow());
        }
        let projection = super::transaction(request)?;
        let payments = || projection.output_notes.iter().filter(|note| !note.tx_fee);

        if let Some(allowed) = &self.assets {
            let sent = payments().flat_map(|n| n.assets.iter());
            let received = projection.input_notes.iter().flat_map(|n| n.assets.iter());
            if let Some(faucet) = sent
                .chain(received)
                .map(|a| a.faucet_id.to_ascii_lowercase())
                .find(|faucet| !allowed.contains(faucet))
            {
                return Ok(Rule::AssetAllowlist.deny(format!("asset {faucet} is not allowed")));
            }
        }

        if !self.caps.is_empty() {
            let mut sent: BTreeMap<String, u128> = BTreeMap::new();
            for asset in payments().flat_map(|n| n.assets.iter()) {
                let amount = match &asset.value {
                    Some(asset::Value::FungibleAmount(amount)) => {
                        amount.parse::<u128>().unwrap_or(u128::MAX)
                    }
                    Some(asset::Value::NftId(_)) => continue,
                    None if !self
                        .caps
                        .contains_key(&asset.faucet_id.to_ascii_lowercase()) =>
                    {
                        continue;
                    }
                    None => {
                        return Err(ModuleError::InvalidRequest(format!(
                            "asset of {} has no value",
                            asset.faucet_id
                        )));
                    }
                };
                let total = sent
                    .entry(asset.faucet_id.to_ascii_lowercase())
                    .or_default();
                *total = total.saturating_add(amount);
            }
            for (faucet, cap) in &self.caps {
                if sent.get(faucet).is_some_and(|total| total > cap) {
                    return Ok(Rule::AmountCap
                        .deny(format!("sent {} of {faucet}, cap {cap}", sent[faucet])));
                }
            }
        }

        Ok(Decision::allow())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SpendingLimit;

#[async_trait]
impl Module for SpendingLimit {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(super::info(KIND, &HOOKS))
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(super::verdict(
            params,
            Rules::parse(params),
            &[
                Accounts::Keys("max_per_transaction"),
                Accounts::List("allowed_assets"),
            ],
        ))
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        Rules::parse(&request.params)
            .map_err(ModuleError::InvalidRequest)?
            .judge(request)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::testing::*;
    use super::*;
    use crate::proto::hook_request;
    use crate::{Asset, InputNote, MidenTransaction, VaultChange};

    async fn decide(request: &HookRequest) -> Decision {
        SpendingLimit.evaluate(request).await.unwrap()
    }

    fn cap() -> serde_json::Value {
        json!({ "max_per_transaction": { USDC: "1000" } })
    }

    fn paying(notes: Vec<crate::OutputNote>) -> MidenTransaction {
        MidenTransaction {
            output_notes: notes,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn allows_a_payment_under_the_cap() {
        let projection = paying(vec![p2id(RECIPIENT, vec![fungible(USDC, 500)])]);
        assert!(decide(&tx_request(cap(), projection)).await.allows());
    }

    #[tokio::test]
    async fn sums_split_payments_against_the_cap() {
        let projection = paying(vec![
            p2id(RECIPIENT, vec![fungible(USDC, 600)]),
            p2id(
                "0x0000000000000000000000000000a2",
                vec![fungible(USDC, 600)],
            ),
        ]);
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
            input_notes: vec![InputNote {
                note_id: "0x14".into(),
                assets: vec![fungible(USDC, 1000)],
            }],
            output_notes: vec![p2id(RECIPIENT, vec![fungible(USDC, 1500)])],
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
        let projection = paying(vec![p2id(RECIPIENT, vec![fungible(ETH, 5000), nft])]);
        assert!(decide(&tx_request(cap(), projection)).await.allows());
    }

    #[tokio::test]
    async fn an_asset_without_a_value_is_an_error_not_an_allow() {
        let unset = Asset {
            faucet_id: USDC.into(),
            value: None,
        };
        let projection = paying(vec![p2id(RECIPIENT, vec![unset])]);
        assert!(matches!(
            SpendingLimit.evaluate(&tx_request(cap(), projection)).await,
            Err(ModuleError::InvalidRequest(_))
        ));
        let uncapped = Asset {
            faucet_id: ETH.into(),
            value: None,
        };
        let projection = paying(vec![p2id(RECIPIENT, vec![uncapped])]);
        assert!(
            decide(&tx_request(cap(), projection)).await.allows(),
            "an uncapped faucet's asset is not counted, value or not"
        );
    }

    #[tokio::test]
    async fn the_fee_note_is_not_a_payment() {
        let projection = paying(vec![
            fee_note(5000),
            p2id(RECIPIENT, vec![fungible(USDC, 500)]),
        ]);
        assert!(decide(&tx_request(cap(), projection)).await.allows());
        let only_fee = paying(vec![fee_note(5000)]);
        assert!(
            decide(&tx_request(json!({ "allowed_assets": [ETH] }), only_fee))
                .await
                .allows()
        );
    }

    #[tokio::test]
    async fn allowed_assets_cover_sent_and_received_assets() {
        let sending = paying(vec![p2id(RECIPIENT, vec![fungible(ETH, 1)])]);
        assert_eq!(
            decide(&tx_request(json!({ "allowed_assets": [USDC] }), sending))
                .await
                .reason_code,
            "asset_not_allowed"
        );
        let consuming = MidenTransaction {
            input_notes: vec![InputNote {
                note_id: "0x14".into(),
                assets: vec![fungible(ETH, 1)],
            }],
            ..Default::default()
        };
        assert_eq!(
            decide(&tx_request(json!({ "allowed_assets": [USDC] }), consuming))
                .await
                .reason_code,
            "asset_not_allowed"
        );
    }

    #[tokio::test]
    async fn a_denial_names_its_rule_to_the_client_and_discloses_no_param() {
        let projection = paying(vec![p2id(RECIPIENT, vec![fungible(USDC, 5_000)])]);
        let decision = decide(&tx_request(cap(), projection)).await;
        assert_eq!(decision.reason_code, "amount_cap_exceeded");
        assert!(!decision.client_message.is_empty());
        assert!(decision.client_message.len() <= 256);
        for detail in ["1000", "5000", USDC, RECIPIENT] {
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

    #[tokio::test]
    async fn forged_metadata_does_not_change_the_decision() {
        let projection = paying(vec![p2id(RECIPIENT, vec![fungible(USDC, 5000)])]);
        let mut request = tx_request(cap(), projection);
        if let Some(hook_request::Subject::Transaction(tx)) = &mut request.subject {
            tx.unverified_metadata =
                serde_json::to_vec(&json!({ "proposal_type": "change_threshold", "amount": "1" }))
                    .unwrap();
        }
        assert!(!decide(&request).await.allows());
    }

    #[tokio::test]
    async fn an_instance_with_no_rule_allows_without_reading_the_request() {
        let unprojected = without_projection(tx_request(json!({}), MidenTransaction::default()));
        assert!(decide(&unprojected).await.allows());
    }

    #[tokio::test]
    async fn a_request_it_cannot_judge_is_an_error_not_an_allow() {
        let unprojected = without_projection(tx_request(cap(), MidenTransaction::default()));
        let registration = registration_request(cap(), None, &[SIGNER]);
        for request in [unprojected, registration] {
            assert!(matches!(
                SpendingLimit.evaluate(&request).await,
                Err(ModuleError::InvalidRequest(_))
            ));
        }
    }

    #[tokio::test]
    async fn describes_only_the_transaction_hooks() {
        let info = SpendingLimit
            .describe(&DescribeRequest::default())
            .await
            .unwrap();
        assert_eq!(info.name, KIND);
        assert!(!info.hooks.contains(&(Hook::BeforeAccountRegistered as i32)));
    }

    #[tokio::test]
    async fn params_are_validated() {
        for params in [
            json!({ "max_per_transaction": { "faucet": "10" } }),
            json!({ "max_per_transaction": { USDC: "ten" } }),
            json!({ "max_per_transaction": { "0xREPLACE_WITH_FAUCET_ID": "5" } }),
            json!({ "max_per_transaction": { "0x0000000000000000000000000000AA": "5", "0x0000000000000000000000000000aa": "9" } }),
            json!({ "allowed_assets": ["0xa1b2c3"] }),
            json!({ "max_amount_per_tx": { USDC: "5" } }),
            json!({ "denylist": [RECIPIENT] }),
            json!({ "max_per_transaction": { DEVNET_ADDRESS: "5", ADDRESSED: "9" } }),
            json!({ "allowed_assets": [address_of(0, 15)] }),
        ] {
            let verdict = SpendingLimit
                .validate_params(&serde_json::to_vec(&params).unwrap())
                .await
                .unwrap();
            assert!(!verdict.ok, "{params}");
        }
        let verdict = SpendingLimit
            .validate_params(&serde_json::to_vec(&cap()).unwrap())
            .await
            .unwrap();
        assert!(verdict.ok);
    }

    #[tokio::test]
    async fn a_faucet_named_by_its_address_is_stored_and_capped_as_its_hex_id() {
        let named = json!({
            "max_per_transaction": { DEVNET_ADDRESS: "5", USDC: "7" },
            "allowed_assets": [ROUTED_ADDRESS, USDC],
        });
        let verdict = SpendingLimit
            .validate_params(&serde_json::to_vec(&named).unwrap())
            .await
            .unwrap();
        assert!(verdict.ok, "{}", verdict.message);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&verdict.normalized_params).unwrap(),
            json!({
                "max_per_transaction": { ADDRESSED: "5", USDC: "7" },
                "allowed_assets": [ADDRESSED, USDC],
            })
        );

        let over = paying(vec![p2id(RECIPIENT, vec![fungible(ADDRESSED, 6)])]);
        assert_eq!(
            decide(&tx_request(named, over)).await.reason_code,
            "amount_cap_exceeded",
            "params stored before normalization still cap"
        );
    }
}
