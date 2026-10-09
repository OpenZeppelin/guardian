use crate::session_grant::{SESSION_GRANT_SCOPE, SessionGrant, SessionRevokeAllMessage};
use miden_protocol::Word;
use miden_protocol::crypto::hash::keccak::Keccak256;

const DOMAIN_TYPE: &str = "EIP712Domain(string name,string version)";
const REQUEST_TYPE: &str = "GuardianRequest(bytes32 requestHash)";
const LOOKUP_TYPE: &str = "GuardianLookup(bytes32 lookupHash)";
const SESSION_TYPE: &str = "GuardianSession(bytes32 signer,bytes sessionKey,string origin,uint64 issuedAt,uint64 expiresAt,string expires,string scope,bytes32 guardianKey,string network)";
const REVOKE_ALL_TYPE: &str = "GuardianSessionRevokeAll(bytes32 signer,uint64 timestamp)";
const REQUEST_DOMAIN_NAME: &str = "Guardian Request";
const LOOKUP_DOMAIN_NAME: &str = "Guardian Lookup";
const SESSION_DOMAIN_NAME: &str = "Guardian Session";
const DOMAIN_VERSION: &str = "1";
const EIP191_PREFIX: u8 = 0x19;
const EIP712_VERSION: u8 = 0x01;

fn keccak(bytes: &[u8]) -> [u8; 32] {
    Keccak256::hash(bytes).into()
}

