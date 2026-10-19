//! Session grants and the account-less session messages (issue #219).
//!
//! A wallet signs a [`SessionGrant`] once to delegate request signing to a
//! client-held P-256 key, the delegated signer. The grant states every value
//! it binds in the clear: EIP-712 wallets display them field by field (see
//! [`crate::auth_request_eip712::session_digest`]) and Falcon or raw ECDSA
//! wallets sign a domain-separated RPO digest of the same fields.

use chrono::DateTime;
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::{Felt, Word};
use std::sync::OnceLock;

/// Domain-tag byte string. The 4-felt RPO digest of these bytes is the first
/// word of every [`SessionGrant`] digest, so a grant signature cannot validate
/// as an `AuthRequestMessage` (7-felt layout) or `LookupAuthMessage` (9-felt
/// layout) signature, or the other way round.
///
/// Future incompatible layout changes MUST bump the version segment
/// (e.g. `guardian.session.v2`) rather than mutate this constant.
const DOMAIN_TAG_BYTES: &[u8] = b"guardian.session.v1";

/// Domain tag for [`SessionLogoutMessage`].
const LOGOUT_DOMAIN_TAG_BYTES: &[u8] = b"guardian.session.logout.v1";

/// Domain tag for [`SessionRevokeAllMessage`].
const REVOKE_ALL_DOMAIN_TAG_BYTES: &[u8] = b"guardian.session.revoke_all.v1";

/// The scope every v1 grant states, shown by the wallet: the delegated signer
/// acts for every account its wallet key cosigns on this Guardian, including
/// accounts the key is added to later, until the grant expires.
pub const SESSION_GRANT_SCOPE: &str =
    "Every account this signer cosigns on this Guardian, now or later, until this grant expires";

/// Length of a SEC1-compressed P-256 public key.
pub const SESSION_PUBLIC_KEY_LEN: usize = 33;

/// Longest accepted `origin`.
pub const SESSION_ORIGIN_MAX_LEN: usize = 256;

/// Cached session-grant domain tag.
pub fn session_domain_tag() -> Word {
    static TAG: OnceLock<Word> = OnceLock::new();
    *TAG.get_or_init(|| crate::felt::domain_tag_word(DOMAIN_TAG_BYTES))
}

/// Cached session-logout domain tag.
pub fn session_logout_domain_tag() -> Word {
    static TAG: OnceLock<Word> = OnceLock::new();
    *TAG.get_or_init(|| crate::felt::domain_tag_word(LOGOUT_DOMAIN_TAG_BYTES))
}

/// Cached session-revoke-all domain tag.
pub fn session_revoke_all_domain_tag() -> Word {
    static TAG: OnceLock<Word> = OnceLock::new();
    *TAG.get_or_init(|| crate::felt::domain_tag_word(REVOKE_ALL_DOMAIN_TAG_BYTES))
}

/// Encodes bytes as `[len, u32_le_chunk...]` felts. Every 4-byte chunk is below
/// the field modulus and the length prefix removes padding ambiguity, so the
/// encoding is injective for arbitrary input, including public-key bytes.
fn felts_from_bytes(bytes: &[u8]) -> Vec<Felt> {
    let mut elements = Vec::with_capacity(1 + bytes.len().div_ceil(4));
    elements.push(Felt::from(bytes.len() as u32));
    for chunk in bytes.chunks(4) {
        let mut chunk_bytes = [0u8; 4];
        chunk_bytes[..chunk.len()].copy_from_slice(chunk);
        elements.push(Felt::from(u32::from_le_bytes(chunk_bytes)));
    }
    elements
}

fn hash_bytes(bytes: &[u8]) -> Word {
    Rpo256::hash_elements(&felts_from_bytes(bytes))
}

/// The canonical readable expiry, `YYYY-MM-DD HH:MM:SS UTC`, derived from
/// `expires_at` (Unix seconds). `None` when out of the representable range.
pub fn format_utc_seconds(unix_seconds: u64) -> Option<String> {
    let seconds = i64::try_from(unix_seconds).ok()?;
    DateTime::from_timestamp(seconds, 0)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}

