use serde::{Deserialize, Serialize};

use crate::manifest::Sdk;

use super::ScenarioResult;

/// A required entry is a scenario *and* the SDK expected to run it. Keying on
/// the scenario id alone lets one SDK's pass stand in for the other's failure.
pub type RequiredEntry = (String, Sdk);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationClaim {
    Full,
    Partial,
    None,
}

/// A run whose only non-passing scenarios are environment-blocked still
/// concludes successfully. Leaving a schedule red through an upgrade window is
/// how a signal stops being read.
pub fn conclusion(results: &[ScenarioResult]) -> Conclusion {
    if results.iter().any(ScenarioResult::blocks_conclusion) {
        Conclusion::Failure
    } else {
        Conclusion::Success
    }
}

pub fn qualification_claim(
    results: &[ScenarioResult],
    required: &[RequiredEntry],
    filtered: bool,
) -> QualificationClaim {
    if filtered {
        return QualificationClaim::None;
    }
    // An empty required set is not a satisfied one. `all()` is vacuously true on
    // an empty iterator, so without this a run with nothing required claims
    // `Full`, which is the strongest thing this suite can say and the easiest to
    // reach by accident: a live profile with no network resolves to no required
    // entries at all.
    if required.is_empty() {
        return QualificationClaim::None;
    }
    let every_required_passed = required.iter().all(|(id, sdk)| {
        results
            .iter()
            .filter(|result| &result.scenario_id == id && result.sdk == *sdk)
            .any(ScenarioResult::counts_as_pass)
    });
    if every_required_passed {
        QualificationClaim::Full
    } else {
        QualificationClaim::Partial
    }
}

/// Presence is not coverage: an operator scenario that only skipped leaves the
/// surface uncovered, so the caller passes whether one actually passed.
pub fn not_covered(operator_covered: bool) -> Vec<String> {
    let mut entries = vec![
        "EVM proposal surface (out of scope)".to_string(),
        "mixed-scheme accounts (capability gap: no account builder can construct one)".to_string(),
        "published package tarball consumers (downstream projects out of scope)".to_string(),
    ];
    if !operator_covered {
        entries.push("operator and dashboard surface (deferred)".to_string());
    }
    entries
}

#[cfg(test)]
mod tests {

    // `all()` is vacuously true on an empty iterator, so an empty required set
    // used to claim the strongest verdict available. A live profile with no
    // network produces exactly that set.
    #[test]
    fn an_empty_required_set_claims_nothing() {
        assert_eq!(
            qualification_claim(&[], &[], false),
            QualificationClaim::None
        );
    }

    use super::*;
    use crate::duration::Budget;
    use crate::manifest::{Runtime, Sdk};
    use crate::report::{Classification, Outcome};

    fn result(
        id: &str,
        outcome: Outcome,
        classification: Option<Classification>,
    ) -> ScenarioResult {
        ScenarioResult {
            scenario_id: id.to_string(),
            sdk: Sdk::Rust,
            runtime: Runtime::Native,
            outcome,
            reason: (outcome != Outcome::Passed).then(|| "because".to_string()),
            classification,
            embedded_retry: false,
            duration: Budget::from_seconds(1),
        }
    }

    #[test]
    fn an_uncovered_operator_surface_is_declared() {
        assert!(
            not_covered(false)
                .iter()
                .any(|entry| entry.contains("operator"))
        );
        assert!(
            !not_covered(true)
                .iter()
                .any(|entry| entry.contains("operator"))
        );
    }

    #[test]
    fn product_failure_concludes_failure() {
        let results = vec![result("a", Outcome::Failed, Some(Classification::Product))];
        assert_eq!(conclusion(&results), Conclusion::Failure);
    }

    #[test]
    fn setup_failure_concludes_failure() {
        let results = vec![result("a", Outcome::Failed, Some(Classification::Setup))];
        assert_eq!(conclusion(&results), Conclusion::Failure);
    }

    #[test]
    fn environment_blocked_does_not_conclude_failure() {
        let results = vec![
            result("a", Outcome::Passed, None),
            result("b", Outcome::EnvironmentBlocked, None),
        ];
        assert_eq!(conclusion(&results), Conclusion::Success);
    }

    #[test]
    fn a_successful_run_can_claim_less_than_full() {
        let results = vec![
            result("a", Outcome::Passed, None),
            result("b", Outcome::EnvironmentBlocked, None),
        ];
        let required = vec![("a".to_string(), Sdk::Rust), ("b".to_string(), Sdk::Rust)];
        assert_eq!(conclusion(&results), Conclusion::Success);
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Partial
        );
    }

    #[test]
    fn full_claim_needs_every_required_scenario_to_pass() {
        let results = vec![
            result("a", Outcome::Passed, None),
            result("b", Outcome::Passed, None),
        ];
        let required = vec![("a".to_string(), Sdk::Rust), ("b".to_string(), Sdk::Rust)];
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Full
        );
    }

    #[test]
    fn a_required_scenario_absent_from_the_run_prevents_a_full_claim() {
        let results = vec![result("a", Outcome::Passed, None)];
        let required = vec![("a".to_string(), Sdk::Rust), ("b".to_string(), Sdk::Rust)];
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Partial
        );
    }

    #[test]
    fn a_skipped_required_scenario_prevents_a_full_claim() {
        let results = vec![
            result("a", Outcome::Passed, None),
            result("b", Outcome::Skipped, None),
        ];
        let required = vec![("a".to_string(), Sdk::Rust), ("b".to_string(), Sdk::Rust)];
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Partial
        );
    }

    fn result_for(id: &str, sdk: Sdk, outcome: Outcome) -> ScenarioResult {
        ScenarioResult {
            scenario_id: id.to_string(),
            sdk,
            runtime: Runtime::Native,
            outcome,
            reason: (outcome != Outcome::Passed).then(|| "because".to_string()),
            classification: None,
            embedded_retry: false,
            duration: Budget::from_seconds(1),
        }
    }

    #[test]
    fn one_sdk_passing_does_not_cover_the_other_failing() {
        let results = vec![
            result_for("a", Sdk::Rust, Outcome::Passed),
            result_for("a", Sdk::Typescript, Outcome::Failed),
        ];
        let required = vec![
            ("a".to_string(), Sdk::Rust),
            ("a".to_string(), Sdk::Typescript),
        ];
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Partial
        );
    }

    #[test]
    fn one_sdk_passing_does_not_cover_the_other_being_absent() {
        let results = vec![result_for("a", Sdk::Rust, Outcome::Passed)];
        let required = vec![
            ("a".to_string(), Sdk::Rust),
            ("a".to_string(), Sdk::Typescript),
        ];
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Partial
        );
    }

    #[test]
    fn both_sdks_passing_claims_full() {
        let results = vec![
            result_for("a", Sdk::Rust, Outcome::Passed),
            result_for("a", Sdk::Typescript, Outcome::Passed),
        ];
        let required = vec![
            ("a".to_string(), Sdk::Rust),
            ("a".to_string(), Sdk::Typescript),
        ];
        assert_eq!(
            qualification_claim(&results, &required, false),
            QualificationClaim::Full
        );
    }

    #[test]
    fn a_filtered_run_never_claims_qualification() {
        let results = vec![result("a", Outcome::Passed, None)];
        let required = vec![("a".to_string(), Sdk::Rust)];
        assert_eq!(
            qualification_claim(&results, &required, true),
            QualificationClaim::None
        );
    }
}