/// ABI encoding of a `uint64` struct member.
fn uint64_word(value: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

/// EIP-712 digest of the existing account, timestamp, and payload-bound request hash.
pub fn request_digest(request_hash: Word) -> [u8; 32] {
    typed_digest(REQUEST_DOMAIN_NAME, REQUEST_TYPE, request_hash)
}

/// EIP-712 digest of the account-less, timestamp- and commitment-bound lookup hash.
pub fn lookup_digest(lookup_hash: Word) -> [u8; 32] {
    typed_digest(LOOKUP_DOMAIN_NAME, LOOKUP_TYPE, lookup_hash)
}

/// EIP-712 digest of a session grant. Unlike the request and lookup types, the
/// grant is signed as readable typed data so the wallet displays every field
/// it binds instead of a single hash:
///
/// ```text
/// GuardianSession(bytes32 signer,bytes sessionKey,string origin,uint64 issuedAt,uint64 expiresAt,string expires,string scope,bytes32 guardianKey,string network)
/// ```
pub fn session_digest(grant: &SessionGrant) -> [u8; 32] {
    let mut message = [0u8; 320];
    message[..32].copy_from_slice(&keccak(SESSION_TYPE.as_bytes()));
    message[32..64].copy_from_slice(&grant.signer_commitment().as_bytes());
    message[64..96].copy_from_slice(&keccak(grant.session_public_key()));
    message[96..128].copy_from_slice(&keccak(grant.origin().as_bytes()));
    message[128..160].copy_from_slice(&uint64_word(grant.issued_at()));
    message[160..192].copy_from_slice(&uint64_word(grant.expires_at()));
    message[192..224].copy_from_slice(&keccak(grant.expires().as_bytes()));
    message[224..256].copy_from_slice(&keccak(SESSION_GRANT_SCOPE.as_bytes()));
    message[256..288].copy_from_slice(&grant.guardian_commitment().as_bytes());
    message[288..].copy_from_slice(&keccak(grant.network().as_bytes()));
    finalize(SESSION_DOMAIN_NAME, keccak(&message))
}

/// EIP-712 digest of a wallet-signed revoke-all, readable like the grant:
///
/// ```text
/// GuardianSessionRevokeAll(bytes32 signer,uint64 timestamp)
/// ```
///
/// `timestamp` is `timestamp_ms`. A negative timestamp has no `uint64`
/// encoding and is an error, as in the TypeScript SDK.
pub fn revoke_all_digest(message: &SessionRevokeAllMessage) -> Result<[u8; 32], String> {
    let timestamp = u64::try_from(message.timestamp_ms())
        .map_err(|_| "Revoke-all timestamp must not be negative".to_string())?;
    let mut encoded = [0u8; 96];
    encoded[..32].copy_from_slice(&keccak(REVOKE_ALL_TYPE.as_bytes()));
    encoded[32..64].copy_from_slice(&message.signer_commitment().as_bytes());
    encoded[64..].copy_from_slice(&uint64_word(timestamp));
    Ok(finalize(SESSION_DOMAIN_NAME, keccak(&encoded)))
}

fn typed_digest(domain_name: &str, message_type: &str, message_hash: Word) -> [u8; 32] {
    let mut request = [0u8; 64];
    request[..32].copy_from_slice(&keccak(message_type.as_bytes()));
    request[32..].copy_from_slice(&message_hash.as_bytes());
    finalize(domain_name, keccak(&request))
}

fn finalize(domain_name: &str, struct_hash: [u8; 32]) -> [u8; 32] {
    let mut domain = [0u8; 96];
    domain[..32].copy_from_slice(&keccak(DOMAIN_TYPE.as_bytes()));
    domain[32..64].copy_from_slice(&keccak(domain_name.as_bytes()));
    domain[64..].copy_from_slice(&keccak(DOMAIN_VERSION.as_bytes()));
    let domain_separator = keccak(&domain);

    let mut preimage = [0u8; 66];
    preimage[..2].copy_from_slice(&[EIP191_PREFIX, EIP712_VERSION]);
    preimage[2..34].copy_from_slice(&domain_separator);
    preimage[34..].copy_from_slice(&struct_hash);
    keccak(&preimage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::FromHex;

    #[test]
    fn matches_viem_guardian_session_digest() {
        // viem `hashTypedData` over the readable `GuardianSession` fields: the
        // `devnet_8h` vector of tests/fixtures/session_grant_vectors.json.
        let session_public_key = crate::session_grant::parse_session_public_key(
            &hex::decode("026ff03b949241ce1dadd43519e6960e0a85b41a69a05c328103aa2bce1594ca16")
                .unwrap(),
        )
        .unwrap();
        let grant = SessionGrant::new(
            Word::from([1u32, 2, 3, 4]),
            &session_public_key,
            "https://multisig.miden.xyz",
            1_791_280_800,
            1_791_309_600,
            Word::from([5u32, 6, 7, 8]),
            "devnet",
        )
        .unwrap();
        assert_eq!(grant.expires(), "2026-10-06 18:00:00 UTC");
        assert_eq!(
            hex::encode(session_digest(&grant)),
            "5175c632ad4927e18e6bc081a2238a7f9168fa852a188fad05ad3c6eddcfab62"
        );
    }

    #[test]
    fn matches_viem_guardian_session_revoke_all_digest() {
        // viem `hashTypedData` over `GuardianSessionRevokeAll`: the `typical`
        // revoke-all vector of tests/fixtures/session_grant_vectors.json.
        let message = SessionRevokeAllMessage::new(
            Word::from([0xdeadbeefu32, 0xcafebabe, 0x12345678, 0x87654321]),
            1_791_280_800_000,
        );
        assert_eq!(
            hex::encode(revoke_all_digest(&message).unwrap()),
            "f635fa26e9080362ef8d9fba703c8060d8482f5b840327b206ef83f566467f9b"
        );
    }

    #[test]
    fn rejects_a_negative_revoke_all_timestamp() {
        let message = SessionRevokeAllMessage::new(Word::default(), -1);
        assert!(revoke_all_digest(&message).is_err());
    }

    #[test]
    fn matches_viem_guardian_request_digest() {
        let request_hash =
            Word::from_hex("0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        assert_eq!(
            hex::encode(request_digest(request_hash)),
            "a37b07bbe236b49fd5be224928bd181a37622afe2caa3295e33b5482c08636d5"
        );
    }
}
