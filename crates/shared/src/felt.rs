use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::{Felt, Word};

/// Maps an arbitrary `u64` onto a canonical field element by reducing modulo
/// the field order.
///
/// Miden 0.15's `Felt::new` rejects non-canonical inputs, whereas 0.14 reduced
/// silently. Byte-packed digest inputs are arbitrary `u64`s, so reducing here
/// preserves the original digest layout and keeps the construction infallible.
pub fn felt_from_u64_reduced(value: u64) -> Felt {
    Felt::new(value % Felt::ORDER).expect("value reduced modulo the field order is canonical")
}

/// RPO hash of `bytes` packed as 8-byte little-endian chunks (the last one
/// zero-padded). The convention every Guardian message domain tag uses.
pub fn domain_tag_word(bytes: &[u8]) -> Word {
    let elements: Vec<Felt> = bytes
        .chunks(8)
        .map(|chunk| {
            let mut chunk_bytes = [0u8; 8];
            chunk_bytes[..chunk.len()].copy_from_slice(chunk);
            felt_from_u64_reduced(u64::from_le_bytes(chunk_bytes))
        })
        .collect();
    Rpo256::hash_elements(&elements)
}