/// Parses a SEC1-compressed P-256 public key, rejecting points off the curve.
pub fn parse_session_public_key(bytes: &[u8]) -> Result<[u8; SESSION_PUBLIC_KEY_LEN], String> {
    let key: [u8; SESSION_PUBLIC_KEY_LEN] = bytes.try_into().map_err(|_| {
        format!(
            "Session public key must be {SESSION_PUBLIC_KEY_LEN} bytes (SEC1 compressed P-256), got {}",
            bytes.len()
        )
    })?;
    if key[0] != 0x02 && key[0] != 0x03 {
        return Err("Session public key must be a SEC1 compressed P-256 point".to_string());
    }
    p256::ecdsa::VerifyingKey::from_sec1_bytes(&key)
        .map_err(|_| "Session public key is not a valid P-256 point".to_string())?;
    Ok(key)
}

/// One-time wallet authorization of a client-held P-256 delegated signer.
///
/// The wallet signs, in the clear:
///
/// - the commitment of the wallet key that signs the grant;
/// - the delegated signer's public key;
/// - `origin`: the website that asked for the grant, shown to the user by the
///   wallet (empty for clients outside a browser); informational, Guardian
///   does not check it against requests;
/// - `issued_at` and `expires_at` (Unix seconds), plus the readable expiry
///   [`format_utc_seconds`] derives from `expires_at`;
/// - the scope, always [`SESSION_GRANT_SCOPE`];
/// - the commitment of this Guardian's ACK key for the wallet's scheme;
/// - the Miden network.
///
/// The readable expiry and the scope are derived, never sent: a wallet that
/// displayed anything else signed a different digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGrant {
    signer_commitment: Word,
    session_public_key: [u8; SESSION_PUBLIC_KEY_LEN],
    origin: String,
    issued_at: u64,
    expires_at: u64,
    guardian_commitment: Word,
    network: String,
}

impl SessionGrant {
    /// `session_public_key` must come from [`parse_session_public_key`].
    /// `issued_at` and `expires_at` are Unix seconds.
    pub fn new(
        signer_commitment: Word,
        session_public_key: &[u8; SESSION_PUBLIC_KEY_LEN],
        origin: impl Into<String>,
        issued_at: u64,
        expires_at: u64,
        guardian_commitment: Word,
        network: impl Into<String>,
    ) -> Result<Self, String> {
        let origin = origin.into();
        if origin.len() > SESSION_ORIGIN_MAX_LEN {
            return Err(format!(
                "Session grant origin is longer than {SESSION_ORIGIN_MAX_LEN} bytes"
            ));
        }
        if expires_at <= issued_at {
            return Err("Session grant expires_at must be after issued_at".to_string());
        }
        if format_utc_seconds(expires_at).is_none() {
            return Err("Session grant expires_at is out of range".to_string());
        }
        Ok(Self {
            signer_commitment,
            session_public_key: *session_public_key,
            origin,
            issued_at,
            expires_at,
            guardian_commitment,
            network: network.into(),
        })
    }

    pub fn signer_commitment(&self) -> Word {
        self.signer_commitment
    }

    pub fn session_public_key(&self) -> &[u8; SESSION_PUBLIC_KEY_LEN] {
        &self.session_public_key
    }

    /// The website that asked for the grant; empty outside a browser.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Unix seconds.
    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }

    /// Unix seconds.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// The readable expiry the wallet displays next to `expires_at`.
    pub fn expires(&self) -> String {
        format_utc_seconds(self.expires_at).expect("validated in SessionGrant::new")
    }

    pub fn guardian_commitment(&self) -> Word {
        self.guardian_commitment
    }

    pub fn network(&self) -> &str {
        &self.network
    }

    /// Digest signed by Falcon and raw ECDSA wallets.
    ///
    /// Layout:
    /// ```text
    /// RPO256_hash([
    ///   DOMAIN_TAG_W0..W3,
    ///   signer_commitment_W0..W3,
    ///   len(session_public_key), session_public_key_u32_le_chunks...,
    ///   H(origin)_W0..W3,
    ///   issued_at_felt, expires_at_felt,
    ///   H(expires)_W0..W3,
    ///   H(scope)_W0..W3,
    ///   guardian_commitment_W0..W3,
    ///   H(network)_W0..W3,
    /// ])
    /// ```
    /// where `H(s)` is the RPO hash of `[len, u32_le_chunks...]` of the UTF-8
    /// bytes. EIP-712 wallets sign the same fields as typed data instead, see
    /// [`crate::auth_request_eip712::session_digest`].
    pub fn to_word(&self) -> Word {
        let mut elements = Vec::with_capacity(40);
        elements.extend_from_slice(session_domain_tag().as_elements());
        elements.extend_from_slice(self.signer_commitment.as_elements());
        elements.extend(felts_from_bytes(&self.session_public_key));
        elements.extend_from_slice(hash_bytes(self.origin.as_bytes()).as_elements());
        elements.push(crate::felt::felt_from_u64_reduced(self.issued_at));
        elements.push(crate::felt::felt_from_u64_reduced(self.expires_at));
        elements.extend_from_slice(hash_bytes(self.expires().as_bytes()).as_elements());
        elements.extend_from_slice(hash_bytes(SESSION_GRANT_SCOPE.as_bytes()).as_elements());
        elements.extend_from_slice(self.guardian_commitment.as_elements());
        elements.extend_from_slice(hash_bytes(self.network.as_bytes()).as_elements());
        Rpo256::hash_elements(&elements)
    }
}

