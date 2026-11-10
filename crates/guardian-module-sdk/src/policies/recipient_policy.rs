//! `recipient-policy`: who a transaction may pay.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::Deserialize;

use super::{IdKind, Rule};
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
        let payments = projection.output_notes.iter().filter(|note| !note.tx_fee);
        for note in payments {
            let recipient = match (&note.p2id_target, note.partial) {
                (Some(target), false) => Some(target.to_ascii_lowercase()),
                _ => None,
            };
            let refused = match (self, recipient) {
                (Rules::Any, _) => None,
                (Rules::Allow(_), None) => Some(
                    Rule::RecipientAllowlist
                        .deny(format!("note {} has no checkable recipient", note.note_id)),
                ),
                (Rules::Allow(allowed), Some(recipient)) => {
                    (!allowed.contains(&recipient)).then(|| {
                        Rule::RecipientAllowlist
                            .deny(format!("recipient {recipient} is not allowed"))
                    })
                }
                (Rules::Deny { deny_unknown, .. }, None) => deny_unknown.then(|| {
                    Rule::RecipientDenylist
                        .deny(format!("note {} has no checkable recipient", note.note_id))
                }),
                (Rules::Deny { blocked, .. }, Some(recipient)) => {
                    blocked.contains(&recipient).then(|| {
                        Rule::RecipientDenylist.deny(format!("recipient {recipient} is blocked"))
                    })
                }
            };
            if let Some(denial) = refused {
                return Ok(denial);
            }
        }
        Ok(Decision::allow())
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
        Ok(super::verdict(Rules::parse(params)))
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

    #[tokio::test]
    async fn params_are_validated() {
        for params in [
            json!({ "denylist": ["mtst1qrkd5x7qy0mw0yqzlvqlqd3w9cafjkw"] }),
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
