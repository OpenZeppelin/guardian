//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo run --example procedure_roots -p miden-multisig-client -- --json`

use miden_protocol::Word;

/// Procedure names that can be used for threshold overrides.
///
/// Roots come from `cargo run --example procedure_roots -- --json` (typescript_hex
/// encoding). The auth roots come from the upstream `AuthGuardedMultisig` component
/// (`AuthGuardedMultisig::code()`), which is what both builders now assemble accounts from:
/// the Rust `MultisigGuardianBuilder` uses the component directly, and the TypeScript builder
/// gets it from the web SDK's `createAuthGuardedMultisig`.
///
/// Neither builder compiles the MASM itself any more. Doing so must link the standards package
/// exactly as upstream's component does (dynamically since protocol 0.17.0-rc.8), or the two hash
/// differently for `auth_tx`, the sole export that calls `miden::standards::fee`. An account
/// carrying that root fails the pinned-contract check. The request builders do not ask
/// miden-client to invent the fee commitment: they set the three-word multisig auth args
/// themselves.
///
/// The wallet roots come from `BasicWallet` and are unaffected. Neither source has a standalone
/// `verify_guardian` procedure; guardian verification is internal to `auth_tx_guarded_multisig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcedureName {
    UpdateSigners,
    UpdateProcedureThreshold,
    AuthTx,
    UpdateGuardian,
    SendAsset,
    ReceiveAsset,
    CreateNote,
}

impl ProcedureName {
    /// Get the procedure root for this procedure name.
    ///
    /// These roots are deterministic given the MASM they were derived from.
    pub fn root(&self) -> Word {
        match self {
            ProcedureName::UpdateSigners => procedure_root_word(
                "0xb7b2cf42cc4f2f331acfeee835e60c4115c9ee5f80db0448dd098f6b56785d19",
            ),
            ProcedureName::UpdateProcedureThreshold => procedure_root_word(
                "0x9cc9ab8112578967b43b8a34a98314443511e6f3fdc5f81581777f415c1d7b33",
            ),
            ProcedureName::AuthTx => procedure_root_word(
                "0x08fc0b2d1dd361737c8cfd1890a5826c07bee5ed1edd743f798d47969168727e",
            ),
            ProcedureName::UpdateGuardian => procedure_root_word(
                "0xc6971a2e9a617e2c88a96ff44d7af65ca5b50a42114f7422a522111baf1cc173",
            ),
            ProcedureName::SendAsset => procedure_root_word(
                "0x6bd84395a8aace697f1a8e0bc5a74d5d2ee5125cd506b3440d74d6a31bc60611",
            ),
            ProcedureName::ReceiveAsset => procedure_root_word(
                "0xfa82d5e3a621c413649d383b07ea7c09ea4d5e63f9661516b97f5d9a994a736c",
            ),
            ProcedureName::CreateNote => procedure_root_word(
                "0x23e4c270aa766134ec2998dbe7cae25260444819f68d10002668202cde6db04c",
            ),
        }
    }

    /// Get all available procedure names.
    pub fn all() -> &'static [ProcedureName] {
        &[
            ProcedureName::UpdateSigners,
            ProcedureName::UpdateProcedureThreshold,
            ProcedureName::AuthTx,
            ProcedureName::UpdateGuardian,
            ProcedureName::SendAsset,
            ProcedureName::ReceiveAsset,
            ProcedureName::CreateNote,
        ]
    }
}

/// Per-procedure threshold override.
///
/// Allows specifying different signature thresholds for specific procedures.
///
/// # Example
///
/// ```
/// use miden_multisig_client::{ProcedureThreshold, ProcedureName};
///
/// let receive_threshold = ProcedureThreshold::new(ProcedureName::ReceiveAsset, 1);
/// let config_threshold = ProcedureThreshold::new(ProcedureName::UpdateSigners, 3);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ProcedureThreshold {
    pub procedure: ProcedureName,
    pub threshold: u32,
}

impl ProcedureThreshold {
    pub fn new(procedure: ProcedureName, threshold: u32) -> Self {
        Self {
            procedure,
            threshold,
        }
    }

    pub fn procedure_root(&self) -> Word {
        self.procedure.root()
    }
}

impl std::fmt::Display for ProcedureName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcedureName::UpdateSigners => write!(f, "update_signers"),
            ProcedureName::UpdateProcedureThreshold => write!(f, "update_procedure_threshold"),
            ProcedureName::AuthTx => write!(f, "auth_tx"),
            ProcedureName::UpdateGuardian => write!(f, "update_guardian"),
            ProcedureName::SendAsset => write!(f, "send_asset"),
            ProcedureName::ReceiveAsset => write!(f, "receive_asset"),
            ProcedureName::CreateNote => write!(f, "create_note"),
        }
    }
}

impl std::str::FromStr for ProcedureName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "update_signers" => Ok(ProcedureName::UpdateSigners),
            "update_procedure_threshold" => Ok(ProcedureName::UpdateProcedureThreshold),
            "auth_tx" => Ok(ProcedureName::AuthTx),
            "update_guardian" => Ok(ProcedureName::UpdateGuardian),
            "send_asset" => Ok(ProcedureName::SendAsset),
            "receive_asset" => Ok(ProcedureName::ReceiveAsset),
            "create_note" => Ok(ProcedureName::CreateNote),
            _ => Err(format!("unknown procedure name: {}", s)),
        }
    }
}