/// Session-key-signed request to revoke the session it belongs to. Account-less
/// and replay-bounded by the server clock skew window on `timestamp_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLogoutMessage {
    session_public_key: [u8; SESSION_PUBLIC_KEY_LEN],
    timestamp_ms: i64,
}

impl SessionLogoutMessage {
    pub fn new(session_public_key: &[u8; SESSION_PUBLIC_KEY_LEN], timestamp_ms: i64) -> Self {
        Self {
            session_public_key: *session_public_key,
            timestamp_ms,
        }
    }

    /// Layout:
    /// ```text
    /// RPO256_hash([
    ///   LOGOUT_DOMAIN_TAG_W0..W3,
    ///   timestamp_ms_felt,
    ///   len(session_public_key), session_public_key_u32_le_chunks...,
    /// ])
    /// ```
    pub fn to_word(&self) -> Word {
        let mut elements = Vec::with_capacity(15);
        elements.extend_from_slice(session_logout_domain_tag().as_elements());
        elements.push(crate::felt::felt_from_u64_reduced(self.timestamp_ms as u64));
        elements.extend(felts_from_bytes(&self.session_public_key));
        Rpo256::hash_elements(&elements)
    }
}

/// Wallet-signed request to revoke every session of a signer on this Guardian.
/// Account-less and replay-bounded by the server clock skew window on
/// `timestamp_ms`. EIP-712 wallets sign the same two fields as typed data, see
/// [`crate::auth_request_eip712::revoke_all_digest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRevokeAllMessage {
    signer_commitment: Word,
    timestamp_ms: i64,
}

impl SessionRevokeAllMessage {
    pub fn new(signer_commitment: Word, timestamp_ms: i64) -> Self {
        Self {
            signer_commitment,
            timestamp_ms,
        }
    }

