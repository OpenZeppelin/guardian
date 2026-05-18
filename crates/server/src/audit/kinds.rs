//! Stable `action_kind` vocabulary for the `admin_actions` audit trail.
//!
//! One central registry per feature 006-operator-authz §FR-024. Consumer
//! features add their own consts here (e.g. #181 will register
//! `accounts.pause` / `accounts.unpause`). The audit table column is
//! TEXT and the writer accepts any string, but production code MUST use
//! one of these consts so a `git log -p audit/kinds.rs` shows the
//! complete audit-vocabulary history.

/// Authorization middleware rejected a request because the
/// authenticated operator lacked one or more required permissions.
/// `payload` carries `{ route_path, http_method, required_permissions }`
/// (FR-025); `target_account_id` is NULL.
pub const AUTH_DENIED: &str = "auth.denied";

/// Authorization-middleware probe endpoint was hit successfully. Test
/// surface only — the probe is behind the `authz-test-probe` Cargo feature
/// and never reaches production builds. `payload` is `{}`.
pub const PROBE_ACCESS: &str = "probe.access";

/// All registered kinds in v1, for tests and introspection. Append
/// new consts above and add them to this slice in the same commit.
pub const ALL_KINDS: &[&str] = &[AUTH_DENIED, PROBE_ACCESS];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_matches_consts() {
        assert_eq!(ALL_KINDS, &[AUTH_DENIED, PROBE_ACCESS]);
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
