//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo test --package miden-confidential-contracts log_procedure_roots -- --nocapture`

use miden_protocol::{Felt, Word};

/// Procedure names that can be used for threshold overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcedureName {
    UpdateSigners,
    AuthTx,
    UpdatePsm,
    VerifyPsm,
    SendAsset,
    ReceiveAsset,
}

impl ProcedureName {
    /// Get the procedure root for this procedure name.
    ///
    /// These roots are deterministic based on the MASM bytecode.
    pub fn root(&self) -> Word {
        match self {
            // Multisig component procedures
            ProcedureName::UpdateSigners => {
                word_from_hex("26905086c572765c44337002a961a4d69514889d7e55686dc31c00383b614c47")
            }
            ProcedureName::AuthTx => {
                word_from_hex("2bc7664a9dd47b36e7c8b8c3df03412798e4410173f36acfe03d191a38add053")
            }
            // PSM component procedures
            ProcedureName::UpdatePsm => {
                word_from_hex("26ec27195f1fd3eb622b851dfc9eab038bca87522cfc7ec209bfe507682303b1")
            }
            ProcedureName::VerifyPsm => {
                word_from_hex("878a1f70568f2c3798cfa0163fc085fa350f92c1b4a8fe78a605613cc27f7230")
            }
            // BasicWallet procedures
            ProcedureName::SendAsset => {
                word_from_hex("d6c130dba13c67ac4733915f24bea9d19f517f51a65c74ded7bcd27e066b400e")
            }
            ProcedureName::ReceiveAsset => {
                word_from_hex("016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f")
            }
        }
    }

    /// Get all available procedure names.
    pub fn all() -> &'static [ProcedureName] {
        &[
            ProcedureName::UpdateSigners,
            ProcedureName::AuthTx,
            ProcedureName::UpdatePsm,
            ProcedureName::VerifyPsm,
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
            ProcedureName::AuthTx => write!(f, "auth_tx"),
            ProcedureName::UpdatePsm => write!(f, "update_psm"),
            ProcedureName::VerifyPsm => write!(f, "verify_psm"),
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
            "auth_tx" => Ok(ProcedureName::AuthTx),
            "update_psm" => Ok(ProcedureName::UpdatePsm),
            "verify_psm" => Ok(ProcedureName::VerifyPsm),
            "send_asset" => Ok(ProcedureName::SendAsset),
            "receive_asset" => Ok(ProcedureName::ReceiveAsset),
            _ => Err(format!("unknown procedure name: {}", s)),
        }
    }
}

/// Convert a 64-char hex string to Word (big-endian format).
///
/// The hex string represents 4 field elements in big-endian order.
fn word_from_hex(hex_str: &str) -> Word {
    let bytes = hex::decode(hex_str).expect("invalid hex in procedure root constant");
    assert_eq!(bytes.len(), 32, "procedure root must be 32 bytes");

    // The hex is in big-endian order: [e3_bytes, e2_bytes, e1_bytes, e0_bytes]
    // Word is [e0, e1, e2, e3] where each element is a Felt
    let e3 = u64::from_be_bytes(bytes[0..8].try_into().unwrap());
    let e2 = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let e1 = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let e0 = u64::from_be_bytes(bytes[24..32].try_into().unwrap());

    Word::from([Felt::new(e0), Felt::new(e1), Felt::new(e2), Felt::new(e3)])
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
        // Just verify we can get roots without panicking
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
