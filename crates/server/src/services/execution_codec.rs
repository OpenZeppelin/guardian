pub use guardian_shared::request_envelope::{
    ENVELOPE_FORMAT_VERSION, EnvelopeRejection, PROTOCOL_LINE as SERVER_PROTOCOL_LINE,
    TransactionRequestEnvelope,
};

#[cfg(test)]
mod tests {
    use super::SERVER_PROTOCOL_LINE;

    #[test]
    fn the_server_protocol_line_matches_the_pinned_protocol_crate() {
        let lockfile = include_str!("../../../../Cargo.lock");
        let version = lockfile
            .split("[[package]]")
            .find(|entry| entry.contains("\nname = \"miden-protocol\"\n"))
            .and_then(|entry| {
                entry
                    .lines()
                    .find_map(|line| line.strip_prefix("version = \""))
                    .map(|version| version.trim_end_matches('"').to_string())
            })
            .expect("miden-protocol is in Cargo.lock");
        assert!(
            version.starts_with(&format!("{SERVER_PROTOCOL_LINE}.")),
            "server protocol line {SERVER_PROTOCOL_LINE} does not match miden-protocol {version}"
        );
    }
}
