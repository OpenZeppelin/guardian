//! Per-instance circuit breakers and the `Describe` health probe. Breaker state is per replica and
//! in memory; nothing about it is stored.

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CircuitState {
    #[default]
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

#[derive(Debug, Clone, Copy, Default)]
struct Breaker {
    state: CircuitState,
    consecutive_failures: u32,
    /// A half-open breaker's one trial call is out and has not reported back.
    trial_in_flight: bool,
    /// Moves each time the breaker opens and each time a half-open breaker lets a trial
    /// through, so only the call admitted as the current trial can close it.
    generation: u64,
    /// The last health probe failed and the module has not answered since. Only failures
    /// counted from that probe on open a closed breaker: a call's failure can depend on its
    /// request, while the probe carries none.
    probe_failing: bool,
}

/// Who is asking to call a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// A request that leads to a signature or a stored proposal.
    Request,
    /// A policy check: it neither tests a recovering module nor changes a breaker.
    DryRun,
}

/// How a call was let through, which its result reports back with: a half-open breaker closes
/// only on the answer of the trial it admitted last, never on a call admitted before it opened
/// that happens to finish late.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    generation: u64,
    trial: bool,
}

/// One breaker per instance id. A closed breaker opens after `threshold` consecutive failures,
/// once a health probe has also failed since the module last answered, so failures that depend
/// on the request do not open it while the module answers its probes; an open one skips the
/// module until a successful probe moves it to half-open, where one real call at a time is let
/// through: it closes the breaker, counting a failure when the trial fails, and a failed probe
/// opens it again.
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

    /// Whether the module may be called now, and how; when not, the instance's `on_error`
    /// applies at once instead. A half-open breaker admits one request at a time as its trial.
    /// A request admitted reports back with [`Self::answered`] or [`Self::failed`].
    pub fn admit(&self, instance: &str, caller: Caller) -> Option<Admission> {
        let mut breakers = self.by_instance.lock().ok()?;
        let Some(breaker) = breakers.get_mut(instance) else {
            return Some(Admission {
                generation: 0,
                trial: false,
            });
        };
        match (breaker.state, caller) {
            (CircuitState::Closed, _) => Some(Admission {
                generation: breaker.generation,
                trial: false,
            }),
            (CircuitState::Open, _) | (CircuitState::HalfOpen, Caller::DryRun) => None,
            (CircuitState::HalfOpen, Caller::Request) if breaker.trial_in_flight => None,
            (CircuitState::HalfOpen, Caller::Request) => {
                breaker.trial_in_flight = true;
                breaker.generation += 1;
                Some(Admission {
                    generation: breaker.generation,
                    trial: true,
                })
            }
        }
    }

    /// The module answered, allow or deny: a deny is an answer, not a failure. A closed
    /// breaker's failure count resets when the call was admitted since the breaker last
    /// closed, and a half-open one closes when this is its current trial's answer. Any other
    /// answer is to a call admitted before the breaker opened, or before a later trial, and
    /// says nothing a probe and that trial have not decided since.
    pub fn answered(&self, instance: &str, admission: Admission) {
        self.update(instance, |breaker| match breaker.state {
            CircuitState::Closed if admission.generation == breaker.generation => {
                breaker.consecutive_failures = 0;
                breaker.probe_failing = false;
            }
            CircuitState::Closed => {}
            CircuitState::HalfOpen if is_current_trial(breaker, admission) => {
                breaker.state = CircuitState::Closed;
                breaker.consecutive_failures = 0;
                breaker.probe_failing = false;
                breaker.trial_in_flight = false;
            }
            CircuitState::HalfOpen | CircuitState::Open => {}
        });
    }

    /// The module timed out, failed to respond or returned a malformed decision. A closed
    /// breaker counts it only when the call was admitted since the breaker last closed, so late
    /// failures of calls from before an outage cannot reopen it. A half-open breaker's current
    /// trial failing closes it with the failure counted: the probe that half-opened it says the
    /// module is up, and one call's failure can depend on its request. A probe failing while
    /// half-open opens it again.
    pub fn failed(&self, instance: &str, admission: Admission) {
        let threshold = self.threshold;
        self.update(instance, |breaker| match breaker.state {
            CircuitState::Closed if admission.generation == breaker.generation => {
                count_failure(breaker, threshold)
            }
            CircuitState::Closed => {}
            CircuitState::HalfOpen if is_current_trial(breaker, admission) => {
                breaker.trial_in_flight = false;
                breaker.state = CircuitState::Closed;
                breaker.consecutive_failures = 1;
            }
            CircuitState::HalfOpen | CircuitState::Open => {}
        });
    }

    /// Admits a call like [`Self::admit`] and holds its admission until the call reports
    /// back. A call that never does (cut short by the budget, a dry run, or a request dropped
    /// mid-call) releases its half-open trial when it is dropped, so the next request can be
    /// the trial instead of waiting for a probe.
    pub fn call<'a>(&'a self, instance: &'a str, caller: Caller) -> Option<Call<'a>> {
        self.admit(instance, caller).map(|admission| Call {
            breakers: self,
            instance,
            admission,
            settled: false,
        })
    }

    /// Frees the half-open trial slot `admission` holds, if it is the current trial, without
    /// deciding anything: the breaker stays half-open for the next request.
    pub fn release(&self, instance: &str, admission: Admission) {
        self.update(instance, |breaker| {
            if breaker.state == CircuitState::HalfOpen && is_current_trial(breaker, admission) {
                breaker.trial_in_flight = false;
            }
        });
    }

    /// A health probe could not reach the module. On a closed breaker the first such probe
    /// starts the count again from itself, so only failures after it count with it toward
    /// opening; a half-open breaker opens again.
    pub fn probe_failed(&self, instance: &str) {
        let threshold = self.threshold;
        self.update(instance, |breaker| match breaker.state {
            CircuitState::Closed => {
                if !breaker.probe_failing {
                    breaker.consecutive_failures = 0;
                    breaker.probe_failing = true;
                }
                count_failure(breaker, threshold)
            }
            CircuitState::HalfOpen | CircuitState::Open => {
                breaker.state = CircuitState::Open;
                breaker.trial_in_flight = false;
            }
        });
    }

    /// Opens the breaker at once, whatever the module answered before, and keeps it open until
    /// a probe passes: for a module that did not answer its startup checks, and for one a
    /// health probe finds answering with a protocol version, a required capability, a hook set
    /// or params this Guardian cannot work with. Its decisions are not trusted meanwhile.
    pub fn hold_open(&self, instance: &str) {
        self.update(instance, |breaker| {
            breaker.state = CircuitState::Open;
            breaker.trial_in_flight = false;
        });
    }

    /// A health probe succeeded: an open breaker lets the next real call through, a half-open
    /// one whose trial is still out lets another, and a closed one forgets the failures counted
    /// so far, which the module answering its probe does not confirm.
    pub fn probe_succeeded(&self, instance: &str) {
        self.update(instance, |breaker| {
            breaker.probe_failing = false;
            match breaker.state {
                CircuitState::Open => breaker.state = CircuitState::HalfOpen,
                CircuitState::HalfOpen => breaker.trial_in_flight = false,
                CircuitState::Closed => breaker.consecutive_failures = 0,
            }
        });
    }

    /// Applies `change` and reports a transition while still holding the lock, so concurrent
    /// transitions reach the gauge and the log in the order they happened. Opening moves the
    /// generation, so no call admitted before can close the breaker.
    fn update(&self, instance: &str, change: impl FnOnce(&mut Breaker)) {
        let Ok(mut breakers) = self.by_instance.lock() else {
            return;
        };
        let breaker = breakers.entry(instance.to_string()).or_default();
        let previous = breaker.state;
        change(breaker);
        let current = breaker.state;
        if previous != current {
            if current == CircuitState::Open {
                breaker.generation += 1;
            }
            super::metrics::circuit_open(instance, current == CircuitState::Open);
            tracing::warn!(
                module = instance,
                from = previous.as_str(),
                to = current.as_str(),
                "Policy module circuit changed"
            );
        }
    }
}

