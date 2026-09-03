//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo run --example procedure_roots -p miden-multisig-client -- --json`

use miden_protocol::Word;

/// Procedure names that can be used for threshold overrides.
///
/// Roots come from `cargo run --example procedure_roots -- --json` (typescript_hex
/// encoding). The auth roots are derived from the shared auth MASM — the copy the
/// TypeScript package bundles, byte-identical to upstream's component source — compiled
/// with the standards package linked dynamically, rather than from
/// `AuthGuardedMultisig::code()`.
///
/// The linkage is the reason the distinction matters. `CodeBuilder` links the standards
/// package dynamically and leaves an external reference; upstream's component manifest
/// declares `miden-standards = { linkage = "static" }` and inlines the callee's MAST. So
/// `auth_tx` — the one export that calls `miden::standards::fee` — hashes differently
/// under the two. The other five roots are identical under either linkage.
///
/// Both builders now produce the dynamic build: the TypeScript builder assembles the MASM
/// through the WASM `CodeBuilder`, and the Rust `MultisigGuardianBuilder` swaps the same
/// dynamically compiled code into the upstream component
/// (`miden_confidential_contracts::multisig_guardian::dynamically_linked_auth_code`). One
/// root therefore describes every guardian account, which is what the overrides map — keyed
/// by procedure root — requires. The wallet roots come from `BasicWallet` and are
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
                "0xcbb56b0f5b5eb426303fa69d6bfd51541db1d2fd26e6279214e10745fd0dece2",
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
    /// The shared auth MASM every pin here is derived from, and which both builders
    /// assemble accounts from. See `examples/procedure_roots.rs`.
    use miden_confidential_contracts::multisig_guardian::GUARDED_MULTISIG_AUTH_MASM as PACKAGE_AUTH_MASM;

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

    /// The bundled MASM compiled the way upstream's component is built: with the standards
    /// package linked statically.
    ///
    /// [`CodeBuilder`](miden_standards::code_builder::CodeBuilder) hard-codes
    /// `Linkage::Dynamic` for the standards package, whereas the component project manifest
    /// that produces [`upstream_auth_code`] declares `miden-standards = { linkage = "static" }`.
    /// Static linking inlines the callee's MAST into the caller, dynamic leaves an external
    /// reference, and the two hash differently. `auth_tx_guarded_multisig` is the only export
    /// that reaches into `miden::standards::fee`, which is why it is the only root the two
    /// paths disagree on — the other exports are byte-identical under either linkage.
    ///
    /// This reproduces `compile_component_code` exactly except for that one linkage argument,
    /// so the comparison below isolates the MASM rather than the build.
    fn bundled_auth_code_statically_linked() -> miden_protocol::account::AccountComponentCode {
        use std::sync::Arc;

        use miden_protocol::assembly::{
            DefaultSourceManager, Linkage, Module, ModuleKind, ModuleParser, Path as MasmPath,
        };
        use miden_protocol::transaction::TransactionKernel;
        use miden_standards::StandardsLib;

        let source_manager = Arc::new(DefaultSourceManager::default());
        let mut assembler =
            TransactionKernel::assembler_with_source_manager(source_manager.clone());
        assembler
            .link_package(StandardsLib::default().package(), Linkage::Static)
            .expect("linking the standards package statically should work");

        let module = ModuleParser::new(Some(ModuleKind::Library))
            .parse_str(
                Some(MasmPath::new("guarded_multisig")),
                PACKAGE_AUTH_MASM,
                source_manager,
            )
            .expect("bundled guarded-multisig MASM should parse");

        let package = assembler
            .assemble_library("account-component", module, None::<Box<Module>>)
            .expect("bundled guarded-multisig MASM should assemble");

        miden_protocol::account::AccountComponentCode::from(*package)
    }

    /// Custody-critical guard: a root that does not match the component accounts are
    /// built from means a per-procedure threshold override is stored under the wrong
    /// key and silently ignored at authentication time.
    ///
    /// Covers every procedure whose root is the same under both linkages, so the pin can
    /// be compared against the upstream component directly. `auth_tx` is deliberately
    /// absent — it is linkage-sensitive and has its own two tests below.
    #[test]
    fn procedure_roots_match_upstream_component() {
        use miden_standards::account::wallets::BasicWallet;

        let auth_code = upstream_auth_code();

        assert_eq!(
            ProcedureName::UpdateSigners.root(),
            auth_root_in(auth_code, "update_signers_and_threshold")
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

    /// Equivalence check: guardian's bundled MASM is the same code as the upstream
    /// component, `auth_tx` included.
    ///
    /// Compiled under matched linkage the two agree exactly; compiled the way
    /// [`bundled_auth_code`] does it — [`CodeBuilder`](miden_standards::code_builder::CodeBuilder)
    /// links the standards package dynamically, the upstream component's manifest links it
    /// statically — only `auth_tx_guarded_multisig` diverges, because it is the sole export
    /// that calls into `miden::standards::fee`. See [`bundled_auth_code_statically_linked`].
    ///
    /// The pin is deliberately NOT one side of this comparison. It tracks the dynamic
    /// compile, which is what both builders produce and therefore what deployed accounts
    /// carry; [`auth_tx_root_matches_the_bundled_masm`] and
    /// [`rust_built_accounts_carry_the_pinned_auth_tx_root`] are the tests that guard it.
    /// Do NOT "fix" a failure here by repointing the pin at the upstream root — that would
    /// silently move the pin away from the code accounts are actually built from. What this
    /// test establishes is that the linkage is the whole of the difference between the two
    /// build paths, so nothing but the linkage has to be reconciled.
    #[test]
    fn auth_tx_root_matches_upstream_component() {
        assert_eq!(
            auth_root_in(
                &bundled_auth_code_statically_linked(),
                "auth_tx_guarded_multisig"
            ),
            auth_root_in(upstream_auth_code(), "auth_tx_guarded_multisig")
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

    #[test]
    fn parse_unknown_returns_error() {
        let result: Result<ProcedureName, _> = "unknown_proc".parse();
        assert!(result.is_err());
    }
}
