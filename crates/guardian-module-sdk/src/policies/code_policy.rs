//! `code-policy`: which account code the account may run.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::Deserialize;

use super::{IdKind, Rule};
use crate::proto::hook_request;
use crate::{
    Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo, ParamsVerdict,
};

pub const KIND: &str = "code-policy";

const HOOKS: [Hook; 3] = [
    Hook::BeforeAccountRegistered,
    Hook::BeforeProposalCreated,
    Hook::BeforeTransactionApproved,
];

/// Which code `allowed` is checked against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeCheck {
    /// The code an account registers with, and code a transaction replaces it with; an account
    /// already on other code keeps working until it changes code.
    #[default]
    Changed,
    /// The code the account runs after the registration or transaction, at every hook.
    All,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// The account code commitments the account may run.
    #[serde(default)]
    pub allowed: Option<Vec<String>>,
    /// Which code `allowed` is checked against; `changed` unless set. Needs `allowed`.
    #[serde(default)]
    pub check: Option<CodeCheck>,
}

struct Rules {
    allowed: Option<BTreeSet<String>>,
    check: CodeCheck,
}

impl TryFrom<Params> for Rules {
    type Error = String;

    fn try_from(params: Params) -> Result<Self, String> {
        if params.check.is_some() && params.allowed.is_none() {
            return Err("check applies to allowed, which is not set".into());
        }
        Ok(Self {
            allowed: params
                .allowed
                .as_deref()
                .map(|list| super::ids("allowed", IdKind::Commitment, list))
                .transpose()?,
            check: params.check.unwrap_or_default(),
        })
    }
}

impl Rules {
    fn parse(params: &[u8]) -> Result<Self, String> {
        super::parse::<Params>(params).and_then(Self::try_from)
    }

    fn judge(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        let Some(allowed) = &self.allowed else {
            return Ok(Decision::allow());
        };
        Ok(match self.checked(request)? {
            Some(code) if !allowed.contains(&code.to_ascii_lowercase()) => {
                Rule::Code.deny(format!("code {code} is not allowed"))
            }
            _ => Decision::allow(),
        })
    }

    /// The code commitment `allowed` must list for this request, if any: under `changed` the
    /// code an account registers with or a transaction replaces its code with, under `all` the
    /// code the account runs after the request.
    fn checked(&self, request: &HookRequest) -> Result<Option<String>, ModuleError> {
        let after_code = || super::after(request).map(|after| after.code_commitment.clone());
        match (self.check, &request.subject) {
            (CodeCheck::All, _)
            | (CodeCheck::Changed, Some(hook_request::Subject::Registration(_))) => {
                after_code().map(Some)
            }
            (CodeCheck::Changed, _) => Ok(super::transaction(request)?.new_code_commitment.clone()),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CodePolicy;

#[async_trait]
impl Module for CodePolicy {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(super::info(KIND, &HOOKS))
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(super::verdict(params, Rules::parse(params), &[]))
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

    const REVIEWED: &str = "0x000000000000000000000000000000000000000000000000000000000000c0de";
    const UNREVIEWED: &str = "0x0000000000000000000000000000000000000000000000000000000000000bad";

    async fn decide(request: &HookRequest) -> Decision {
        CodePolicy.evaluate(request).await.unwrap()
    }

    fn rule(check: &str) -> serde_json::Value {
        json!({ "allowed": [REVIEWED], "check": check })
    }

    fn running(mut request: HookRequest, code: &str) -> HookRequest {
        if let Some(after) = request.after.as_mut() {
            after.code_commitment = code.into();
        }
        request
    }

    fn registration(params: serde_json::Value, code: &str) -> HookRequest {
        running(registration_request(params, None, &[SIGNER]), code)
    }

    fn replacing_code_with(code: Option<&str>) -> MidenTransaction {
        MidenTransaction {
            new_code_commitment: code.map(str::to_string),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn changed_check_judges_registration_and_code_replacement_only() {
        let changed = rule("changed");
        assert!(
            decide(&registration(changed.clone(), REVIEWED))
                .await
                .allows()
        );
        let refused = decide(&registration(changed.clone(), UNREVIEWED)).await;
        assert_eq!(refused.reason_code, "code_not_allowed");
        assert!(!refused.client_message.contains(UNREVIEWED));

        let unchanged = running(
            tx_request(changed.clone(), replacing_code_with(None)),
            UNREVIEWED,
        );
        assert!(
            decide(&unchanged).await.allows(),
            "code an account already runs is kept until it changes"
        );
        let upgrade = tx_request(changed.clone(), replacing_code_with(Some(UNREVIEWED)));
        assert_eq!(decide(&upgrade).await.reason_code, "code_not_allowed");
        let reviewed = tx_request(changed, replacing_code_with(Some(REVIEWED)));
        assert!(decide(&reviewed).await.allows());
    }

    #[tokio::test]
    async fn all_check_judges_the_code_an_account_runs_at_every_hook() {
        let all = rule("all");
        let unchanged = running(
            tx_request(all.clone(), replacing_code_with(None)),
            UNREVIEWED,
        );
        assert_eq!(decide(&unchanged).await.reason_code, "code_not_allowed");
        let upgraded = running(
            tx_request(all, replacing_code_with(Some(REVIEWED))),
            REVIEWED,
        );
        assert!(
            decide(&upgraded).await.allows(),
            "an account on unlisted code can still upgrade to listed code"
        );
    }

    #[tokio::test]
    async fn a_transaction_without_a_projection_is_not_judged_under_changed() {
        let unprojected = without_projection(tx_request(
            rule("changed"),
            replacing_code_with(Some(UNREVIEWED)),
        ));
        assert!(matches!(
            CodePolicy.evaluate(&unprojected).await,
            Err(ModuleError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn params_are_validated() {
        for params in [
            json!({ "allowed": [REVIEWED], "check": "sometimes" }),
            json!({ "allowed": [format!("0x{}", "c".repeat(63))] }),
            json!({ "check": "all" }),
            json!({ "allowed_code_commitments": [REVIEWED] }),
        ] {
            let verdict = CodePolicy
                .validate_params(&serde_json::to_vec(&params).unwrap())
                .await
                .unwrap();
            assert!(!verdict.ok, "{params}");
        }
    }
}
