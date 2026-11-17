//! A KYC and sanctions policy module for the Guardian.
//!
//! The lists come from a [`Lists`] source: here a JSON file read once at startup
//! ([`Lists::from_file`]). The params of each catalog instance choose which checks apply, so
//! one process can serve a strict instance for some accounts and a lenient one for others.
//!
//! - **Signers (KYC).** Every signer of the account must be KYC-verified: at registration
//!   (`/configure`, hook `before_account_registered`) and, with `signers = "all"`, on every
//!   transaction, so an account that adds an unverified signer, or whose signer loses its
//!   verification, is refused.
//! - **Recipients.** A payment may not go to a sanctioned account (`recipients = "blocklist"`),
//!   or may go only to a verified one (`recipients = "allowlist"`). The recipients of a
//!   payment note are its P2ID target and, for a reclaimable P2IDE note, its reclaimer. A note
//!   whose recipient the Guardian cannot read (a partial note, a custom script) is refused.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{
    Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo,
    PROTOCOL_VERSION, ParamsVerdict, async_trait,
};
use serde::Deserialize;

/// The hooks this module judges: account registration and both transaction gates.
pub const HOOKS: [Hook; 3] = [
    Hook::BeforeAccountRegistered,
    Hook::BeforeProposalCreated,
    Hook::BeforeTransactionApproved,
];

/// Hex digits of a Miden account id and of a signer commitment, as the Guardian sends them.
const ACCOUNT_ID_DIGITS: usize = 30;
const COMMITMENT_DIGITS: usize = 64;

/// Who is verified and who is sanctioned. Read from a JSON file here; a real deployment would
/// read the same three lists from its compliance database or vendor API (see the guide).
#[derive(Debug, Default)]
pub struct Lists {
    kyc_signers: BTreeSet<String>,
    sanctioned_accounts: BTreeSet<String>,
    verified_accounts: BTreeSet<String>,
}

/// The data file, as written: ids in `0x` hex, any case.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListsFile {
    #[serde(default)]
    kyc_signers: Vec<String>,
    #[serde(default)]
    sanctioned_accounts: Vec<String>,
    #[serde(default)]
    verified_accounts: Vec<String>,
}

impl Lists {
    /// Reads and checks the data file. A malformed id refuses the whole file: a list with an
    /// id that can never match would silently not apply.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: ListsFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self {
            kyc_signers: ids("kyc_signers", COMMITMENT_DIGITS, &file.kyc_signers)?,
            sanctioned_accounts: ids(
                "sanctioned_accounts",
                ACCOUNT_ID_DIGITS,
                &file.sanctioned_accounts,
            )?,
            verified_accounts: ids(
                "verified_accounts",
                ACCOUNT_ID_DIGITS,
                &file.verified_accounts,
            )?,
        })
    }

    /// The count of each list, for the startup log.
    pub fn sizes(&self) -> (usize, usize, usize) {
        (
            self.kyc_signers.len(),
            self.sanctioned_accounts.len(),
            self.verified_accounts.len(),
        )
    }
}

fn ids(field: &str, digits: usize, values: &[String]) -> Result<BTreeSet<String>, String> {
    values
        .iter()
        .map(|value| {
            let hex = value
                .strip_prefix("0x")
                .filter(|rest| rest.len() == digits && rest.bytes().all(|b| b.is_ascii_hexdigit()));
            match hex {
                Some(_) => Ok(value.to_ascii_lowercase()),
                None => Err(format!(
                    "{field}: {value:?} is not 0x followed by {digits} hex digits"
                )),
            }
        })
        .collect()
}

/// Which signers a request checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerCheck {
    /// Every signer of the account, on every request.
    #[default]
    All,
    /// Every signer at registration; afterwards only the signers a transaction adds.
    Added,
    Off,
}

/// Which recipients a payment may go to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientCheck {
    /// Any account that is not sanctioned.
    #[default]
    Blocklist,
    /// Only verified accounts.
    Allowlist,
    Off,
}

/// An instance's params: which checks apply. The lists themselves are the module's data.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default)]
    pub signers: SignerCheck,
    #[serde(default)]
    pub recipients: RecipientCheck,
}

impl Params {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    }
}

/// A denial: a stable reason code for operators and clients, the detail for the log only,
/// and a client message that names the rule, never a list entry.
fn deny(reason_code: &str, logged: String, client_message: &str) -> Decision {
    Decision::deny(reason_code, logged).with_client_message(client_message)
}

pub struct Compliance {
    lists: Arc<Lists>,
}

impl Compliance {
    pub fn new(lists: Lists) -> Self {
        Self {
            lists: Arc::new(lists),
        }
    }

