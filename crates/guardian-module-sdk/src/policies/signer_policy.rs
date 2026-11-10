//! `signer-policy`: who may sign for the account.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::Deserialize;

use super::{IdKind, Rule};
use crate::{
    AccountView, Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo,
    ParamsVerdict,
};

pub const KIND: &str = "signer-policy";

const HOOKS: [Hook; 3] = [
    Hook::BeforeAccountRegistered,
    Hook::BeforeProposalCreated,
    Hook::BeforeTransactionApproved,
];

/// Which signers a list is checked against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerCheck {
    /// The signers a registration or a transaction adds; signers the account already has are
    /// kept until they change.
    #[default]
    Added,
    /// Every signer the account has after the registration or transaction.
    All,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// The only signer commitments the account may have. Excludes `denylist`.
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
    /// Signer commitments the account may not have. Excludes `allowlist`.
    #[serde(default)]
    pub denylist: Option<Vec<String>>,
    /// Which signers the list is checked against; `added` unless set. Needs a list.
    #[serde(default)]
    pub check: Option<SignerCheck>,
}

enum List {
    Allow(BTreeSet<String>),
    Deny(BTreeSet<String>),
}

impl List {
    fn permits(&self, signer: &str) -> bool {
        match self {
            List::Allow(list) => list.contains(signer),
            List::Deny(list) => !list.contains(signer),
        }
    }
}

struct Rules {
    list: Option<List>,
    check: SignerCheck,
}

impl TryFrom<Params> for Rules {
    type Error = String;

    fn try_from(params: Params) -> Result<Self, String> {
        if params.check.is_some() && params.allowlist.is_none() && params.denylist.is_none() {
            return Err("check applies to allowlist or denylist, neither of which is set".into());
        }
        let list = match (params.allowlist, params.denylist) {
            (Some(_), Some(_)) => {
                return Err("allowlist and denylist are mutually exclusive".into());
            }
            (Some(list), None) => Some(List::Allow(super::ids(
                "allowlist",
                IdKind::Commitment,
                &list,
            )?)),
            (None, Some(list)) => Some(List::Deny(super::ids(
                "denylist",
                IdKind::Commitment,
                &list,
            )?)),
            (None, None) => None,
        };
        Ok(Self {
            list,
            check: params.check.unwrap_or_default(),
        })
    }
}

impl Rules {
    fn parse(params: &[u8]) -> Result<Self, String> {
        super::parse::<Params>(params).and_then(Self::try_from)
    }

    fn judge(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        let Some(list) = &self.list else {
            return Ok(Decision::allow());
        };
        let after = super::after(request)?;
        Ok(
            match self
                .checked(request.before.as_ref(), after)
                .into_iter()
                .find(|signer| !list.permits(signer))
            {
                Some(signer) => Rule::Signers.deny(format!("signer {signer} is not permitted")),
                None => Decision::allow(),
            },
        )
    }

    fn checked(&self, before: Option<&AccountView>, after: &AccountView) -> Vec<String> {
        let existing: BTreeSet<String> = match (self.check, before) {
            (SignerCheck::Added, Some(before)) => super::normalized(&before.signer_commitments),
            _ => BTreeSet::new(),
        };
        super::normalized(&after.signer_commitments)
            .into_iter()
            .filter(|signer| !existing.contains(signer))
            .collect()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SignerPolicy;

#[async_trait]
impl Module for SignerPolicy {
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

    const LISTED: &str = "0x0000000000000000000000000000000000000000000000000000000000000052";
    const OTHER: &str = "0x0000000000000000000000000000000000000000000000000000000000000053";
    const BAD: &str = "0x0000000000000000000000000000000000000000000000000000000000000bad";

    async fn decide(request: &HookRequest) -> Decision {
        SignerPolicy.evaluate(request).await.unwrap()
    }

    #[tokio::test]
    async fn added_mode_checks_only_new_signers() {
        let params = json!({ "allowlist": [LISTED] });
        let adding_listed =
            registration_request(params.clone(), Some(&[SIGNER]), &[SIGNER, LISTED]);
        assert!(decide(&adding_listed).await.allows());
        let adding_other = registration_request(params, Some(&[SIGNER]), &[SIGNER, OTHER]);
        assert_eq!(
            decide(&adding_other).await.reason_code,
            "signer_not_allowed"
        );
    }

    #[tokio::test]
    async fn all_mode_checks_every_signer() {
        let params = json!({ "check": "all", "allowlist": [LISTED] });
        let request = registration_request(params, Some(&[SIGNER]), &[SIGNER, LISTED]);
        assert!(!decide(&request).await.allows());
    }

    #[tokio::test]
    async fn registration_checks_every_initial_signer() {
        let params = json!({ "denylist": [BAD] });
        let request = registration_request(
            params,
            None,
            &[
                SIGNER,
                "0x0000000000000000000000000000000000000000000000000000000000000BAD",
            ],
        );
        assert!(!decide(&request).await.allows());
    }

    #[tokio::test]
    async fn a_transaction_is_judged_on_its_account_views() {
        let params = json!({ "denylist": [BAD] });
        let mut request = tx_request(params, crate::MidenTransaction::default());
        request.after = Some(view(&[SIGNER, BAD]));
        assert_eq!(decide(&request).await.reason_code, "signer_not_allowed");
    }

    #[tokio::test]
    async fn missing_account_view_is_an_error_not_an_allow() {
        let mut request = registration_request(json!({ "allowlist": [SIGNER] }), None, &[]);
        request.after = None;
        assert!(SignerPolicy.evaluate(&request).await.is_err());
    }

    #[tokio::test]
    async fn params_are_validated() {
        for params in [
            json!({ "denylist": ["0x"] }),
            json!({ "allowlist": [], "denylist": [] }),
            json!({ "allowlist": ["0x000000000000000000000000000051"] }),
            json!({ "check": "all" }),
            json!({ "check": "sometimes", "allowlist": [SIGNER] }),
            json!({ "signer_allowlist": [SIGNER] }),
        ] {
            let verdict = SignerPolicy
                .validate_params(&serde_json::to_vec(&params).unwrap())
                .await
                .unwrap();
            assert!(!verdict.ok, "{params}");
        }
    }
}
