use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A wall-clock budget, parsed from a compact form such as `90s` or `4m30s`
/// and serialized as an ISO 8601 duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Budget {
    seconds: u64,
}

impl Budget {
    pub const fn from_seconds(seconds: u64) -> Self {
        Self { seconds }
    }

    pub const fn as_seconds(self) -> u64 {
        self.seconds
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BudgetParseError {
    #[error("duration is empty")]
    Empty,
    #[error("duration `{0}` has no recognized unit; expected h, m or s")]
    MissingUnit(String),
    #[error("duration `{0}` has a value that is not a whole number")]
    NotANumber(String),
    #[error("duration `{0}` overflows")]
    Overflow(String),
}

impl FromStr for Budget {
    type Err = BudgetParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(BudgetParseError::Empty);
        }

        // Both spellings, because the two are written by different hands: the
        // manifest is authored compactly (`90s`, `4m30s`) and the run result is
        // ISO 8601 by contract (`PT90S`), which is also what this type
        // serializes. Accepting only the compact form left the reader unable to
        // parse what the writer had just produced.
        let text = match text.strip_prefix("PT").or_else(|| text.strip_prefix("pt")) {
            Some(rest) if !rest.is_empty() => &rest.to_ascii_lowercase(),
            Some(_) => return Err(BudgetParseError::MissingUnit(raw.to_string())),
            None => text,
        };

        let mut total: u64 = 0;
        let mut digits = String::new();
        let mut saw_unit = false;

        for ch in text.chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                continue;
            }
            let multiplier = match ch {
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => return Err(BudgetParseError::MissingUnit(raw.to_string())),
            };
            if digits.is_empty() {
                return Err(BudgetParseError::NotANumber(raw.to_string()));
            }
            let value: u64 = digits
                .parse()
                .map_err(|_| BudgetParseError::Overflow(raw.to_string()))?;
            // Checked on both steps. The multiplication was not, so a value
            // that parses as a `u64` but overflows when scaled, `1h` short of
            // `u64::MAX` for one, panicked in a debug build and wrapped to a
            // wrong budget in a release one. `Budget` is deserialized from run
            // result files as well as from the committed manifest, so the input
            // is not only text this repository wrote.
            total = value
                .checked_mul(multiplier)
                .and_then(|scaled| total.checked_add(scaled))
                .ok_or_else(|| BudgetParseError::Overflow(raw.to_string()))?;
            digits.clear();
            saw_unit = true;
        }

        if !digits.is_empty() || !saw_unit {
            return Err(BudgetParseError::MissingUnit(raw.to_string()));
        }
        Ok(Self { seconds: total })
    }
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hours = self.seconds / 3600;
        let minutes = (self.seconds % 3600) / 60;
        let seconds = self.seconds % 60;
        write!(f, "PT")?;
        if hours > 0 {
            write!(f, "{hours}H")?;
        }
        if minutes > 0 {
            write!(f, "{minutes}M")?;
        }
        if seconds > 0 || (hours == 0 && minutes == 0) {
            write!(f, "{seconds}S")?;
        }
        Ok(())
    }
}

impl Serialize for Budget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Budget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {

    /// Parses as a `u64` and overflows only when scaled to seconds, which is
    /// the case the checked addition alone could not catch.
    #[test]
    fn a_value_that_overflows_when_scaled_is_refused() {
        for raw in ["9223372036854775807h", "18446744073709551615m"] {
            assert!(
                matches!(Budget::from_str(raw), Err(BudgetParseError::Overflow(_))),
                "{raw} should be refused as an overflow"
            );
        }
    }
    use super::*;

    /// What this type serializes must be what it parses, or the reader cannot
    /// read what the writer produced.
    #[test]
    fn a_budget_round_trips_through_its_own_rendering() {
        for seconds in [0_u64, 1, 48, 90, 150, 1800, 3600, 3661] {
            let budget = Budget::from_seconds(seconds);
            let rendered = budget.to_string();
            assert_eq!(
                Budget::from_str(&rendered).expect("parses its own rendering"),
                budget,
                "rendered as {rendered}"
            );
        }
    }

    #[test]
    fn both_spellings_mean_the_same_thing() {
        assert_eq!(Budget::from_str("PT2M30S"), Budget::from_str("2m30s"));
        assert_eq!(Budget::from_str("PT48S"), Budget::from_str("48s"));
    }

    #[test]
    fn a_bare_prefix_is_not_a_duration() {
        assert!(Budget::from_str("PT").is_err());
    }

    #[test]
    fn parses_compound_units() {
        assert_eq!(Budget::from_str("90s").unwrap().as_seconds(), 90);
        assert_eq!(Budget::from_str("4m30s").unwrap().as_seconds(), 270);
        assert_eq!(Budget::from_str("1h").unwrap().as_seconds(), 3600);
    }

    #[test]
    fn rejects_bare_numbers_and_unknown_units() {
        assert!(Budget::from_str("90").is_err());
        assert!(Budget::from_str("90x").is_err());
        assert_eq!(Budget::from_str(""), Err(BudgetParseError::Empty));
    }

    #[test]
    fn renders_iso_8601() {
        assert_eq!(Budget::from_seconds(0).to_string(), "PT0S");
        assert_eq!(Budget::from_seconds(90).to_string(), "PT1M30S");
        assert_eq!(Budget::from_seconds(3600).to_string(), "PT1H");
    }
}
