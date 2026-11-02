//! Which transport block watch reads over, and how long it waits before
//! reopening one: a pure state machine, so every transition is testable
//! without a node.
//!
//! In `auto` mode the stream is tried first. A node that does not serve
//! it, or refuses this caller (`UNIMPLEMENTED`, `PERMISSION_DENIED`,
//! `UNAUTHENTICATED`), sends block watch to polling until the stream retry
//! cooldown passes. A stream closed for lag, or refused for lack of a free
//! subscription (`RESOURCE_EXHAUSTED`: the node bans the caller for 10
//! minutes after a lag disconnect), sends it to polling for the ban
//! cooldown. Anything else reopens the stream after an exponential backoff
//! (1 s doubling to 60 s), and five such failures in a row without a block
//! in between send it to polling for the stream retry cooldown. A failed
//! poll is retried with the same backoff. `stream` and `poll` modes never
//! switch.

use std::time::Duration;

use tokio::time::Instant;

use super::source::{BlockSourceError, Transport};
use crate::block_watch::{BlockWatchConfig, TransportMode};

/// Backoff of the first retry; it doubles per consecutive failure.
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Longest backoff between two attempts.
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// Consecutive stream failures (no block in between) after which `auto`
/// mode polls for the stream retry cooldown.
pub const STREAM_FAILURES_BEFORE_POLLING: u32 = 5;

/// How a failure bears on the transport choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The node does not serve the stream to this caller.
    Refused,
    /// The node cut the stream for lag or has no subscription slot; it
    /// refuses the caller for a while.
    Banned,
    /// Anything else: a broken connection, a restart, a stall.
    Other,
}

impl FailureKind {
    pub fn of(error: &BlockSourceError) -> Self {
        match error.code() {
            Some(
                tonic::Code::Unimplemented
                | tonic::Code::PermissionDenied
                | tonic::Code::Unauthenticated,
            ) => Self::Refused,
            Some(tonic::Code::ResourceExhausted) => Self::Banned,
            _ => Self::Other,
        }
    }
}

/// What to open next, and after how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub transport: Transport,
    pub wait: Duration,
}

#[derive(Debug)]
pub struct TransportPolicy {
    mode: TransportMode,
    ban_cooldown: Duration,
    stream_retry: Duration,
    stream_failures: u32,
    poll_failures: u32,
    /// `auto` mode while polling: when to try the stream again.
    stream_retry_at: Option<Instant>,
}

/// `BACKOFF_BASE * 2^(failures - 1)`, capped at `BACKOFF_CAP`.
pub fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    (BACKOFF_BASE * 2u32.pow(doublings)).min(BACKOFF_CAP)
}

impl TransportPolicy {
    pub fn new(config: &BlockWatchConfig) -> Self {
        Self {
            mode: config.transport,
            ban_cooldown: config.ban_cooldown(),
            stream_retry: config.stream_retry(),
            stream_failures: 0,
            poll_failures: 0,
            stream_retry_at: None,
        }
    }

    /// The transport to open at `now`.
    pub fn transport(&self, now: Instant) -> Transport {
        match self.mode {
            TransportMode::Stream => Transport::Stream,
            TransportMode::Poll => Transport::Poll,
            TransportMode::Auto => {
                if self.stream_retry_at.is_some_and(|at| now < at) {
                    Transport::Poll
                } else {
                    Transport::Stream
                }
            }
        }
    }

    /// `auto` mode on the polling transport: when the stream is due to be
    /// tried again.
    pub fn stream_retry_at(&self) -> Option<Instant> {
        match self.mode {
            TransportMode::Auto => self.stream_retry_at,
            TransportMode::Stream | TransportMode::Poll => None,
        }
    }

    /// A source delivered a block: its transport works.
    pub fn on_block(&mut self, transport: Transport) {
        match transport {
            Transport::Stream => self.stream_failures = 0,
            Transport::Poll => self.poll_failures = 0,
        }
    }

