//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo run --example procedure_roots -p miden-multisig-client -- --json`

use miden_protocol::Word;

/// Procedure names that can be used for threshold overrides.
///
/// Roots come from `cargo run --example procedure_roots -- --json` (typescript_hex
/// encoding). The auth roots are derived from the auth MASM the TypeScript package
/// bundles, rather than from `AuthGuardedMultisig::code()`. The two sources agree only
/// while the MASM is byte-identical *and* both compile against the same standards
/// release, and `auth_tx` calls `miden::standards::fee`, so its root moves with the fee
/// library and they currently do not.
///
/// Which means these roots describe browser-built accounts: the TypeScript builder
/// assembles from that MASM, while the Rust `MultisigGuardianBuilder` goes through
/// `AuthGuardedMultisig::code()`. Reconciling the two is the open design question the
/// account-roundtrip failures track. The wallet roots come from `BasicWallet` and are
/// unaffected. Neither source has a standalone `verify_guardian` procedure; guardian
/// verification is internal to `auth_tx_guarded_multisig`.
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
                "0x128d0c53cc141e8f175df4a541a981d724f11a62a8fd15486ec4c6132404f6fc",
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

    /// Custody-critical guard: a root that does not match the component accounts are
    /// built from means a per-procedure threshold override is stored under the wrong
    /// key and silently ignored at authentication time.
    ///
    /// The bundled auth MASM every pin here is derived from, and which the TypeScript
    /// builder assembles browser accounts from. See `examples/procedure_roots.rs`.
    #[cfg(test)]
    const PACKAGE_AUTH_MASM: &str = include_str!(
        "../../../packages/miden-multisig-client/masm/account_components/auth/guarded_multisig.masm"
    );

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

    fn bundled_auth_code() -> miden_protocol::account::AccountComponentCode {
        miden_standards::code_builder::CodeBuilder::new()
            .compile_component_code("guarded_multisig", PACKAGE_AUTH_MASM)
            .expect("bundled guarded-multisig MASM should compile")
    }

    /// Custody-critical guard: a root that does not match the component accounts are
    /// built from means a per-procedure threshold override is stored under the wrong
    /// key and silently ignored at authentication time.
    ///
    /// Covers every procedure whose root the upstream component and the bundled MASM
    /// agree on. `auth_tx` is deliberately absent — it has its own two tests below,
    /// because it is the one that diverges and an assertion for it here would abort
    /// this test before the procedures after it were ever checked.
    #[test]
    fn procedure_roots_match_upstream_component() {
        use miden_standards::account::wallets::BasicWallet;

        let auth_code = upstream_auth_code();

        assert_eq!(
            ProcedureName::UpdateSigners.root(),
            auth_root_in(&auth_code, "update_signers_and_threshold")
        );
        assert_eq!(
            ProcedureName::UpdateProcedureThreshold.root(),
            auth_root_in(&auth_code, "set_procedure_threshold")
        );
        assert_eq!(
            ProcedureName::UpdateGuardian.root(),
            auth_root_in(&auth_code, "update_guardian_public_key")
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

    /// The `auth_tx` pin against the source it is actually derived from. Since `auth_tx`
    /// began calling `miden::standards::fee` its root moves with the fee library, so the
    /// pin tracks the bundled MASM rather than the upstream component.
    #[test]
    fn auth_tx_root_matches_the_bundled_masm() {
        assert_eq!(
            ProcedureName::AuthTx.root(),
            auth_root_in(&bundled_auth_code(), "auth_tx_guarded_multisig")
        );
    }

    /// Expected red, and the only assertion in this module that is: the upstream
    /// component and the bundled MASM disagree on `auth_tx`, which is the open design
    /// question the account-roundtrip failures track.
    ///
    /// Do NOT "fix" this by copying whatever root the upstream component reports — that
    /// would silently repoint the pin away from the MASM accounts are built from. It
    /// goes green when the two sources are reconciled, at which point this test and
    /// [`auth_tx_root_matches_the_bundled_masm`] collapse into the test above.
    #[test]
    fn auth_tx_root_matches_upstream_component() {
        assert_eq!(
            ProcedureName::AuthTx.root(),
            auth_root_in(&upstream_auth_code(), "auth_tx_guarded_multisig")
        );
    }

    #[test]
    fn parse_unknown_returns_error() {
        let result: Result<ProcedureName, _> = "unknown_proc".parse();
        assert!(result.is_err());
    }
}
