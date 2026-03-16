pub(crate) fn hex_body_eq(left: &str, right: &str) -> bool {
    strip_hex_prefix(left).eq_ignore_ascii_case(strip_hex_prefix(right))
}

fn strip_hex_prefix(value: &str) -> &str {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::hex_body_eq;

    #[test]
    fn hex_body_eq_ignores_prefix_and_case() {
        assert!(hex_body_eq("0xAbCd", "abcd"));
        assert!(hex_body_eq("0XABCD", "0xabcd"));
        assert!(!hex_body_eq("0xabc0", "0xabcd"));
    }
}
