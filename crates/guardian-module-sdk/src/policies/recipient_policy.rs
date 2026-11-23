//! `recipient-policy`: who a transaction may pay.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Accounts, IdKind, Rule};
use crate::{
    Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo, ParamsVerdict,
};

pub const KIND: &str = "recipient-policy";

const HOOKS: [Hook; 2] = [Hook::BeforeProposalCreated, Hook::BeforeTransactionApproved];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// The only accounts a payment note may go to. A note whose recipient cannot be identified
    /// (a partial note, or one that is neither P2ID nor P2IDE) is refused, since it cannot be
    /// shown to go to a listed account. Excludes `denylist`.
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
    /// Accounts no payment note may go to. Excludes `allowlist`.
    #[serde(default)]
    pub denylist: Option<Vec<String>>,
    /// With `denylist`: whether a payment note whose recipient cannot be identified is refused.
    /// Unset, it is refused whenever the denylist names anyone, since a blocked recipient could
    /// otherwise be paid through a note the Guardian cannot read.
    #[serde(default)]
    pub deny_unknown: Option<bool>,
}

enum Rules {
    Any,
    Allow(BTreeSet<String>),
    Deny {
        blocked: BTreeSet<String>,
        deny_unknown: bool,
    },
}

impl TryFrom<Params> for Rules {
    type Error = String;

    fn try_from(params: Params) -> Result<Self, String> {
        if params.deny_unknown.is_some() && params.denylist.is_none() {
            return Err("deny_unknown applies to denylist, which is not set (an allowlist refuses unknown recipients already)".into());
        }
        match (params.allowlist, params.denylist) {
            (Some(_), Some(_)) => Err("allowlist and denylist are mutually exclusive".into()),
            (Some(list), None) => Ok(Rules::Allow(super::ids(
                "allowlist",
                IdKind::Account,
                &list,
            )?)),
            (None, Some(list)) => Ok(Rules::Deny {
                deny_unknown: params.deny_unknown.unwrap_or(!list.is_empty()),
                blocked: super::ids("denylist", IdKind::Account, &list)?,
            }),
            (None, None) => Ok(Rules::Any),
        }
    }
}

impl Rules {
    fn parse(params: &[u8]) -> Result<Self, String> {
        super::parse::<Params>(params).and_then(Self::try_from)
    }

    fn judge(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        if matches!(self, Rules::Any) {
            return Ok(Decision::allow());
        }
        let projection = super::transaction(request)?;
        let payer = request.account_id.to_ascii_lowercase();
        let payments = projection.output_notes.iter().filter(|note| !note.tx_fee);
        for note in payments {
            let target = match (&note.p2id_target, note.partial) {
                (Some(target), false) => Some(target.to_ascii_lowercase()),
                _ => None,
            };
            let reclaimer = note
                .reclaimer
                .as_deref()
                .map(str::to_ascii_lowercase)
                .filter(|reclaimer| *reclaimer != payer);
            let recipients = std::iter::once(target).chain(reclaimer.map(Some));
            if let Some(denial) = recipients
                .filter_map(|recipient| self.refusal(&note.note_id, recipient))
                .next()
            {
                return Ok(denial);
            }
        }
        Ok(Decision::allow())
    }