    fn unverified_signer<'a>(
        &self,
        signers: impl IntoIterator<Item = &'a String>,
    ) -> Option<Decision> {
        signers
            .into_iter()
            .find(|signer| {
                !self
                    .lists
                    .kyc_signers
                    .contains(&signer.to_ascii_lowercase())
            })
            .map(|signer| {
                deny(
                    "signer_not_verified",
                    format!("signer {signer} is not KYC-verified"),
                    "A signer of this account has not completed identity verification.",
                )
            })
    }

    fn judge_registration(
        &self,
        params: Params,
        request: &HookRequest,
    ) -> Result<Decision, ModuleError> {
        if params.signers == SignerCheck::Off {
            return Ok(Decision::allow());
        }
        let after = account_after(request)?;
        Ok(self
            .unverified_signer(&after.signer_commitments)
            .unwrap_or_else(Decision::allow))
    }

    fn judge_transaction(
        &self,
        params: Params,
        request: &HookRequest,
    ) -> Result<Decision, ModuleError> {
        if let Some(denial) = self.judge_signers(params, request)? {
            return Ok(denial);
        }
        self.judge_recipients(params, request)
    }

    fn judge_signers(
        &self,
        params: Params,
        request: &HookRequest,
    ) -> Result<Option<Decision>, ModuleError> {
        Ok(match params.signers {
            SignerCheck::Off => None,
            SignerCheck::All => self.unverified_signer(&account_after(request)?.signer_commitments),
            SignerCheck::Added => {
                let after = account_after(request)?;
                let before: BTreeSet<&String> = request
                    .before
                    .iter()
                    .flat_map(|view| &view.signer_commitments)
                    .collect();
                self.unverified_signer(
                    after
                        .signer_commitments
                        .iter()
                        .filter(|signer| !before.contains(signer)),
                )
            }
        })
    }

    fn judge_recipients(
        &self,
        params: Params,
        request: &HookRequest,
    ) -> Result<Decision, ModuleError> {
        if params.recipients == RecipientCheck::Off {
            return Ok(Decision::allow());
        }
        let Some(hook_request::Subject::Transaction(subject)) = &request.subject else {
            return Err(ModuleError::InvalidRequest(
                "request has no transaction".into(),
            ));
        };
        let transaction = subject.miden().ok_or_else(|| {
            ModuleError::InvalidRequest("transaction has no Miden projection".into())
        })?;
        let payer = request.account_id.to_ascii_lowercase();
        for note in transaction.output_notes.iter().filter(|note| !note.tx_fee) {
            let target = note
                .p2id_target
                .as_deref()
                .filter(|_| !note.partial)
                .map(str::to_ascii_lowercase);
            let Some(target) = target else {
                return Ok(deny(
                    "recipient_unknown",
                    format!(
                        "note {} has no recipient this module can check",
                        note.note_id
                    ),
                    "A payment goes to a recipient that cannot be screened.",
                ));
            };
            let reclaimer = note
                .reclaimer
                .as_deref()
                .map(str::to_ascii_lowercase)
                .filter(|reclaimer| *reclaimer != payer);
            for recipient in std::iter::once(target).chain(reclaimer) {
                if let Some(denial) = self.refused_recipient(params.recipients, &recipient) {
                    return Ok(denial);
                }
            }
        }
        Ok(Decision::allow())
    }

    fn refused_recipient(&self, check: RecipientCheck, recipient: &str) -> Option<Decision> {
        match check {
            RecipientCheck::Off => None,
            RecipientCheck::Blocklist => {
                self.lists.sanctioned_accounts.contains(recipient).then(|| {
                    deny(
                        "recipient_sanctioned",
                        format!("recipient {recipient} is on the sanctions list"),
                        "A payment goes to a recipient this Guardian cannot pay.",
                    )
                })
            }
            RecipientCheck::Allowlist => {
                (!self.lists.verified_accounts.contains(recipient)).then(|| {
                    deny(
                        "recipient_not_verified",
                        format!("recipient {recipient} is not a verified account"),
                        "A payment goes to a recipient that has not been verified.",
                    )
                })
            }
        }
    }
}

/// The account as it will be if every module allows. Without it the module cannot judge the
/// signers, which is an error for the instance's `on_error` to decide, never an allow.
fn account_after(request: &HookRequest) -> Result<&guardian_module_sdk::AccountView, ModuleError> {
    let after = request
        .after
        .as_ref()
        .ok_or_else(|| ModuleError::InvalidRequest("request has no account view".into()))?;
    if after.signer_commitments.is_empty() {
        return Err(ModuleError::InvalidRequest(
            "account view lists no signer".into(),
        ));
    }
    Ok(after)
}

#[async_trait]
impl Module for Compliance {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(ModuleInfo {
            name: "compliance".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: HOOKS.iter().map(|hook| *hook as i32).collect(),
            ..Default::default()
        })
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(match Params::parse(params) {
            Ok(_) => ParamsVerdict::ok(),
            Err(message) => ParamsVerdict::invalid(message),
        })
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        let params = Params::parse(&request.params).map_err(ModuleError::InvalidRequest)?;
        match &request.subject {
            Some(hook_request::Subject::Registration(_)) => {
                self.judge_registration(params, request)
            }
            Some(hook_request::Subject::Transaction(_)) => self.judge_transaction(params, request),
            None => Err(ModuleError::InvalidRequest(
                "request has no subject this module knows".into(),
            )),
        }
    }
}
