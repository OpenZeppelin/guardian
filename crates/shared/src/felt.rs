use miden_protocol::Felt;

/// Maps an arbitrary `u64` onto a canonical field element by reducing modulo
/// the field order.
///
/// Miden 0.15 made `Felt::new` reject non-canonical inputs (it returns a
/// `Result`), whereas 0.14 reduced silently. Byte-packed digest inputs are
/// arbitrary `u64` values, so they are reduced here to preserve the original
/// digest layout. The `% Felt::ORDER` guarantees a canonical value, so the
/// inner construction never fails.
pub fn felt_from_u64_reduced(value: u64) -> Felt {
    Felt::new(value % Felt::ORDER).expect("value reduced modulo the field order is canonical")
}