    /// The denial for one account a payment note can reach (its P2ID target, or the reclaimer of
    /// a reclaimable P2IDE note), `None` for a note whose recipient cannot be read.
    fn refusal(&self, note_id: &str, recipient: Option<String>) -> Option<Decision> {
        match (self, recipient) {
            (Rules::Any, _) => None,
            (Rules::Allow(_), None) => Some(
                Rule::RecipientAllowlist.deny(format!("note {note_id} has no checkable recipient")),
            ),
            (Rules::Allow(allowed), Some(recipient)) => {
                (!allowed.contains(&recipient)).then(|| {
                    Rule::RecipientAllowlist.deny(format!("recipient {recipient} is not allowed"))
                })
            }
            (Rules::Deny { deny_unknown, .. }, None) => deny_unknown.then(|| {
                Rule::RecipientDenylist.deny(format!("note {note_id} has no checkable recipient"))
            }),
            (Rules::Deny { blocked, .. }, Some(recipient)) => blocked
                .contains(&recipient)
                .then(|| Rule::RecipientDenylist.deny(format!("recipient {recipient} is blocked"))),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RecipientPolicy;

#[async_trait]
impl Module for RecipientPolicy {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(super::info(KIND, &HOOKS))
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(super::verdict(
            params,
            Rules::parse(params),
            &[Accounts::List("allowlist"), Accounts::List("denylist")],
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
    use crate::MidenTransaction;

    const BLOCKED: &str = "0x000000000000000000000000000bad";

    async fn decide(request: &HookRequest) -> Decision {
        RecipientPolicy.evaluate(request).await.unwrap()
    }

    fn paying(notes: Vec<crate::OutputNote>) -> MidenTransaction {
        MidenTransaction {
            output_notes: notes,
            ..Default::default()
        }
    }

    fn with_an_unreadable_note() -> MidenTransaction {
        let mut partial = p2id(RECIPIENT, vec![fungible(USDC, 1)]);
        partial.partial = true;
        paying(vec![p2id(RECIPIENT, vec![fungible(USDC, 1)]), partial])
    }

    #[tokio::test]
    async fn an_allowlist_refuses_unlisted_and_unreadable_recipients() {
        let params = json!({ "allowlist": [RECIPIENT] });
        let listed = paying(vec![p2id(
            "0x0000000000000000000000000000A1",
            vec![fungible(USDC, 1)],
        )]);
        assert!(decide(&tx_request(params.clone(), listed)).await.allows());

        assert_eq!(
            decide(&tx_request(params.clone(), with_an_unreadable_note()))
                .await
                .reason_code,
            "recipient_not_allowed"
        );

        let unlisted = paying(vec![p2id(
            "0x0000000000000000000000000000a9",
            vec![fungible(USDC, 1)],
        )]);
        assert_eq!(
            decide(&tx_request(params, unlisted)).await.reason_code,
            "recipient_not_allowed"
        );
    }

    #[tokio::test]
    async fn a_denylist_refuses_listed_recipients_and_by_default_unreadable_ones() {
        let params = json!({ "denylist": ["0x000000000000000000000000000BAD"] });
        let blocked = paying(vec![fee_note(1), p2id(BLOCKED, vec![fungible(USDC, 1)])]);
        assert_eq!(
            decide(&tx_request(params.clone(), blocked))
                .await
                .reason_code,
            "recipient_blocked"
        );
        assert_eq!(
            decide(&tx_request(params, with_an_unreadable_note()))
                .await
                .reason_code,
            "recipient_blocked",
            "a recipient the denylist cannot check is refused: a blocked one could hide there"
        );

        let lenient = json!({ "denylist": [BLOCKED], "deny_unknown": false });
        assert!(
            decide(&tx_request(lenient, with_an_unreadable_note()))
                .await
                .allows(),
            "the operator opted out"
        );

        let nobody_listed = json!({ "denylist": [] });
        assert!(
            decide(&tx_request(nobody_listed, with_an_unreadable_note()))
                .await
                .allows(),
            "an empty denylist has nobody to hide"
        );

        let strict_anyway = json!({ "denylist": [], "deny_unknown": true });
        assert_eq!(
            decide(&tx_request(strict_anyway, with_an_unreadable_note()))
                .await
                .reason_code,
            "recipient_blocked"
        );
    }

    #[tokio::test]
    async fn the_fee_note_is_not_a_payment() {
        let params = json!({ "allowlist": [RECIPIENT] });
        let projection = paying(vec![fee_note(5), p2id(RECIPIENT, vec![fungible(USDC, 5)])]);
        assert!(decide(&tx_request(params, projection)).await.allows());
    }

    #[tokio::test]
    async fn no_list_allows_and_a_request_it_cannot_judge_is_an_error() {
        let projection = paying(vec![p2id(BLOCKED, vec![fungible(USDC, 1)])]);
        assert!(
            decide(&tx_request(json!({}), projection.clone()))
                .await
                .allows()
        );
        assert!(
            decide(&without_projection(tx_request(
                json!({}),
                projection.clone()
            )))
            .await
            .allows(),
            "an instance with no rule allows without reading the transaction, like every kind"
        );
        let unprojected =
            without_projection(tx_request(json!({ "denylist": [BLOCKED] }), projection));
        assert!(matches!(
            RecipientPolicy.evaluate(&unprojected).await,
            Err(ModuleError::InvalidRequest(_))
        ));
    }

    const PAYER: &str = "0x0000000000000000000000000000f1";

    fn reclaimable(target: &str, reclaimer: &str) -> crate::OutputNote {
        crate::OutputNote {
            reclaimer: Some(reclaimer.into()),
            ..p2id(target, vec![fungible(USDC, 1)])
        }
    }

    async fn decide_for_payer(params: serde_json::Value, note: crate::OutputNote) -> Decision {
        let mut request = tx_request(params, paying(vec![note]));
        request.account_id = PAYER.to_uppercase().replace("0X", "0x");
        decide(&request).await
    }

    #[tokio::test]
    async fn the_reclaimer_of_a_reclaimable_note_is_judged_as_a_recipient() {
        let denied = json!({ "denylist": [BLOCKED] });
        assert_eq!(
            decide_for_payer(denied.clone(), reclaimable(RECIPIENT, BLOCKED))
                .await
                .reason_code,
            "recipient_blocked",
            "a blocked account named as reclaimer can take the note"
        );
        assert!(
            decide_for_payer(denied, reclaimable(RECIPIENT, PAYER))
                .await
                .allows(),
            "the payer reclaiming its own note pays nobody"
        );

        let allowed = json!({ "allowlist": [RECIPIENT] });
        assert_eq!(
            decide_for_payer(allowed.clone(), reclaimable(RECIPIENT, BLOCKED))
                .await
                .reason_code,
            "recipient_not_allowed",
            "an allowed target does not cover an unlisted reclaimer"
        );
        assert!(
            decide_for_payer(allowed, reclaimable(RECIPIENT, PAYER))
                .await
                .allows()
        );
    }

    #[tokio::test]
    async fn a_recipient_named_by_its_address_is_stored_and_matched_as_its_hex_id() {
        let named = json!({ "denylist": [DEVNET_ADDRESS, ROUTED_ADDRESS] });
        let verdict = RecipientPolicy
            .validate_params(&serde_json::to_vec(&named).unwrap())
            .await
            .unwrap();
        assert!(verdict.ok, "{}", verdict.message);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&verdict.normalized_params).unwrap(),
            json!({ "denylist": [ADDRESSED, ADDRESSED] })
        );

        let paid = paying(vec![p2id(ADDRESSED, vec![fungible(USDC, 1)])]);
        assert_eq!(
            decide(&tx_request(named, paid)).await.reason_code,
            "recipient_blocked",
            "params stored before normalization still match"
        );

        let allowed = json!({ "allowlist": [DEVNET_ADDRESS.to_uppercase()] });
        let verdict = RecipientPolicy
            .validate_params(&serde_json::to_vec(&allowed).unwrap())
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&verdict.normalized_params).unwrap(),
            json!({ "allowlist": [ADDRESSED] }),
            "an address in upper case is the same address"
        );

        let hex = RecipientPolicy
            .validate_params(&serde_json::to_vec(&json!({ "denylist": [BLOCKED] })).unwrap())
            .await
            .unwrap();
        assert!(
            hex.ok && hex.normalized_params.is_empty(),
            "hex params are stored as sent"
        );
    }

    #[tokio::test]
    async fn params_are_validated() {
        for params in [
            json!({ "denylist": ["mtst1qrkd5x7qy0mw0yqzlvqlqd3w9cafjkw"] }),
            json!({ "denylist": [address_of(0, 15)] }),
            json!({ "denylist": [address_of(232, 10)] }),
            json!({ "denylist": [format!("{DEVNET_ADDRESS}_")] }),
            json!({ "denylist": [format!("{DEVNET_ADDRESS}_garbage")] }),
            json!({ "allowlist": [format!("{ROUTED_ADDRESS}_qr7qqq9wr6w")] }),
            json!({ "denylist": [DEVNET_ADDRESS.replacen('m', "M", 1)] }),
            json!({ "denylist": ["0xa1b2c3"] }),
            json!({ "allowlist": [RECIPIENT], "denylist": [BLOCKED] }),
            json!({ "deny_unknown": true }),
            json!({ "allowlist": [RECIPIENT], "deny_unknown": false }),
            json!({ "recipient_denylist": [BLOCKED] }),
            json!({ "max_per_transaction": {} }),
        ] {
            let verdict = RecipientPolicy
                .validate_params(&serde_json::to_vec(&params).unwrap())
                .await
                .unwrap();
            assert!(!verdict.ok, "{params}");
        }
    }
}
