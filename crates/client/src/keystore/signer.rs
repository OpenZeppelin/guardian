use miden_protocol::Word;
use private_state_manager_shared::SignatureScheme;

/// Signing boundary for PSM authentication and multisig proposal workflows.
pub trait Signer: Send + Sync {
    /// Returns the signer's signature scheme.
    fn scheme(&self) -> SignatureScheme;

    /// Returns the signer's commitment as a Word.
    fn commitment(&self) -> Word;

    /// Returns the signer's commitment as a hex string with 0x prefix.
    fn commitment_hex(&self) -> String;

    /// Returns the signer's public key as a hex string with 0x prefix.
    fn public_key_hex(&self) -> String;

    /// Signs the provided word and returns the hex-encoded signature.
    fn sign_word_hex(&self, message: Word) -> String;
}
