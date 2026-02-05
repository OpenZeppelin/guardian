//! Well-known procedure roots for multisig accounts.
//!
//! Extracted from: `cargo test --package miden-confidential-contracts log_procedure_roots -- --nocapture`

use miden_objects::{Felt, Word};
use private_state_manager_shared::SignatureScheme;

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
        self.root_for_scheme(SignatureScheme::Falcon)
    }

    /// Get the procedure root for this procedure name and signature scheme.
    pub fn root_for_scheme(&self, scheme: SignatureScheme) -> Word {
        match (scheme, self) {
            // Multisig component procedures
            (SignatureScheme::Falcon, ProcedureName::UpdateSigners) => {
                word_from_hex("26905086c572765c44337002a961a4d69514889d7e55686dc31c00383b614c47")
            }
            (SignatureScheme::Falcon, ProcedureName::AuthTx) => {
                word_from_hex("2bc7664a9dd47b36e7c8b8c3df03412798e4410173f36acfe03d191a38add053")
            }
            // PSM component procedures
            (SignatureScheme::Falcon, ProcedureName::UpdatePsm) => {
                word_from_hex("26ec27195f1fd3eb622b851dfc9eab038bca87522cfc7ec209bfe507682303b1")
            }
            (SignatureScheme::Falcon, ProcedureName::VerifyPsm) => {
                word_from_hex("878a1f70568f2c3798cfa0163fc085fa350f92c1b4a8fe78a605613cc27f7230")
            }
            // BasicWallet procedures
            (SignatureScheme::Falcon, ProcedureName::SendAsset) => {
                word_from_hex("d6c130dba13c67ac4733915f24bea9d19f517f51a65c74ded7bcd27e066b400e")
            }
            (SignatureScheme::Falcon, ProcedureName::ReceiveAsset) => {
                word_from_hex("016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f")
            }
            // ECDSA component procedures (to be updated from ECDSA MASM compilation)
            (SignatureScheme::Ecdsa, ProcedureName::UpdateSigners) => {
                word_from_hex("930f9ea86d33c7b3b2d23c4e9ac2492add461359344ed25b1acd38f3713dd79c")
            }
            (SignatureScheme::Ecdsa, ProcedureName::AuthTx) => {
                word_from_hex("2bc7664a9dd47b36e7c8b8c3df03412798e4410173f36acfe03d191a38add053")
            }
            (SignatureScheme::Ecdsa, ProcedureName::UpdatePsm) => {
                word_from_hex("26ec27195f1fd3eb622b851dfc9eab038bca87522cfc7ec209bfe507682303b1")
            }
            (SignatureScheme::Ecdsa, ProcedureName::VerifyPsm) => {
                word_from_hex("beb9e08a83eca030968f2f137b5673136f21bc16c82fd408c2ce2495ccbdcd15")
            }
            (SignatureScheme::Ecdsa, ProcedureName::SendAsset) => {
                word_from_hex("d6c130dba13c67ac4733915f24bea9d19f517f51a65c74ded7bcd27e066b400e")
            }
            (SignatureScheme::Ecdsa, ProcedureName::ReceiveAsset) => {
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
