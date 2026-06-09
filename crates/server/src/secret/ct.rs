use subtle::ConstantTimeEq;

/// Constant-time byte-slice equality. Kept available for byte-by-byte secret
/// comparisons against untrusted input. Not for HashMap-keyed lookups (which
/// don't expose the per-byte timing oracle this defends against).
#[allow(dead_code)]
pub(crate) fn eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}