    /// A source over `transport` failed to open or ended with a failure of
    /// `kind` at `now`.
    pub fn on_failure(
        &mut self,
        transport: Transport,
        kind: FailureKind,
        now: Instant,
    ) -> Decision {
        match (transport, self.mode) {
            (Transport::Stream, TransportMode::Auto) => match kind {
                FailureKind::Refused => self.poll_until(now + self.stream_retry),
                FailureKind::Banned => self.poll_until(now + self.ban_cooldown),
                FailureKind::Other => {
                    self.stream_failures += 1;
                    if self.stream_failures >= STREAM_FAILURES_BEFORE_POLLING {
                        self.poll_until(now + self.stream_retry)
                    } else {
                        Decision {
                            transport: Transport::Stream,
                            wait: backoff(self.stream_failures),
                        }
                    }
                }
            },
            // The operator chose the stream: wait it out.
            (Transport::Stream, _) => {
                let wait = match kind {
                    FailureKind::Refused => self.stream_retry,
                    FailureKind::Banned => self.ban_cooldown,
                    FailureKind::Other => {
                        self.stream_failures += 1;
                        backoff(self.stream_failures)
                    }
                };
                Decision {
                    transport: Transport::Stream,
                    wait,
                }
            }
            (Transport::Poll, _) => {
                self.poll_failures += 1;
                let wait = backoff(self.poll_failures);
                Decision {
                    transport: self.transport(now + wait),
                    wait,
                }
            }
        }
    }

