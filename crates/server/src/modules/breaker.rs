//! Per-instance circuit breakers and the `Describe` health probe. Breaker state is per replica and
//! in memory; nothing about it is stored.

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(self) -> &'static str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Breaker {
    state: CircuitState,
    consecutive_failures: u32,
}

impl Default for Breaker {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
        }
    }
}

/// One breaker per instance id. A closed breaker opens after `threshold` consecutive failures; an
/// open one skips the module until a successful probe moves it to half-open, where the next real
/// call closes it on an answer or opens it again on a failure.
pub struct Breakers {
    threshold: u32,
    by_instance: Mutex<HashMap<String, Breaker>>,
}

impl Breakers {
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold: threshold.max(1),
            by_instance: Mutex::default(),
        }
    }

    pub fn state(&self, instance: &str) -> CircuitState {
        self.by_instance
            .lock()
            .ok()
            .and_then(|breakers| breakers.get(instance).map(|breaker| breaker.state))
            .unwrap_or(CircuitState::Closed)
    }

    /// Whether the module may be called; an open breaker applies the instance's `on_error` at
    /// once instead.
    pub fn admits(&self, instance: &str) -> bool {
        self.state(instance) != CircuitState::Open
    }

    /// The module answered, allow or deny: a deny is an answer, not a failure.
    pub fn answered(&self, instance: &str) {
        self.update(instance, |breaker| *breaker = Breaker::default());
    }

    /// The module timed out, failed to respond or returned a malformed decision.
    pub fn failed(&self, instance: &str) {
        let threshold = self.threshold;
        self.update(instance, |breaker| match breaker.state {
            CircuitState::Closed => {
                breaker.consecutive_failures = breaker.consecutive_failures.saturating_add(1);
                if breaker.consecutive_failures >= threshold {
                    breaker.state = CircuitState::Open;
                }
            }
            CircuitState::HalfOpen | CircuitState::Open => breaker.state = CircuitState::Open,
        });
    }

    /// A health probe succeeded: an open breaker lets the next real call through.
    pub fn probe_succeeded(&self, instance: &str) {
        self.update(instance, |breaker| {
            if breaker.state == CircuitState::Open {
                breaker.state = CircuitState::HalfOpen;
            }
        });
    }

    fn update(&self, instance: &str, change: impl FnOnce(&mut Breaker)) {
        let previous = self.state(instance);
        let current = match self.by_instance.lock() {
            Ok(mut breakers) => {
                let breaker = breakers.entry(instance.to_string()).or_default();
                change(breaker);
                breaker.state
            }
            Err(_) => return,
        };
        if previous != current {
            tracing::warn!(
                module = instance,
                from = previous.as_str(),
                to = current.as_str(),
                "Policy module circuit changed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_failures_open_the_breaker_and_an_answer_resets_the_count() {
        let breakers = Breakers::new(3);
        breakers.failed("m");
        breakers.failed("m");
        breakers.answered("m");
        breakers.failed("m");
        breakers.failed("m");
        assert!(breakers.admits("m"), "an answer reset the count");
        breakers.failed("m");
        assert_eq!(breakers.state("m"), CircuitState::Open);
        assert!(!breakers.admits("m"));
    }

    #[test]
    fn a_probe_half_opens_and_the_next_call_decides() {
        let breakers = Breakers::new(1);
        breakers.failed("m");
        breakers.probe_succeeded("m");
        assert_eq!(breakers.state("m"), CircuitState::HalfOpen);
        assert!(breakers.admits("m"));
        breakers.failed("m");
        assert_eq!(breakers.state("m"), CircuitState::Open);
        breakers.probe_succeeded("m");
        breakers.answered("m");
        assert_eq!(breakers.state("m"), CircuitState::Closed);
    }

    #[test]
    fn a_probe_does_not_touch_a_closed_breaker_and_instances_are_independent() {
        let breakers = Breakers::new(1);
        breakers.probe_succeeded("a");
        assert_eq!(breakers.state("a"), CircuitState::Closed);
        breakers.failed("a");
        assert!(!breakers.admits("a"));
        assert!(breakers.admits("b"));
    }
}
