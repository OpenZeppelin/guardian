//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo run --example procedure_roots -p miden-multisig-client -- --json`

use miden_protocol::Word;

/// Procedure names that can be used for threshold overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcedureName {
    UpdateSigners,
    UpdateProcedureThreshold,
    AuthTx,
    UpdateGuardian,
    VerifyGuardian,
    SendAsset,
    ReceiveAsset,
}

impl ProcedureName {
    /// Get the procedure root for this procedure name.
    ///
    /// These roots are deterministic based on the MASM bytecode.
    pub fn root(&self) -> Word {
        match self {
            ProcedureName::UpdateSigners => procedure_root_word(
                "0x3a5c07142f2501edd45b93db0d5c97d88093d92defc4c5d01f09840db7951cd8",
            ),
            ProcedureName::UpdateProcedureThreshold => procedure_root_word(
                "0xec74c4b96ce593c11017ae54dec9c0ae5e0d242e8b3074eb3908d961300aed67",
            ),
            ProcedureName::AuthTx => procedure_root_word(
                "0x0708020dce7b91b61116e3eb27e5d686e129a83df3c540e0a7693b4523814e72",
            ),
            ProcedureName::UpdateGuardian => procedure_root_word(
                "0xeceb1f2c2d7d20312dbaf091e9a27a2b63f9fcba120948043069793a5715bc96",
            ),
            ProcedureName::VerifyGuardian => procedure_root_word(
                "0xff45881bc28ced66a55102c5410d6b2253ca3ac757daf308fb72d1b0af76fcf9",
            ),
            ProcedureName::SendAsset => procedure_root_word(
                "0xfb1c73d10de1954e9e8948964e3e77cf4e33759d2e012cb00eb10c50f2974eb4",
            ),
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
            ProcedureName::VerifyGuardian,
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
            ProcedureName::VerifyGuardian => write!(f, "verify_guardian"),
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
            "verify_guardian" => Ok(ProcedureName::VerifyGuardian),
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
    fn procedure_roots_match_compiled_account() {
        use miden_confidential_contracts::multisig_guardian::{
            MultisigGuardianBuilder, MultisigGuardianConfig,
        };
        use miden_protocol::{Felt, Word};

        let commit = |s: u64| {
            Word::from([
                Felt::new_unchecked(s),
                Felt::new_unchecked(s + 1),
                Felt::new_unchecked(s + 2),
                Felt::new_unchecked(s + 3),
            ])
        };
        let config = MultisigGuardianConfig::new(1, vec![commit(1)], commit(10));
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([42u8; 32])
            .build()
            .expect("build account");

        let roots: Vec<Word> = account
            .code()
            .procedures()
            .iter()
            .map(|p| *p.mast_root())
            .collect();

        assert_eq!(
            ProcedureName::UpdateSigners.root(),
            roots[0],
            "update_signers"
        );
        assert_eq!(
            ProcedureName::UpdateProcedureThreshold.root(),
            roots[1],
            "update_procedure_threshold"
        );
        assert_eq!(
            ProcedureName::UpdateGuardian.root(),
            roots[2],
            "update_guardian"
        );
        assert_eq!(ProcedureName::AuthTx.root(), roots[3], "auth_tx");
        assert_eq!(
            ProcedureName::VerifyGuardian.root(),
            roots[4],
            "verify_guardian"
        );
        assert_eq!(ProcedureName::SendAsset.root(), roots[5], "send_asset");
        assert_eq!(
            ProcedureName::ReceiveAsset.root(),
            roots[6],
            "receive_asset"
        );
    }

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

    #[test]
    fn parse_unknown_returns_error() {
        let result: Result<ProcedureName, _> = "unknown_proc".parse();
        assert!(result.is_err());
    }
}
