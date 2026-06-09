use sha2::{Digest, Sha256};

/// SHA-256 digest of a session token, used as the storage-map key so the
/// plaintext token is never retained beyond the issuing request. A coredump
/// or heap inspection of the long-lived session map yields no usable tokens.
pub(crate) fn session_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}
