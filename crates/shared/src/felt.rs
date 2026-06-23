use miden_protocol::Felt;

/// Maps an arbitrary `u64` onto a canonical field element by reducing modulo
/// the field order.
///
/// Miden 0.15's `Felt::new` rejects non-canonical inputs, whereas 0.14 reduced
/// silently. Byte-packed digest inputs are arbitrary `u64`s, so reducing here
/// preserves the original digest layout and keeps the construction infallible.
pub fn felt_from_u64_reduced(value: u64) -> Felt {
    Felt::new(value % Felt::ORDER).expect("value reduced modulo the field order is canonical")
}
