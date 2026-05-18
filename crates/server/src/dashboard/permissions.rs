//! Operator permission vocabulary for feature 006-operator-authz.
//!
//! v1 vocabulary (FR-004): `dashboard:read`, `accounts:pause`,
//! `policies:write`. The vocabulary is server-defined; unknown strings
//! are rejected at allowlist load time so a typo in a deployment's
//! config surfaces explicitly rather than silently degrading to a
//! no-permission grant.
//!
//! Consumer features (#181, #182) add their permission consts here
//! alongside the existing v1 set.

use std::fmt;

/// The set of permissions this Guardian build recognizes. Stable
/// across releases; renaming a variant is a breaking contract change.
///
/// Intentionally does NOT derive serde traits: the canonical wire
/// form is the colon string from [`Permission::as_str`]. Routing the
/// value through serde would produce the rust-snake-case form
/// (`accounts_pause`) and silently diverge from the allowlist /
/// dashboard / audit wire vocabulary.
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Permission {
    DashboardRead,
    AccountsPause,
    PoliciesWrite,
}

/// Wire string for [`Permission::DashboardRead`]. Hard-coded by
/// dashboards and audit consumers; do not rename without bumping the
/// contract.
pub const DASHBOARD_READ: &str = "dashboard:read";

/// Wire string for [`Permission::AccountsPause`].
pub const ACCOUNTS_PAUSE: &str = "accounts:pause";

/// Wire string for [`Permission::PoliciesWrite`].
pub const POLICIES_WRITE: &str = "policies:write";

/// Returned by [`Permission::from_str`] when the input is not in the
/// v1 vocabulary. The offending string is preserved so the allowlist
/// loader can surface it in the error message that reaches operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPermission(pub String);

impl fmt::Display for UnknownPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown permission: {:?}", self.0)
    }
}

impl std::error::Error for UnknownPermission {}

impl Permission {
    /// Stable wire string for this permission. Always matches the
    /// `pub const` of the same name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DashboardRead => DASHBOARD_READ,
            Self::AccountsPause => ACCOUNTS_PAUSE,
            Self::PoliciesWrite => POLICIES_WRITE,
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Permission {
    type Err = UnknownPermission;

    /// Parse a permission string. Matches case-sensitively (FR-005)
    /// and rejects any leading or trailing whitespace by requiring an
    /// exact match against the v1 vocabulary. No coercion, no trim.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            DASHBOARD_READ => Ok(Self::DashboardRead),
            ACCOUNTS_PAUSE => Ok(Self::AccountsPause),
            POLICIES_WRITE => Ok(Self::PoliciesWrite),
            other => Err(UnknownPermission(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn parses_each_known_permission() {
        assert_eq!(
            Permission::from_str(DASHBOARD_READ).unwrap(),
            Permission::DashboardRead
        );
        assert_eq!(
            Permission::from_str(ACCOUNTS_PAUSE).unwrap(),
            Permission::AccountsPause
        );
        assert_eq!(
            Permission::from_str(POLICIES_WRITE).unwrap(),
            Permission::PoliciesWrite
        );
    }

    #[test]
    fn rejects_case_mismatch() {
        let err = Permission::from_str("Accounts:Pause").unwrap_err();
        assert_eq!(err.0, "Accounts:Pause");
    }

    #[test]
    fn rejects_leading_whitespace() {
        let err = Permission::from_str(" accounts:pause").unwrap_err();
        assert_eq!(err.0, " accounts:pause");
    }

    #[test]
    fn rejects_trailing_whitespace() {
        let err = Permission::from_str("accounts:pause ").unwrap_err();
        assert_eq!(err.0, "accounts:pause ");
    }

    #[test]
    fn rejects_empty_string() {
        let err = Permission::from_str("").unwrap_err();
        assert_eq!(err.0, "");
    }

    #[test]
    fn rejects_unknown_vocabulary() {
        let err = Permission::from_str("accounts:freeze").unwrap_err();
        assert_eq!(err.0, "accounts:freeze");
    }

    #[test]
    fn as_str_round_trips_through_from_str() {
        for permission in [
            Permission::DashboardRead,
            Permission::AccountsPause,
            Permission::PoliciesWrite,
        ] {
            let parsed = Permission::from_str(permission.as_str()).unwrap();
            assert_eq!(parsed, permission);
        }
    }
}