    pub fn signer_commitment(&self) -> Word {
        self.signer_commitment
    }

    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    /// Layout:
    /// ```text
    /// RPO256_hash([
    ///   REVOKE_ALL_DOMAIN_TAG_W0..W3,
    ///   timestamp_ms_felt,
    ///   signer_commitment_W0..W3,
    /// ])
    /// ```
    pub fn to_word(&self) -> Word {
        let mut elements = Vec::with_capacity(9);
        elements.extend_from_slice(session_revoke_all_domain_tag().as_elements());
        elements.push(crate::felt::felt_from_u64_reduced(self.timestamp_ms as u64));
        elements.extend_from_slice(self.signer_commitment.as_elements());
        Rpo256::hash_elements(&elements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_request_message::AuthRequestMessage;
    use crate::auth_request_payload::AuthRequestPayload;
    use crate::lookup_auth_message::LookupAuthMessage;
    use miden_protocol::account::{AccountId, AccountIdVersion, AccountType, AssetCallbackFlag};

    fn session_key(seed: u8) -> [u8; SESSION_PUBLIC_KEY_LEN] {
        let mut key = [seed; SESSION_PUBLIC_KEY_LEN];
        key[0] = 0x02;
        key
    }

    fn sample_grant() -> SessionGrant {
        SessionGrant::new(
            Word::from([1u32, 2, 3, 4]),
            &session_key(7),
            "https://multisig.example",
            1_791_280_800,
            1_791_309_600,
            Word::from([5u32, 6, 7, 8]),
            "devnet",
        )
        .expect("valid grant")
    }

    #[test]
    fn rejects_malformed_session_keys() {
        let valid = crate::session_key::SessionKey::from_bytes(&[1; 32])
            .unwrap()
            .public_key();
        assert!(parse_session_public_key(&valid).is_ok());
        assert!(parse_session_public_key(&valid[..32]).is_err());
        let mut uncompressed_prefix = valid;
        uncompressed_prefix[0] = 0x04;
        assert!(parse_session_public_key(&uncompressed_prefix).is_err());
        assert!(
            parse_session_public_key(&session_key(1)).is_err(),
            "off-curve"
        );
    }

    #[test]
    fn formats_expiry_as_utc() {
        assert_eq!(format_utc_seconds(0).unwrap(), "1970-01-01 00:00:00 UTC");
        assert_eq!(
            format_utc_seconds(951_782_400).unwrap(),
            "2000-02-29 00:00:00 UTC"
        );
        assert_eq!(
            format_utc_seconds(1_791_309_600).unwrap(),
            "2026-10-06 18:00:00 UTC"
        );
        assert_eq!(format_utc_seconds(u64::MAX), None);
    }

    #[test]
    fn rejects_empty_lifetimes_and_long_origins() {
        let grant = |origin: &str, issued_at, expires_at| {
            SessionGrant::new(
                Word::default(),
                &session_key(1),
                origin,
                issued_at,
                expires_at,
                Word::default(),
                "devnet",
            )
        };
        assert!(grant("", 100, 100).is_err());
        assert!(grant("", 101, 100).is_err());
        assert!(grant("", 100, u64::MAX).is_err());
        assert!(grant("", 100, 101).is_ok());
        let long_host = format!("https://{}", "a".repeat(SESSION_ORIGIN_MAX_LEN - 8));
        assert!(grant(&long_host, 100, 101).is_ok());
        assert!(grant(&format!("{long_host}a"), 100, 101).is_err());
    }

    #[test]
    fn digest_binds_every_field() {
        let base = sample_grant();
        let variants: [fn(&mut SessionGrant); 7] = [
            |g| g.signer_commitment = Word::from([9u32, 2, 3, 4]),
            |g| g.session_public_key = session_key(8),
            |g| g.origin = "https://phishing.example".to_string(),
            |g| g.issued_at += 1,
            |g| g.expires_at += 1,
            |g| g.guardian_commitment = Word::from([9u32, 6, 7, 8]),
            |g| g.network = "testnet".to_string(),
        ];
        for change in variants {
            let mut variant = base.clone();
            change(&mut variant);
            assert_ne!(variant.to_word(), base.to_word());
        }
    }

    #[test]
    fn byte_encoding_is_length_prefixed() {
        assert_ne!(hash_bytes(b"ab"), hash_bytes(b"ab\0"));
        assert_ne!(hash_bytes(b""), hash_bytes(b"\0"));
    }

    #[test]
    fn session_messages_are_distinct_from_other_guardian_messages() {
        let grant = sample_grant();
        let account_id = AccountId::dummy(
            [0x8a; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let request = AuthRequestMessage::new(
            account_id,
            1_791_280_800_000,
            AuthRequestPayload::from_bytes(&grant.to_word().as_bytes()),
        )
        .to_word();
        let lookup = LookupAuthMessage::new(1_791_280_800_000, grant.signer_commitment()).to_word();
        let revoke_all =
            SessionRevokeAllMessage::new(grant.signer_commitment(), 1_791_280_800_000).to_word();

        assert_ne!(grant.to_word(), request);
        assert_ne!(grant.to_word(), lookup);
        assert_ne!(revoke_all, lookup, "same fields, different domain tag");
        let tags = [
            session_domain_tag(),
            session_logout_domain_tag(),
            session_revoke_all_domain_tag(),
        ];
        assert_ne!(tags[0], tags[1]);
        assert_ne!(tags[1], tags[2]);
        assert_ne!(tags[0], tags[2]);
    }

    #[test]
    fn logout_digest_changes_with_timestamp_and_key() {
        let base = SessionLogoutMessage::new(&session_key(7), 1_000).to_word();
        assert_ne!(
            base,
            SessionLogoutMessage::new(&session_key(7), 1_001).to_word()
        );
        assert_ne!(
            base,
            SessionLogoutMessage::new(&session_key(8), 1_000).to_word()
        );
    }

    #[test]
    fn revoke_all_digest_changes_with_timestamp_and_signer() {
        let signer = Word::from([1u32, 2, 3, 4]);
        let base = SessionRevokeAllMessage::new(signer, 1_000).to_word();
        assert_ne!(base, SessionRevokeAllMessage::new(signer, 1_001).to_word());
        assert_ne!(
            base,
            SessionRevokeAllMessage::new(Word::from([2u32, 2, 3, 4]), 1_000).to_word()
        );
    }
}
