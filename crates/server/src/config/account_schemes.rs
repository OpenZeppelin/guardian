use std::fmt;

use guardian_shared::SignatureScheme;

use crate::error::{GuardianError, Result};

pub const ENV_ALLOWED_ACCOUNT_SCHEMES: &str = "GUARDIAN_ALLOWED_ACCOUNT_SCHEMES";

/// Signature schemes this Guardian accepts for **new** account registrations.
///
/// The scheme is fixed per account at creation and baked into its on-chain
/// auth code, and every later request is verified with the scheme stored in
/// that account's metadata. Restricting this set therefore only affects
/// `configure_account` for accounts that do not exist yet; accounts already
/// registered keep working whatever their scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowedAccountSchemes {
    falcon: bool,
    ecdsa: bool,
}

impl AllowedAccountSchemes {
    pub const ALL: Self = Self {
        falcon: true,
        ecdsa: true,
    };

    /// Resolves the set from `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES`, a
    /// comma-separated list of scheme names. Unset or blank means every
    /// scheme (so a Compose `${VAR:-}` passthrough stays permissive); a
    /// value that names an unrecognized scheme, or only separators, is a
    /// startup error rather than a silently permissive policy.
    pub fn from_env() -> Result<Self> {
        match std::env::var(ENV_ALLOWED_ACCOUNT_SCHEMES) {
            Ok(value) if value.trim().is_empty() => Ok(Self::ALL),
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::ALL),
            Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(
                format!("{ENV_ALLOWED_ACCOUNT_SCHEMES} must contain valid UTF-8"),
            )),
        }
    }

    pub fn parse(csv: &str) -> Result<Self> {
        let mut allowed = Self {
            falcon: false,
            ecdsa: false,
        };
        for name in csv
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let scheme = SignatureScheme::from(name).map_err(|_| {
                GuardianError::ConfigurationError(format!(
                    "{ENV_ALLOWED_ACCOUNT_SCHEMES} contains unsupported scheme {name:?} (expected a comma-separated list of `falcon` and/or `ecdsa`)"
                ))
            })?;
            allowed.enable(scheme);
        }
        if allowed
            == (Self {
                falcon: false,
                ecdsa: false,
            })
        {
            return Err(GuardianError::ConfigurationError(format!(
                "{ENV_ALLOWED_ACCOUNT_SCHEMES} must name at least one scheme (`falcon`, `ecdsa`) or be unset"
            )));
        }
        Ok(allowed)
    }

    fn enable(&mut self, scheme: SignatureScheme) {
        match scheme {
            SignatureScheme::Falcon => self.falcon = true,
            SignatureScheme::Ecdsa => self.ecdsa = true,
        }
    }

    pub fn allows(&self, scheme: SignatureScheme) -> bool {
        match scheme {
            SignatureScheme::Falcon => self.falcon,
            SignatureScheme::Ecdsa => self.ecdsa,
        }
    }

    /// Scheme names in the set, in wire order (`falcon` before `ecdsa`).
    pub fn names(&self) -> Vec<String> {
        [SignatureScheme::Falcon, SignatureScheme::Ecdsa]
            .into_iter()
            .filter(|scheme| self.allows(*scheme))
            .map(|scheme| scheme.as_str().to_string())
            .collect()
    }
}

impl Default for AllowedAccountSchemes {
    fn default() -> Self {
        Self::ALL
    }
}

impl fmt::Display for AllowedAccountSchemes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.names().join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allows_every_scheme() {
        let all = AllowedAccountSchemes::default();
        assert!(all.allows(SignatureScheme::Falcon));
        assert!(all.allows(SignatureScheme::Ecdsa));
        assert_eq!(all.to_string(), "falcon,ecdsa");
    }

    #[test]
    fn parses_a_single_scheme_case_insensitively_with_whitespace() {
        let only_ecdsa = AllowedAccountSchemes::parse(" ECDSA , ").unwrap();
        assert!(!only_ecdsa.allows(SignatureScheme::Falcon));
        assert!(only_ecdsa.allows(SignatureScheme::Ecdsa));
        assert_eq!(only_ecdsa.names(), vec!["ecdsa".to_string()]);
    }

    #[test]
    fn parses_both_schemes_in_any_order() {
        let both = AllowedAccountSchemes::parse("ecdsa,falcon").unwrap();
        assert_eq!(both, AllowedAccountSchemes::ALL);
    }

    #[test]
    fn rejects_unknown_scheme_names() {
        let err = AllowedAccountSchemes::parse("ecdsa,rsa").unwrap_err();
        assert!(err.to_string().contains("unsupported scheme \"rsa\""));
    }

    #[test]
    fn rejects_an_empty_set() {
        for value in ["", " ", ","] {
            let err = AllowedAccountSchemes::parse(value).unwrap_err();
            assert!(err.to_string().contains("at least one scheme"), "{value:?}");
        }
    }

    #[test]
    fn from_env_treats_unset_and_blank_as_every_scheme() {
        let _guard = crate::testing::env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; the variable is removed before returning.
        unsafe { std::env::remove_var(ENV_ALLOWED_ACCOUNT_SCHEMES) };
        assert_eq!(
            AllowedAccountSchemes::from_env().unwrap(),
            AllowedAccountSchemes::ALL
        );
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var(ENV_ALLOWED_ACCOUNT_SCHEMES, "  ") };
        assert_eq!(
            AllowedAccountSchemes::from_env().unwrap(),
            AllowedAccountSchemes::ALL
        );
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var(ENV_ALLOWED_ACCOUNT_SCHEMES, "ecdsa") };
        let only_ecdsa = AllowedAccountSchemes::from_env().unwrap();
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::remove_var(ENV_ALLOWED_ACCOUNT_SCHEMES) };
        assert!(!only_ecdsa.allows(SignatureScheme::Falcon));
        assert!(only_ecdsa.allows(SignatureScheme::Ecdsa));
    }
}
