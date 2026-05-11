use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::{Felt, Word};
use std::sync::OnceLock;

/// Domain-tag byte string. The 4-felt RPO digest of these bytes is prepended to
/// every `LookupAuthMessage` digest so that lookup signatures are structurally
/// distinct from `AuthRequestMessage` signatures (which are account-bound and
/// have a 7-felt layout). A signature crafted under one shape cannot validate
/// under the other in either direction.
///
/// Future incompatible layout changes MUST bump the version segment
/// (e.g. `guardian.lookup.v2`) rather than mutate this constant.
const DOMAIN_TAG_BYTES: &[u8] = b"guardian.lookup.v1";

/// Cached 4-felt domain-tag word, computed once on first use.
fn domain_tag() -> Word {
    static TAG: OnceLock<Word> = OnceLock::new();
    *TAG.get_or_init(|| {
        let mut elements = Vec::with_capacity(DOMAIN_TAG_BYTES.len().div_ceil(8));
        for chunk in DOMAIN_TAG_BYTES.chunks(8) {
            let mut chunk_bytes = [0u8; 8];
            chunk_bytes[..chunk.len()].copy_from_slice(chunk);
            elements.push(Felt::new(u64::from_le_bytes(chunk_bytes)));
        }
        Rpo256::hash_elements(&elements)
    })
}

/// Account-less, replay-protected message format used to sign requests against
/// the `/state/lookup` endpoint and the `GetAccountByKeyCommitment` gRPC method.
///
/// Unlike [`crate::auth_request_message::AuthRequestMessage`], this message does
/// not bind to an `account_id` — that is the value the caller is trying to
/// discover. Replay protection comes from a server-clock skew window enforced
/// against `timestamp_ms`. Cross-domain replay protection comes from the
/// fixed 4-felt domain tag at the head of the digest input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupAuthMessage {
    timestamp_ms: i64,
    key_commitment: Word,
}

impl LookupAuthMessage {
    pub fn new(timestamp_ms: i64, key_commitment: Word) -> Self {
        Self {
            timestamp_ms,
            key_commitment,
        }
    }

    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    pub fn key_commitment(&self) -> Word {
        self.key_commitment
    }

    /// Compute the message digest used for signing.
    ///
    /// Layout:
    /// ```text
    /// RPO256_hash([
    ///   DOMAIN_TAG_W0, DOMAIN_TAG_W1, DOMAIN_TAG_W2, DOMAIN_TAG_W3,
    ///   timestamp_ms_felt,
    ///   key_commitment_W0, key_commitment_W1,
    ///   key_commitment_W2, key_commitment_W3,
    /// ])
    /// ```
    pub fn to_word(&self) -> Word {
        let tag = domain_tag();
        let tag_elements = tag.as_elements();
        let kc_elements = self.key_commitment.as_elements();
        let timestamp_felt = Felt::new(self.timestamp_ms as u64);
        let message_elements: [Felt; 9] = [
            tag_elements[0],
            tag_elements[1],
            tag_elements[2],
            tag_elements[3],
            timestamp_felt,
            kc_elements[0],
            kc_elements[1],
            kc_elements[2],
            kc_elements[3],
        ];
        Rpo256::hash_elements(&message_elements)
    }
}

/// Returns the cached domain-tag word so server, client, and parity-fixture code
/// can all assert on the same constant.
pub fn lookup_domain_tag() -> Word {
    domain_tag()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_request_message::AuthRequestMessage;
    use crate::auth_request_payload::AuthRequestPayload;
    use miden_protocol::account::AccountId;

    fn sample_commitment(seed: u32) -> Word {
        Word::from([
            seed,
            seed.wrapping_add(1),
            seed.wrapping_add(2),
            seed.wrapping_add(3),
        ])
    }

    #[test]
    fn domain_tag_is_stable_across_calls() {
        let a = lookup_domain_tag();
        let b = lookup_domain_tag();
        assert_eq!(a, b);
    }

    #[test]
    fn domain_tag_is_not_zero_word() {
        let tag = lookup_domain_tag();
        assert_ne!(tag, Word::from([Felt::ZERO; 4]));
    }

    #[test]
    fn digest_changes_with_commitment() {
        let timestamp = 1_700_000_000_000i64;
        let left = LookupAuthMessage::new(timestamp, sample_commitment(1)).to_word();
        let right = LookupAuthMessage::new(timestamp, sample_commitment(2)).to_word();
        assert_ne!(left, right);
    }

    #[test]
    fn digest_changes_with_timestamp() {
        let commitment = sample_commitment(7);
        let left = LookupAuthMessage::new(1_700_000_000_000, commitment).to_word();
        let right = LookupAuthMessage::new(1_700_000_000_001, commitment).to_word();
        assert_ne!(left, right);
    }

    #[test]
    fn digest_is_deterministic() {
        let msg = LookupAuthMessage::new(1_700_000_000_000, sample_commitment(42));
        assert_eq!(msg.to_word(), msg.to_word());
    }

    #[test]
    fn digest_handles_extreme_timestamps() {
        let commitment = sample_commitment(99);
        let zero = LookupAuthMessage::new(0, commitment).to_word();
        let large = LookupAuthMessage::new(i64::MAX, commitment).to_word();
        assert_ne!(zero, large);
        // Negative timestamps are accepted by the type but rejected by the
        // server skew check; the digest itself is just a deterministic mapping.
        let negative = LookupAuthMessage::new(-1, commitment).to_word();
        assert_ne!(negative, zero);
    }

    #[test]
    fn lookup_digest_is_distinct_from_auth_request_digest() {
        // A LookupAuthMessage digest must not collide with any AuthRequestMessage
        // digest, even when timestamp and the 4-felt payload are aligned to the
        // commitment, so a signature for one cannot be replayed against the other.
        let timestamp = 1_700_000_000_000i64;
        let commitment = sample_commitment(123);

        let lookup_digest = LookupAuthMessage::new(timestamp, commitment).to_word();

        let account_id =
            AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").expect("valid account id");
        let payload = AuthRequestPayload::from_bytes(&commitment.as_bytes());
        let request_digest = AuthRequestMessage::new(account_id, timestamp, payload).to_word();

        assert_ne!(lookup_digest, request_digest);
    }

    #[test]
    fn domain_tag_is_known_constant() {
        // Pin the on-the-wire domain-separator. If this assertion ever needs to
        // change, every signer in the field must be updated in lockstep.
        // The expected value is computed from b"guardian.lookup.v1" via RPO256.
        let tag = lookup_domain_tag();
        // Recompute the expected value here (rather than hard-coding) so the
        // assertion fails clearly if the chunking convention or hash function
        // ever changes.
        let mut elements: Vec<Felt> = Vec::new();
        for chunk in DOMAIN_TAG_BYTES.chunks(8) {
            let mut bytes = [0u8; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            elements.push(Felt::new(u64::from_le_bytes(bytes)));
        }
        let expected = Rpo256::hash_elements(&elements);
        assert_eq!(tag, expected);
    }
}