fn procedure_root_word(hex_str: &str) -> Word {
    Word::parse(hex_str).expect("valid procedure root constant")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procedure_threshold_new_creates_correctly() {
        let threshold = ProcedureThreshold::new(ProcedureName::ReceiveAsset, 1);
        assert_eq!(threshold.procedure, ProcedureName::ReceiveAsset);
        assert_eq!(threshold.threshold, 1);
    }

    #[test]
    fn procedure_threshold_procedure_root_returns_correct_root() {
        let threshold = ProcedureThreshold::new(ProcedureName::SendAsset, 2);
        assert_eq!(threshold.procedure_root(), ProcedureName::SendAsset.root());
    }

    #[test]
    fn procedure_name_round_trip() {
        for name in ProcedureName::all() {
            let s = name.to_string();
            let parsed: ProcedureName = s.parse().unwrap();
            assert_eq!(*name, parsed);
        }
    }

    #[test]
    fn procedure_roots_are_valid() {
        for name in ProcedureName::all() {
            let _root = name.root();
        }
    }

    fn auth_root_in(code: &miden_protocol::account::AccountComponentCode, masm_name: &str) -> Word {
        let export = code
            .exports()
            .find(|e| e.path.to_string().rsplit("::").next() == Some(masm_name))
            .unwrap_or_else(|| panic!("procedure `{masm_name}` not found"));
        code.get_procedure_root_by_path(&*export.path)
            .expect("root by path")
            .into()
    }

    fn upstream_auth_code() -> &'static miden_protocol::account::AccountComponentCode {
        miden_standards::account::auth::AuthGuardedMultisig::code()
    }

    /// Custody-critical guard: a root that does not match the component accounts are
    /// built from means a per-procedure threshold override is stored under the wrong
    /// key and silently ignored at authentication time.
    ///
    /// Every pinned root is compared against the upstream component. `auth_tx` needed
    /// special handling while the MASM was vendored — it links `miden::standards::fee`,
    /// so a locally compiled copy rooted differently. Building from upstream removes that.
    #[test]
    fn procedure_roots_match_upstream_component() {
        use miden_standards::account::wallets::BasicWallet;

        let auth_code = upstream_auth_code();

        assert_eq!(
            ProcedureName::UpdateSigners.root(),
            auth_root_in(auth_code, "update_signers_and_threshold")
        );
        assert_eq!(
            ProcedureName::AuthTx.root(),
            auth_root_in(auth_code, "auth_tx_guarded_multisig")
        );
        assert_eq!(
            ProcedureName::UpdateProcedureThreshold.root(),
            auth_root_in(auth_code, "set_procedure_threshold")
        );
        assert_eq!(
            ProcedureName::UpdateGuardian.root(),
            auth_root_in(auth_code, "update_guardian_public_key")
        );
        assert_eq!(
            ProcedureName::SendAsset.root(),
            Word::from(BasicWallet::move_asset_to_note_root())
        );
        assert_eq!(
            ProcedureName::ReceiveAsset.root(),
            Word::from(BasicWallet::receive_asset_root())
        );
        assert_eq!(
            ProcedureName::CreateNote.root(),
            Word::from(BasicWallet::create_note_root())
        );
    }

    /// The regression guard for the divergence itself: an account built the Rust way must
    /// carry the pinned `auth_tx` root, or every root-keyed threshold read against it fails
    /// with `UnsupportedContractVersion` (and, worse, an account built by one SDK could not
    /// be configured by the other).
    #[test]
    fn rust_built_accounts_carry_the_pinned_auth_tx_root() {
        use miden_confidential_contracts::multisig_guardian::{
            MultisigGuardianBuilder, MultisigGuardianConfig,
        };

        let config = MultisigGuardianConfig::new(
            1,
            vec![Word::from([1u32, 0, 0, 0])],
            Word::from([9u32, 0, 0, 0]),
        );
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([7u8; 32])
            .build_existing()
            .expect("account builds");

        assert!(
            account.code().has_procedure(ProcedureName::AuthTx.root()),
            "the Rust builder must produce the same auth_tx root the browser builder does"
        );
    }

    /// Classification is the rest of the root pin.
    ///
    /// The request builders set the three-word multisig auth args themselves. They no longer
    /// declare a `fee_conversion_salt` and wait for miden-client to commit conversion info.
    /// `AccountComponentInterface::from_procedures` still has to see `AuthGuardedMultisig`,
    /// because that match requires every export of the component. A standards bump that
    /// changes any root other than `auth_tx` drops the classification while the root test
    /// above stays green.
    #[test]
    fn rust_built_accounts_classify_as_guarded_multisig() {
        use miden_confidential_contracts::multisig_guardian::{
            MultisigGuardianBuilder, MultisigGuardianConfig,
        };
        use miden_standards::account::interface::{
            AccountComponentInterface, AccountComponentInterfaceExt,
        };

        let config = MultisigGuardianConfig::new(
            1,
            vec![Word::from([1u32, 0, 0, 0])],
            Word::from([9u32, 0, 0, 0]),
        );
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([7u8; 32])
            .build_existing()
            .expect("account builds");

        let procedures: Vec<_> = account.code().procedures().to_vec();
        let components = AccountComponentInterface::from_procedures(&procedures);

        assert!(
            components.iter().any(|component| matches!(
                component,
                AccountComponentInterface::AuthGuardedMultisig
            )),
            "miden-client must classify the built account as AuthGuardedMultisig, got {components:?}"
        );
    }

    #[test]
    fn parse_unknown_returns_error() {
        let result: Result<ProcedureName, _> = "unknown_proc".parse();
        assert!(result.is_err());
    }
}
