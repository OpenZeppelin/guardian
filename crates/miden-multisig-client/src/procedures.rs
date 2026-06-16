//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo run --example procedure_roots -p miden-multisig-client -- --json`

use miden_protocol::Word;

/// Procedure names that can be used for threshold overrides.
///
/// Roots are sourced from the upstream `AuthGuardedMultisig` + `BasicWallet`
/// procedures via `cargo run --example procedure_roots -- --json` (typescript_hex
/// encoding). The upstream component has no standalone `verify_guardian` procedure;
/// guardian verification is internal to `auth_tx_guarded_multisig`.
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
    /// These roots are deterministic based on the upstream MASM bytecode.
    pub fn root(&self) -> Word {
        match self {
            // update_signers_and_threshold
            ProcedureName::UpdateSigners => procedure_root_word(
                "0xe60215c664714037ad08811093b3685a6ace65c78351263473298cce9c7600e3",
            ),
            // set_procedure_threshold
            ProcedureName::UpdateProcedureThreshold => procedure_root_word(
                "0x9bee1ea89c844874d7f3c63bba52b277a429679028dc3a4e27c54db6cf4f158d",
            ),
            // auth_tx_guarded_multisig
            ProcedureName::AuthTx => procedure_root_word(
                "0xd7b760e20ccbf6f8428538a155f2ef636326b1fcf246c3a34da2cd3a73de77cd",
            ),
            // update_guardian_public_key
            ProcedureName::UpdateGuardian => procedure_root_word(
                "0x0a614ff7c81a561cbd2a4c2d9482031a7a841ca5de33349daed23a9d871b3675",
            ),
            // BasicWallet::move_asset_to_note
            ProcedureName::SendAsset => procedure_root_word(
                "0xfb1c73d10de1954e9e8948964e3e77cf4e33759d2e012cb00eb10c50f2974eb4",
            ),
            // BasicWallet::receive_asset
            ProcedureName::ReceiveAsset => procedure_root_word(
                "0x6170fd6d682d91777b551fd866258f43cc657f1291f8f071500f4e56e9c153da",
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

    /// Custody-critical guard: each hardcoded root MUST match the live upstream
    /// `AuthGuardedMultisig` / `BasicWallet` procedure root. A mismatch means a
    /// per-procedure threshold override would be stored under the wrong key and
    /// silently ignored at authentication time.
    #[test]
    fn procedure_roots_match_upstream_component() {
        use miden_standards::account::auth::AuthGuardedMultisig;
        use miden_standards::account::wallets::BasicWallet;

        let auth_code = AuthGuardedMultisig::code();
        let upstream_root = |masm_name: &str| -> Word {
            let export = auth_code
                .exports()
                .find(|e| e.path.to_string().rsplit("::").next() == Some(masm_name))
                .unwrap_or_else(|| panic!("upstream procedure `{masm_name}` not found"));
            auth_code
                .get_procedure_root_by_path(&*export.path)
                .expect("root by path")
                .into()
        };

        assert_eq!(
            ProcedureName::UpdateSigners.root(),
            upstream_root("update_signers_and_threshold")
        );
        assert_eq!(
            ProcedureName::UpdateProcedureThreshold.root(),
            upstream_root("set_procedure_threshold")
        );
        assert_eq!(
            ProcedureName::AuthTx.root(),
            upstream_root("auth_tx_guarded_multisig")
        );
        assert_eq!(
            ProcedureName::UpdateGuardian.root(),
            upstream_root("update_guardian_public_key")
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

    #[test]
    fn parse_unknown_returns_error() {
        let result: Result<ProcedureName, _> = "unknown_proc".parse();
        assert!(result.is_err());
    }
}
