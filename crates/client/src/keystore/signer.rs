use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_rpo::Signature;

/// Signing boundary for PSM authentication and multisig proposal workflows.
pub trait Signer: Send + Sync {
    /// Returns the signer's commitment as a Word.
    fn commitment(&self) -> Word;

    /// Returns the signer's commitment as a hex string with 0x prefix.
    fn commitment_hex(&self) -> String;

    /// Returns the signer's public key as a hex string with 0x prefix.
    fn public_key_hex(&self) -> String;

    /// Signs the provided word.
    fn sign_word(&self, message: Word) -> Signature;
}