    fn poll_until(&mut self, at: Instant) -> Decision {
        self.stream_failures = 0;
        self.stream_retry_at = Some(at);
        Decision {
            transport: Transport::Poll,
            wait: Duration::ZERO,
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    fn policy(mode: TransportMode) -> TransportPolicy {
        TransportPolicy::new(
            &BlockWatchConfig::default()
                .with_transport(mode)
                .with_ban_cooldown_seconds(600)
                .with_stream_retry_seconds(300),
        )
    }

    fn status(code: tonic::Code) -> BlockSourceError {
        BlockSourceError::Status {
            code,
            message: String::new(),
        }
    }

    #[test]
    fn failures_are_classified_by_status() {
        for code in [
            tonic::Code::Unimplemented,
            tonic::Code::PermissionDenied,
            tonic::Code::Unauthenticated,
        ] {
            assert_eq!(FailureKind::of(&status(code)), FailureKind::Refused);
        }
        assert_eq!(
            FailureKind::of(&status(tonic::Code::ResourceExhausted)),
            FailureKind::Banned
        );
        for error in [
            status(tonic::Code::Unavailable),
            status(tonic::Code::DataLoss),
            status(tonic::Code::Internal),
            BlockSourceError::Transport("reset".into()),
            BlockSourceError::Ended,
            BlockSourceError::OutOfSequence {
                expected: 1,
                got: 3,
            },
        ] {
            assert_eq!(FailureKind::of(&error), FailureKind::Other, "{error}");
        }
    }

    #[test]
    fn backoff_doubles_from_one_second_to_a_minute() {
        let waits: Vec<u64> = (1..=8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(u32::MAX), BACKOFF_CAP);
    }

    #[tokio::test(start_paused = true)]
    async fn auto_mode_starts_on_the_stream() {
        let policy = policy(TransportMode::Auto);
        assert_eq!(policy.transport(Instant::now()), Transport::Stream);
        assert_eq!(policy.stream_retry_at(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_the_node_does_not_serve_sends_auto_mode_polling_until_the_retry() {
        let mut policy = policy(TransportMode::Auto);
        let now = Instant::now();
        let decision = policy.on_failure(Transport::Stream, FailureKind::Refused, now);
        assert_eq!(
            decision,
            Decision {
                transport: Transport::Poll,
                wait: Duration::ZERO
            }
        );
        let retry = now + Duration::from_secs(300);
        assert_eq!(policy.stream_retry_at(), Some(retry));
        assert_eq!(
            policy.transport(retry - Duration::from_secs(1)),
            Transport::Poll
        );
        assert_eq!(policy.transport(retry), Transport::Stream);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lag_disconnect_polls_for_at_least_the_ban() {
        let mut policy = policy(TransportMode::Auto);
        let now = Instant::now();
        let decision = policy.on_failure(Transport::Stream, FailureKind::Banned, now);
        assert_eq!(decision.transport, Transport::Poll);
        assert_eq!(
            policy.stream_retry_at(),
            Some(now + Duration::from_secs(600))
        );
        assert_eq!(
            policy.transport(now + Duration::from_secs(599)),
            Transport::Poll,
            "never back on the stream inside the node's ban"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn other_stream_failures_back_off_then_fall_back_after_five_in_a_row() {
        let mut policy = policy(TransportMode::Auto);
        let now = Instant::now();
        let mut waits = Vec::new();
        for _ in 0..4 {
            let decision = policy.on_failure(Transport::Stream, FailureKind::Other, now);
            assert_eq!(decision.transport, Transport::Stream);
            waits.push(decision.wait.as_secs());
        }
        assert_eq!(waits, vec![1, 2, 4, 8]);
        let fifth = policy.on_failure(Transport::Stream, FailureKind::Other, now);
        assert_eq!(
            fifth,
            Decision {
                transport: Transport::Poll,
                wait: Duration::ZERO
            }
        );
        assert_eq!(
            policy.stream_retry_at(),
            Some(now + Duration::from_secs(300))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_delivered_block_resets_the_failure_count() {
        let mut policy = policy(TransportMode::Auto);
        let now = Instant::now();
        for _ in 0..4 {
            policy.on_failure(Transport::Stream, FailureKind::Other, now);
        }
        policy.on_block(Transport::Stream);
        let decision = policy.on_failure(Transport::Stream, FailureKind::Other, now);
        assert_eq!(
            decision,
            Decision {
                transport: Transport::Stream,
                wait: Duration::from_secs(1)
            },
            "a working stream starts the count over"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_poll_backs_off_and_hands_back_to_the_stream_once_due() {
        let mut policy = policy(TransportMode::Auto);
        let now = Instant::now();
        policy.on_failure(Transport::Stream, FailureKind::Refused, now);
        let first = policy.on_failure(Transport::Poll, FailureKind::Other, now);
        assert_eq!(
            first,
            Decision {
                transport: Transport::Poll,
                wait: Duration::from_secs(1)
            }
        );
        let second = policy.on_failure(Transport::Poll, FailureKind::Other, now);
        assert_eq!(second.wait, Duration::from_secs(2));
        policy.on_block(Transport::Poll);
        assert_eq!(
            policy
                .on_failure(Transport::Poll, FailureKind::Other, now)
                .wait,
            Duration::from_secs(1)
        );
        // A poll failure past the stream retry hands over to the stream.
        let late = now + Duration::from_secs(300);
        assert_eq!(
            policy
                .on_failure(Transport::Poll, FailureKind::Other, late)
                .transport,
            Transport::Stream
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stream_mode_never_polls() {
        let mut policy = policy(TransportMode::Stream);
        let now = Instant::now();
        assert_eq!(
            policy.on_failure(Transport::Stream, FailureKind::Refused, now),
            Decision {
                transport: Transport::Stream,
                wait: Duration::from_secs(300)
            }
        );
        assert_eq!(
            policy.on_failure(Transport::Stream, FailureKind::Banned, now),
            Decision {
                transport: Transport::Stream,
                wait: Duration::from_secs(600)
            },
            "the ban is waited out on the stream itself"
        );
        for expected in [1, 2, 4, 8, 16, 32, 60] {
            let decision = policy.on_failure(Transport::Stream, FailureKind::Other, now);
            assert_eq!(decision.transport, Transport::Stream);
            assert_eq!(decision.wait.as_secs(), expected);
        }
        assert_eq!(policy.stream_retry_at(), None);
        assert_eq!(policy.transport(now), Transport::Stream);
    }

    #[tokio::test(start_paused = true)]
    async fn poll_mode_never_subscribes() {
        let mut policy = policy(TransportMode::Poll);
        let now = Instant::now();
        assert_eq!(policy.transport(now), Transport::Poll);
        let decision = policy.on_failure(Transport::Poll, FailureKind::Banned, now);
        assert_eq!(
            decision,
            Decision {
                transport: Transport::Poll,
                wait: Duration::from_secs(1)
            }
        );
        assert_eq!(policy.stream_retry_at(), None);
    }
}
