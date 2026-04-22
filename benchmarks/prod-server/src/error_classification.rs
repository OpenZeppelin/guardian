use anyhow::Error;

pub fn classify(error: &Error) -> String {
    let message = error.to_string().to_ascii_lowercase();

    if message.contains("permission_denied")
        || message.contains("unauthenticated")
        || message.contains("invalid signature")
        || message.contains("auth")
    {
        return "auth".to_string();
    }
    if message.contains("commitment mismatch") || message.contains("conflictpendingdelta") {
        return "state_conflict".to_string();
    }
    if message.contains("cannot push new delta")
        || message.contains("non-canonical delta pending")
        || message.contains("pending proposals")
    {
        return "state_conflict".to_string();
    }
    if message.contains("network connection")
        || message.contains("http2 error")
        || message.contains("transport")
        || message.contains("dns")
    {
        return "transport".to_string();
    }
    if message.contains("timed out") || message.contains("deadline") || message.contains("timeout")
    {
        return "timeout".to_string();
    }
    if message.contains("miden") || message.contains("rpc") || message.contains("block header") {
        return "upstream_miden".to_string();
    }
    if message.contains("not found") {
        return "not_found".to_string();
    }

    "server".to_string()
}

#[cfg(test)]
mod tests {
    use super::classify;
    use anyhow::anyhow;

    #[test]
    fn classifies_pending_delta_errors_as_state_conflicts() {
        let error =
            anyhow!("Cannot push new delta: there is already a non-canonical delta pending");
        assert_eq!(classify(&error), "state_conflict");
    }
}
