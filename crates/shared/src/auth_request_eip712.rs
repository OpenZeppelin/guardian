use miden_protocol::Word;
use miden_protocol::crypto::hash::keccak::Keccak256;

const DOMAIN_TYPE: &str = "EIP712Domain(string name,string version)";
const REQUEST_TYPE: &str = "GuardianRequest(bytes32 requestHash)";
const DOMAIN_NAME: &str = "Guardian Request";
const DOMAIN_VERSION: &str = "1";
const EIP191_PREFIX: u8 = 0x19;
const EIP712_VERSION: u8 = 0x01;

fn keccak(bytes: &[u8]) -> [u8; 32] {
    Keccak256::hash(bytes).into()
}

/// EIP-712 digest of the existing account, timestamp, and payload-bound request hash.
pub fn request_digest(request_hash: Word) -> [u8; 32] {
    let mut domain = [0u8; 96];
    domain[..32].copy_from_slice(&keccak(DOMAIN_TYPE.as_bytes()));
    domain[32..64].copy_from_slice(&keccak(DOMAIN_NAME.as_bytes()));
    domain[64..].copy_from_slice(&keccak(DOMAIN_VERSION.as_bytes()));
    let domain_separator = keccak(&domain);

    let mut request = [0u8; 64];
    request[..32].copy_from_slice(&keccak(REQUEST_TYPE.as_bytes()));
    request[32..].copy_from_slice(&request_hash.as_bytes());
    let struct_hash = keccak(&request);

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
