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

/// The server detected a guardian switch away from its own ack key and
/// released the account (issue #305). System-initiated
/// (`operator_identity` is `system`). `payload` always carries
/// `new_guardian_commitment` and `detected_by`, plus the observation:
/// `detected_by: "delta"` (the switch delta committed here) adds
/// `{ delta_nonce, new_commitment }`; `detected_by: "chain_sweep"` (the
/// release sweep read the key from published on-chain storage, issue
/// #434) adds `{ on_chain_commitment, stored_commitment }`;
/// `detected_by: "proposal_match"` (the sweep found the chain at the
/// post-state of a switch proposal pending here) adds
/// `{ proposal_id, on_chain_commitment, stored_commitment }`.
/// `target_account_id` is set.
pub const ACCOUNTS_RELEASE: &str = "accounts.release";

/// Operator requested an out-of-cycle `/dashboard/stats` refresh
/// (`POST /dashboard/stats/refresh`, issue #371). Payload: the
/// request outcome (`queued` / `in_progress` / `cooldown`).
pub const STATS_REFRESH: &str = "stats.refresh";

/// All registered kinds in v1, for tests and introspection. Append
/// new consts above and add them to this slice in the same commit.
pub const ALL_KINDS: &[&str] = &[
    AUTH_DENIED,
    PROBE_ACCESS,
    ACCOUNTS_PAUSE,
    ACCOUNTS_UNPAUSE,
    ACCOUNTS_RELEASE,
    STATS_REFRESH,
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
                STATS_REFRESH,
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