/// One admitted call to an instance, from [`Breakers::call`]. It reports with
/// [`Call::answered`] or [`Call::failed`]; dropped without either, it releases its admission.
#[must_use = "a call reports its outcome; dropping it releases a half-open trial undecided"]
pub struct Call<'a> {
    breakers: &'a Breakers,
    instance: &'a str,
    admission: Admission,
    settled: bool,
}

impl Call<'_> {
    pub fn answered(mut self) {
        self.settled = true;
        self.breakers.answered(self.instance, self.admission);
    }

    pub fn failed(mut self) {
        self.settled = true;
        self.breakers.failed(self.instance, self.admission);
    }
}

impl Drop for Call<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.breakers.release(self.instance, self.admission);
        }
    }
}

fn is_current_trial(breaker: &Breaker, admission: Admission) -> bool {
    admission.trial && breaker.trial_in_flight && admission.generation == breaker.generation
}

fn count_failure(breaker: &mut Breaker, threshold: u32) {
    breaker.consecutive_failures = breaker.consecutive_failures.saturating_add(1);
    if breaker.consecutive_failures >= threshold && breaker.probe_failing {
        breaker.state = CircuitState::Open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(breakers: &Breakers) -> Admission {
        call_on(breakers, "m")
    }

    fn call_on(breakers: &Breakers, instance: &str) -> Admission {
        breakers
            .admit(instance, Caller::Request)
            .expect("the breaker admits the call")
    }

    #[test]
    fn consecutive_failures_open_the_breaker_once_a_probe_fails_and_an_answer_resets_the_count() {
        let breakers = Breakers::new(3);
        for _ in 0..3 {
            breakers.failed("m", call(&breakers));
        }
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "request failures alone do not open it"
        );
        breakers.probe_failed("m");
        breakers.failed("m", call(&breakers));
        breakers.answered("m", call(&breakers));
        breakers.failed("m", call(&breakers));
        breakers.failed("m", call(&breakers));
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "an answer shows the module is up and resets the count"
        );
        breakers.probe_failed("m");
        breakers.failed("m", call(&breakers));
        breakers.failed("m", call(&breakers));
        assert_eq!(
            breakers.state("m"),
            CircuitState::Open,
            "a failed probe and the failures after it open it"
        );
        assert!(breakers.admit("m", Caller::Request).is_none());
    }

    #[test]
    fn request_failures_open_the_breaker_while_the_last_probe_failed() {
        let breakers = Breakers::new(3);
        breakers.probe_failed("m");
        breakers.failed("m", call(&breakers));
        assert_eq!(breakers.state("m"), CircuitState::Closed);
        breakers.failed("m", call(&breakers));
        assert_eq!(breakers.state("m"), CircuitState::Open);
    }

    #[test]
    fn a_passing_probe_or_an_answer_stops_request_failures_from_opening_the_breaker() {
        let breakers = Breakers::new(2);
        breakers.probe_failed("m");
        breakers.probe_succeeded("m");
        breakers.failed("m", call(&breakers));
        breakers.failed("m", call(&breakers));
        assert_eq!(breakers.state("m"), CircuitState::Closed);
        let other = Breakers::new(2);
        other.probe_failed("n");
        other.answered("n", call_on(&other, "n"));
        other.failed("n", call_on(&other, "n"));
        other.failed("n", call_on(&other, "n"));
        assert_eq!(
            other.state("n"),
            CircuitState::Closed,
            "an answer shows the module is up until the next probe says otherwise"
        );
    }

    #[test]
    fn a_late_answer_does_not_close_an_open_breaker() {
        let breakers = Breakers::new(5);
        let early = call(&breakers);
        breakers.hold_open("m");
        breakers.answered("m", early);
        assert_eq!(
            breakers.state("m"),
            CircuitState::Open,
            "an answer to a call admitted before the breaker opened"
        );
        breakers.probe_succeeded("m");
        breakers.answered("m", call(&breakers));
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "the trial answered"
        );
    }

    /// A call admitted while the breaker was closed can finish after a probe half-opened it:
    /// its answer is not the trial's, so it neither closes nor reopens the breaker.
    #[test]
    fn a_late_result_does_not_decide_a_half_open_breaker() {
        let breakers = Breakers::new(1);
        let early = call(&breakers);
        let also_early = call(&breakers);
        breakers.probe_failed("m");
        breakers.probe_succeeded("m");
        assert_eq!(breakers.state("m"), CircuitState::HalfOpen);
        breakers.answered("m", early);
        assert_eq!(breakers.state("m"), CircuitState::HalfOpen);
        let trial = call(&breakers);
        breakers.answered("m", also_early);
        breakers.failed("m", also_early);
        assert_eq!(
            breakers.state("m"),
            CircuitState::HalfOpen,
            "only the trial decides"
        );
        breakers.answered("m", trial);
        assert_eq!(breakers.state("m"), CircuitState::Closed);
    }

    #[test]
    fn a_trial_that_never_reports_back_frees_the_slot_for_the_next_request() {
        let breakers = Breakers::new(1);
        breakers.probe_failed("m");
        breakers.probe_succeeded("m");
        let trial = breakers
            .call("m", Caller::Request)
            .expect("the trial is admitted");
        assert!(breakers.admit("m", Caller::Request).is_none());
        drop(trial);
        assert_eq!(breakers.state("m"), CircuitState::HalfOpen);
        let next = breakers
            .call("m", Caller::Request)
            .expect("a dropped trial does not hold the slot until a probe");
        next.answered();
        assert_eq!(breakers.state("m"), CircuitState::Closed);
    }

    #[test]
    fn a_late_failure_from_before_an_outage_does_not_count_once_closed_again() {
        let breakers = Breakers::new(2);
        let early = call(&breakers);
        breakers.probe_failed("m");
        breakers.probe_failed("m");
        assert_eq!(breakers.state("m"), CircuitState::Open);
        breakers.probe_succeeded("m");
        breakers
            .call("m", Caller::Request)
            .expect("trial")
            .answered();
        assert_eq!(breakers.state("m"), CircuitState::Closed);
        breakers.probe_failed("m");
        breakers.failed("m", early);
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "a failure admitted before the breaker opened says nothing about the module now"
        );
        breakers
            .call("m", Caller::Request)
            .expect("closed")
            .failed();
        assert_eq!(breakers.state("m"), CircuitState::Open);
    }

    #[test]
    fn a_half_open_breaker_admits_one_trial_at_a_time_and_no_dry_run() {
        let breakers = Breakers::new(1);
        breakers.probe_failed("m");
        breakers.probe_succeeded("m");
        assert!(breakers.admit("m", Caller::DryRun).is_none());
        let dropped = call(&breakers);
        assert!(
            breakers.admit("m", Caller::Request).is_none(),
            "the trial is still out"
        );
        breakers.probe_succeeded("m");
        let trial = call(&breakers);
        breakers.answered("m", dropped);
        assert_eq!(
            breakers.state("m"),
            CircuitState::HalfOpen,
            "a trial a later probe replaced no longer decides"
        );
        breakers.answered("m", trial);
        assert!(breakers.admit("m", Caller::Request).is_some());
        assert!(breakers.admit("m", Caller::DryRun).is_some());
    }

    #[test]
    fn a_probe_half_opens_and_the_next_call_decides() {
        let breakers = Breakers::new(1);
        breakers.probe_failed("m");
        breakers.probe_succeeded("m");
        assert_eq!(breakers.state("m"), CircuitState::HalfOpen);
        breakers.failed("m", call(&breakers));
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "with the module answering its probe, a failed trial closes the breaker and counts"
        );
        breakers.probe_failed("m");
        assert_eq!(breakers.state("m"), CircuitState::Open);
        breakers.probe_succeeded("m");
        breakers.answered("m", call(&breakers));
        assert_eq!(breakers.state("m"), CircuitState::Closed);
    }

    #[test]
    fn a_probe_failing_while_half_open_opens_the_breaker_again() {
        let breakers = Breakers::new(3);
        for _ in 0..3 {
            breakers.probe_failed("m");
        }
        breakers.probe_succeeded("m");
        let trial = call(&breakers);
        breakers.probe_failed("m");
        assert_eq!(breakers.state("m"), CircuitState::Open);
        breakers.failed("m", trial);
        assert_eq!(breakers.state("m"), CircuitState::Open);
    }

    #[test]
    fn only_failures_from_the_first_failed_probe_on_open_the_breaker() {
        let breakers = Breakers::new(3);
        for _ in 0..4 {
            breakers.failed("m", call(&breakers));
        }
        breakers.probe_failed("m");
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "failures counted while probes passed do not add up with the probe's"
        );
        breakers.failed("m", call(&breakers));
        breakers.failed("m", call(&breakers));
        assert_eq!(breakers.state("m"), CircuitState::Open);
    }

    #[test]
    fn a_passing_probe_forgets_the_failures_a_closed_breaker_counted() {
        let breakers = Breakers::new(3);
        breakers.failed("m", call(&breakers));
        breakers.failed("m", call(&breakers));
        breakers.probe_succeeded("m");
        breakers.probe_failed("m");
        assert_eq!(
            breakers.state("m"),
            CircuitState::Closed,
            "failures from before the passing probe do not add up with a later probe failure"
        );
    }

    #[test]
    fn an_incompatible_probe_opens_a_closed_breaker_until_a_probe_passes() {
        let breakers = Breakers::new(5);
        breakers.hold_open("m");
        assert_eq!(breakers.state("m"), CircuitState::Open);
        assert!(breakers.admit("m", Caller::Request).is_none());
        breakers.probe_succeeded("m");
        assert!(breakers.admit("m", Caller::Request).is_some());
    }

    #[test]
    fn a_passing_probe_keeps_a_closed_breaker_closed_and_instances_are_independent() {
        let breakers = Breakers::new(1);
        breakers.probe_succeeded("a");
        assert_eq!(breakers.state("a"), CircuitState::Closed);
        breakers.probe_failed("a");
        assert!(breakers.admit("a", Caller::Request).is_none());
        assert!(breakers.admit("b", Caller::Request).is_some());
    }
}
