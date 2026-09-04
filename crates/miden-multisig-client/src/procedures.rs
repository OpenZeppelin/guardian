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
/// Neither builder compiles the MASM itself any more. Doing so linked the standards package
/// dynamically while upstream's component manifest links it statically, and the two hash
/// differently for `auth_tx` — the sole export that calls `miden::standards::fee`. An account
/// carrying the dynamic root cannot be classified by `AccountComponentInterface`, so the client
/// attaches no fee conversion info and the transaction fails on a fee-charging chain.
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
}

impl ProcedureName {
    /// Get the procedure root for this procedure name.
    ///
    /// These roots are deterministic given the MASM they were derived from.
    pub fn root(&self) -> Word {
        match self {
            ProcedureName::UpdateSigners => procedure_root_word(
                "0xa261cfd3c8791ac5abe1e78e14eade2f20789d73ab1c23c430418de59bc3380e",
            ),
            ProcedureName::UpdateProcedureThreshold => procedure_root_word(
                "0x97587c61d49313b1d5a3c8b7437e0080e67ed9bd9d3e7206bcae562f934ccd03",
            ),
            ProcedureName::AuthTx => procedure_root_word(
                "0xcd2cee82d17af1c8228563808507bf50ea661259e8f74b60bf497efed9029665",
            ),
            ProcedureName::UpdateGuardian => procedure_root_word(
                "0x0a614ff7c81a561cbd2a4c2d9482031a7a841ca5de33349daed23a9d871b3675",
            ),
            ProcedureName::SendAsset => procedure_root_word(
                "0x595bc83258726a66bd904912cfd5186c07cbd902dfbc115b7d6bc8105efc57e3",
            ),
            ProcedureName::ReceiveAsset => procedure_root_word(
                "0x34a56dd18f6fe5aab63198b9dcfc6467e793ebabb37d56b994b902504635da13",
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

    /// The salt path's single point of failure.
    ///
    /// The request builders declare only a `fee_conversion_salt` and leave miden-client to
    /// commit the conversion info. The client does that only for an account it can classify:
    /// `AuthGuardedMultisig` maps to `FeeAuth::CallerChosenSalt`, while an account it cannot
    /// place is `FeeAuth::Ignored`, and `Ignored` plus a declared salt is a hard
    /// `FeeConversionInfoUnsupported` — every guarded transaction on a fee-charging chain
    /// fails, not just the fee.
    ///
    /// Pinning `auth_tx` alone does not cover this: `extract_component` matches only when
    /// EVERY one of the component's roots is present, so a standards bump that changes any
    /// other export silently drops the classification while the root test above stays green.
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
