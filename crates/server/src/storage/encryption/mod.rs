//! Application-layer encryption of sensitive storage payloads.
//!
//! Sensitive payloads (account state, delta/proposal payloads) are
//! authenticated-encrypted into a self-describing [`envelope::Envelope`] before
//! they reach the concrete backend and decrypted on read, so layers above the
//! storage boundary see unchanged objects. Routing/index fields stay plaintext.

pub(crate) mod cipher;
pub(crate) mod decorator;
pub(crate) mod envelope;
pub(crate) mod key_provider;
pub(crate) mod marker;

/// Whether a storage read error describes a single corrupt record — a
/// payload that is not an envelope, an unsupported envelope, a bad
/// nonce, or a failed AEAD check — as opposed to a systemic failure
/// (key provider unavailable, unknown key id, I/O). Matches the
/// `Display` strings of [`cipher::CipherError`]'s per-record variants;
/// keep the two in sync. Callers that aggregate across many records use
/// it to report one bad row as explicit coverage while treating
/// anything else as a reason to keep their previous result.
pub fn is_record_corruption(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("not an encryption envelope")
        || lower.contains("unsupported envelope")
        || lower.contains("envelope nonce is malformed")
        || lower.contains("decryption failed")
}
