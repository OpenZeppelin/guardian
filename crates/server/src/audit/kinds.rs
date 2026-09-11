//! Stable `action_kind` vocabulary for the `admin_actions` audit trail.
//!
//! One central registry per feature 006-operator-authz §FR-024. Consumer
//! features add their own consts here. The audit table column is TEXT
//! and the writer accepts any string, but production code MUST use one
//! of these consts so a `git log -p audit/kinds.rs` shows the complete
//! audit-vocabulary history.

/// Authorization middleware rejected a request because the
/// authenticated operator lacked one or more required permissions.
/// `payload` carries `{ route_path, http_method, required_permissions }`
/// (FR-025); `target_account_id` is NULL.
pub const AUTH_DENIED: &str = "auth.denied";

/// Authorization-middleware probe endpoint was hit successfully. Test
/// surface only — the probe is behind the `authz-test-probe` Cargo feature
/// and never reaches production builds. `payload` is `{}`.
pub const PROBE_ACCESS: &str = "probe.access";

/// Operator paused an account. `payload` carries
/// `{ before_state, after_state, reason }`; `target_account_id` is set.
pub const ACCOUNTS_PAUSE: &str = "accounts.pause";

/// Operator unpaused (or attempted to unpause an already-active)
/// account. `payload` carries `{ before_state, after_state, reason }`;
/// `target_account_id` is set.
pub const ACCOUNTS_UNPAUSE: &str = "accounts.unpause";

/// The server detected a canonicalized guardian switch away from its
/// own ack key and released the account (issue #305). System-initiated
/// (`operator_identity` is `system`). `payload` carries
/// `{ new_guardian_commitment, delta_nonce, new_commitment }`;
/// `target_account_id` is set.
pub const ACCOUNTS_RELEASE: &str = "accounts.release";

/// All registered kinds in v1, for tests and introspection. Append
/// new consts above and add them to this slice in the same commit.
pub const ALL_KINDS: &[&str] = &[
    AUTH_DENIED,
    PROBE_ACCESS,
    ACCOUNTS_PAUSE,
    ACCOUNTS_UNPAUSE,
    ACCOUNTS_RELEASE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_matches_consts() {
        assert_eq!(
            ALL_KINDS,
            &[
                AUTH_DENIED,
                PROBE_ACCESS,
                ACCOUNTS_PAUSE,
                ACCOUNTS_UNPAUSE,
                ACCOUNTS_RELEASE,
            ]
        );
    }

    #[test]
    fn kinds_are_dot_separated_lowercase() {
        // Audit consumers (psql, log grep) assume `<domain>.<verb>`.
        for kind in ALL_KINDS {
            assert!(
                kind.contains('.'),
                "action_kind {kind} should follow domain.verb"
            );
            assert_eq!(
                kind.to_ascii_lowercase(),
                *kind,
                "action_kind {kind} should be lowercase",
            );
        }
    }
}
