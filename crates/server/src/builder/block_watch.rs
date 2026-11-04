//! Configuration of block watch (issue #585): the release sweep lease
//! holder consumes the node's committed-block feed and hands every guarded
//! account that moved somewhere this server does not expect to the release
//! detectors, instead of finding such moves by walking the fleet. See
//! `jobs::block_watch` for the feed itself.

use std::time::Duration;

/// Environment override for [`BlockWatchConfig::enabled`].
pub const ENV_ENABLED: &str = "GUARDIAN_BLOCK_WATCH_ENABLED";
/// Environment override for [`BlockWatchConfig::transport`].
pub const ENV_TRANSPORT: &str = "GUARDIAN_BLOCK_WATCH_TRANSPORT";
/// Environment override for [`BlockWatchConfig::poll_seconds`].
pub const ENV_POLL_SECONDS: &str = "GUARDIAN_BLOCK_WATCH_POLL_SECONDS";
/// Environment override for [`BlockWatchConfig::poll_max_rate_per_second`].
pub const ENV_POLL_MAX_RATE_PER_SECOND: &str = "GUARDIAN_BLOCK_WATCH_POLL_MAX_RATE_PER_SECOND";
/// Environment override for [`BlockWatchConfig::stall_seconds`].
pub const ENV_STALL_SECONDS: &str = "GUARDIAN_BLOCK_WATCH_STALL_SECONDS";
/// Environment override for [`BlockWatchConfig::max_catchup_blocks`].
pub const ENV_MAX_CATCHUP_BLOCKS: &str = "GUARDIAN_BLOCK_WATCH_MAX_CATCHUP_BLOCKS";
/// Environment override for [`BlockWatchConfig::ban_cooldown_seconds`].
pub const ENV_BAN_COOLDOWN_SECONDS: &str = "GUARDIAN_BLOCK_WATCH_BAN_COOLDOWN_SECONDS";
/// Environment override for [`BlockWatchConfig::stream_retry_seconds`].
pub const ENV_STREAM_RETRY_SECONDS: &str = "GUARDIAN_BLOCK_WATCH_STREAM_RETRY_SECONDS";
/// Environment override for [`BlockWatchConfig::max_attempts`].
pub const ENV_MAX_ATTEMPTS: &str = "GUARDIAN_BLOCK_WATCH_MAX_ATTEMPTS";

pub const MAX_POLL_SECONDS: u64 = 300;
pub const MAX_POLL_RATE_PER_SECOND: u32 = 1_000;
pub const MIN_STALL_SECONDS: u64 = 5;
pub const MAX_STALL_SECONDS: u64 = 3_600;
pub const MAX_CATCHUP_BLOCKS: u32 = 1_000_000;
pub const MAX_COOLDOWN_SECONDS: u64 = 86_400;
pub const MAX_ATTEMPTS: u32 = 1_000;

/// How block watch reads committed blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportMode {
    /// The node's `BlockSubscription` stream, falling back to
    /// `GetBlockByNumber` polling (with cooldowns) when the stream is
    /// unavailable, refused, or closed for lag.
    Auto,
    /// The stream only: reconnects, never polls.
    Stream,
    /// Polling only: never subscribes.
    Poll,
}

impl TransportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Stream => "stream",
            Self::Poll => "poll",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockWatchConfig {
    /// Whether block watch runs.
    pub enabled: bool,

    pub transport: TransportMode,

    /// Chain-tip read interval on the polling transport once caught up
    /// (Miden commits a block every 3 s). Behind the tip, blocks are read
    /// at `poll_max_rate_per_second`.
    pub poll_seconds: u64,

    /// Most blocks read per second on the polling transport while behind
    /// the tip (a replay after downtime or a lease failover, up to
    /// `max_catchup_blocks`): each is one `GetBlockByNumber` and one cursor
    /// write. Block watch polls when the node refused or banned the
    /// stream, so a replay is spread out rather than sent as fast as the
    /// node answers. Any rate above one block per 3 s catches up; the
    /// default replays a full 10,000-block catch-up in about 17 minutes.
    /// The stream replays at the node's own pace.
    pub poll_max_rate_per_second: u32,

    /// No block for this long triggers a chain-tip read; a tip ahead of
    /// the cursor means the feed stalled, and it is reopened.
    pub stall_seconds: u64,

    /// Largest gap between the cursor and the chain tip replayed when
    /// block watch (re)starts. A larger gap, or no cursor at all (the
    /// first start), starts at the tip instead and requests one walk of
    /// the release sweep rotation to cover the blocks it skipped. `0`
    /// always starts at the tip.
    pub max_catchup_blocks: u32,

    /// After the node closed the stream as too slow (`RESOURCE_EXHAUSTED`:
    /// the node then refuses the caller for 10 minutes), how long block
    /// watch polls before trying the stream again.
    pub ban_cooldown_seconds: u64,

    /// How long block watch polls after the stream was unavailable
    /// (`UNIMPLEMENTED`, `PERMISSION_DENIED`, repeated failures) before
    /// trying it again.
    pub stream_retry_seconds: u64,

    /// Visits of one observation that end without checking the account
    /// (a chain read failed) before it is dropped, once one walk of the
    /// rotation is recorded to cover it. A visit that waits (own candidate
    /// in flight, open streak) spends no attempt.
    pub max_attempts: u32,
}

impl Default for BlockWatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: TransportMode::Auto,
            poll_seconds: 3,
            poll_max_rate_per_second: 10,
            stall_seconds: 60,
            max_catchup_blocks: 10_000, // ~8 h of blocks at 3 s
            ban_cooldown_seconds: 600,  // the node's ban is 10 min
            stream_retry_seconds: 600,
            max_attempts: 20,
        }
    }
}

impl BlockWatchConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_seconds)
    }

    /// Shortest gap between two block reads on the polling transport.
    pub fn poll_block_spacing(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.poll_max_rate_per_second.max(1)))
    }

    pub fn stall(&self) -> Duration {
        Duration::from_secs(self.stall_seconds)
    }

    pub fn ban_cooldown(&self) -> Duration {
        Duration::from_secs(self.ban_cooldown_seconds)
    }

    pub fn stream_retry(&self) -> Duration {
        Duration::from_secs(self.stream_retry_seconds)
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_transport(mut self, transport: TransportMode) -> Self {
        self.transport = transport;
        self
    }

    pub fn with_poll_seconds(mut self, seconds: u64) -> Self {
        assert!(
            (1..=MAX_POLL_SECONDS).contains(&seconds),
            "block watch poll interval must be between 1 and {MAX_POLL_SECONDS} seconds"
        );
        self.poll_seconds = seconds;
        self
    }

    pub fn with_poll_max_rate_per_second(mut self, rate: u32) -> Self {
        assert!(
            (1..=MAX_POLL_RATE_PER_SECOND).contains(&rate),
            "block watch poll rate must be between 1 and {MAX_POLL_RATE_PER_SECOND} blocks per \
             second"
        );
        self.poll_max_rate_per_second = rate;
        self
    }

    pub fn with_stall_seconds(mut self, seconds: u64) -> Self {
        assert!(
            (MIN_STALL_SECONDS..=MAX_STALL_SECONDS).contains(&seconds),
            "block watch stall timeout must be between {MIN_STALL_SECONDS} and \
             {MAX_STALL_SECONDS} seconds"
        );
        self.stall_seconds = seconds;
        self
    }

    pub fn with_max_catchup_blocks(mut self, blocks: u32) -> Self {
        assert!(
            blocks <= MAX_CATCHUP_BLOCKS,
            "block watch catch-up must be at most {MAX_CATCHUP_BLOCKS} blocks"
        );
        self.max_catchup_blocks = blocks;
        self
    }

    pub fn with_ban_cooldown_seconds(mut self, seconds: u64) -> Self {
        assert!(
            (1..=MAX_COOLDOWN_SECONDS).contains(&seconds),
            "block watch ban cooldown must be between 1 and {MAX_COOLDOWN_SECONDS} seconds"
        );
        self.ban_cooldown_seconds = seconds;
        self
    }

    pub fn with_stream_retry_seconds(mut self, seconds: u64) -> Self {
        assert!(
            (1..=MAX_COOLDOWN_SECONDS).contains(&seconds),
            "block watch stream retry must be between 1 and {MAX_COOLDOWN_SECONDS} seconds"
        );
        self.stream_retry_seconds = seconds;
        self
    }

    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        assert!(
            (1..=MAX_ATTEMPTS).contains(&attempts),
            "block watch max attempts must be between 1 and {MAX_ATTEMPTS}"
        );
        self.max_attempts = attempts;
        self
    }

    /// Settings that cannot be checked one at a time: on the polling
    /// transport a tip read every `poll_seconds` must come before the stall
    /// timeout, or every idle wait would read as a stall.
    pub fn validate(&self) -> Result<(), String> {
        if self.poll_seconds >= self.stall_seconds {
            return Err(format!(
                "{ENV_POLL_SECONDS} ({}) must be less than {ENV_STALL_SECONDS} ({})",
                self.poll_seconds, self.stall_seconds
            ));
        }
        Ok(())
    }

    /// Defaults with every `GUARDIAN_BLOCK_WATCH_*` override applied. A
    /// present-but-invalid value fails startup loudly. With
    /// `GUARDIAN_BLOCK_WATCH_ENABLED` unset, block watch follows
    /// `release_sweep_enabled`: a deployment that turned the release sweep
    /// off (`GUARDIAN_RELEASE_SWEEP_ENABLED=false`, its kill switch before
    /// block watch existed) does not start block watch by upgrading.
    pub fn from_env_following(release_sweep_enabled: bool) -> Result<Self, String> {
        Self::default().with_env_overrides_following(&ENV_VARS, release_sweep_enabled)
    }

    /// [`Self::from_env_following`] with the release sweep enabled.
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_following(true)
    }

    fn with_env_overrides_following(
        self,
        vars: &EnvVars<'_>,
        enabled_by_default: bool,
    ) -> Result<Self, String> {
        let config = self
            .with_enabled(enabled_by_default)
            .with_env_overrides(vars)?;
        config.validate()?;
        Ok(config)
    }
}

const ENV_VARS: EnvVars<'static> = EnvVars {
    enabled: ENV_ENABLED,
    transport: ENV_TRANSPORT,
    poll_seconds: ENV_POLL_SECONDS,
    poll_max_rate_per_second: ENV_POLL_MAX_RATE_PER_SECOND,
    stall_seconds: ENV_STALL_SECONDS,
    max_catchup_blocks: ENV_MAX_CATCHUP_BLOCKS,
    ban_cooldown_seconds: ENV_BAN_COOLDOWN_SECONDS,
    stream_retry_seconds: ENV_STREAM_RETRY_SECONDS,
    max_attempts: ENV_MAX_ATTEMPTS,
};

impl BlockWatchConfig {
    fn with_env_overrides(self, vars: &EnvVars<'_>) -> Result<Self, String> {
        let mut config = self;
        if let Some(enabled) = bool_from_var(vars.enabled)? {
            config = config.with_enabled(enabled);
        }
        if let Some(transport) = transport_from_var(vars.transport)? {
            config = config.with_transport(transport);
        }
        if let Some(seconds) = bounded_from_var(vars.poll_seconds, 1, MAX_POLL_SECONDS)? {
            config = config.with_poll_seconds(seconds);
        }
        if let Some(rate) = bounded_from_var(
            vars.poll_max_rate_per_second,
            1,
            u64::from(MAX_POLL_RATE_PER_SECOND),
        )? {
            config = config.with_poll_max_rate_per_second(to_u32(rate));
        }
        if let Some(seconds) =
            bounded_from_var(vars.stall_seconds, MIN_STALL_SECONDS, MAX_STALL_SECONDS)?
        {
            config = config.with_stall_seconds(seconds);
        }
        if let Some(blocks) =
            bounded_from_var(vars.max_catchup_blocks, 0, u64::from(MAX_CATCHUP_BLOCKS))?
        {
            config = config.with_max_catchup_blocks(to_u32(blocks));
        }
        if let Some(seconds) = bounded_from_var(vars.ban_cooldown_seconds, 1, MAX_COOLDOWN_SECONDS)?
        {
            config = config.with_ban_cooldown_seconds(seconds);
        }
        if let Some(seconds) = bounded_from_var(vars.stream_retry_seconds, 1, MAX_COOLDOWN_SECONDS)?
        {
            config = config.with_stream_retry_seconds(seconds);
        }
        if let Some(attempts) = bounded_from_var(vars.max_attempts, 1, u64::from(MAX_ATTEMPTS))? {
            config = config.with_max_attempts(to_u32(attempts));
        }
        Ok(config)
    }
}

/// The variable names [`BlockWatchConfig::with_env_overrides`] reads, so
/// tests can use names of their own.
struct EnvVars<'a> {
    enabled: &'a str,
    transport: &'a str,
    poll_seconds: &'a str,
    poll_max_rate_per_second: &'a str,
    stall_seconds: &'a str,
    max_catchup_blocks: &'a str,
    ban_cooldown_seconds: &'a str,
    stream_retry_seconds: &'a str,
    max_attempts: &'a str,
}

fn var(var_name: &str) -> Result<Option<String>, String> {
    match std::env::var(var_name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{var_name} must contain valid UTF-8"))
        }
    }
}

fn bool_from_var(var_name: &str) -> Result<Option<bool>, String> {
    var(var_name)?
        .map(|value| {
            value
                .trim()
                .parse::<bool>()
                .map_err(|_| format!("{var_name} must be 'true' or 'false', got {value:?}"))
        })
        .transpose()
}

fn transport_from_var(var_name: &str) -> Result<Option<TransportMode>, String> {
    var(var_name)?
        .map(|value| match value.trim() {
            "auto" => Ok(TransportMode::Auto),
            "stream" => Ok(TransportMode::Stream),
            "poll" => Ok(TransportMode::Poll),
            _ => Err(format!(
                "{var_name} must be 'auto', 'stream' or 'poll', got {value:?}"
            )),
        })
        .transpose()
}

/// An integer in `min..=max`, or `None` when the variable is unset.
fn bounded_from_var(var_name: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    var(var_name)?
        .map(|value| match value.trim().parse::<u64>() {
            Ok(parsed) if (min..=max).contains(&parsed) => Ok(parsed),
            _ => Err(format!(
                "{var_name} must be an integer between {min} and {max}, got {value:?}"
            )),
        })
        .transpose()
}

/// Every `u32` setting's bound fits in a `u32`.
fn to_u32(value: u64) -> u32 {
    u32::try_from(value).expect("bounded below a u32 maximum")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::env_lock::ENV_LOCK;

    const VARS: EnvVars<'static> = EnvVars {
        enabled: "GUARDIAN_BW_TEST_ENABLED",
        transport: "GUARDIAN_BW_TEST_TRANSPORT",
        poll_seconds: "GUARDIAN_BW_TEST_POLL",
        poll_max_rate_per_second: "GUARDIAN_BW_TEST_POLL_RATE",
        stall_seconds: "GUARDIAN_BW_TEST_STALL",
        max_catchup_blocks: "GUARDIAN_BW_TEST_CATCHUP",
        ban_cooldown_seconds: "GUARDIAN_BW_TEST_BAN",
        stream_retry_seconds: "GUARDIAN_BW_TEST_RETRY",
        max_attempts: "GUARDIAN_BW_TEST_ATTEMPTS",
    };

    fn all_vars() -> [&'static str; 9] {
        [
            VARS.enabled,
            VARS.transport,
            VARS.poll_seconds,
            VARS.poll_max_rate_per_second,
            VARS.stall_seconds,
            VARS.max_catchup_blocks,
            VARS.ban_cooldown_seconds,
            VARS.stream_retry_seconds,
            VARS.max_attempts,
        ]
    }

    fn clear_vars() {
        for var in all_vars() {
            unsafe { std::env::remove_var(var) };
        }
    }

    fn from_test_vars() -> Result<BlockWatchConfig, String> {
        BlockWatchConfig::default().with_env_overrides_following(&VARS, true)
    }

    #[test]
    fn defaults_stream_with_a_polling_fallback_and_match_the_nodes_limits() {
        let config = BlockWatchConfig::default();
        assert!(config.enabled);
        assert_eq!(config.transport, TransportMode::Auto);
        assert_eq!(config.poll_seconds, 3);
        assert_eq!(config.poll_max_rate_per_second, 10);
        assert_eq!(config.poll_block_spacing(), Duration::from_millis(100));
        assert_eq!(config.stall_seconds, 60);
        assert_eq!(config.max_catchup_blocks, 10_000);
        assert_eq!(
            config.ban_cooldown_seconds, 600,
            "at least the node's 10-minute ban"
        );
        assert_eq!(config.stream_retry_seconds, 600);
        assert_eq!(config.max_attempts, 20);
    }

    #[test]
    fn env_overrides_apply_and_missing_keeps_defaults() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        assert_eq!(from_test_vars().unwrap(), BlockWatchConfig::default());

        unsafe {
            std::env::set_var(VARS.enabled, "false");
            std::env::set_var(VARS.transport, "poll");
            std::env::set_var(VARS.poll_seconds, "5");
            std::env::set_var(VARS.poll_max_rate_per_second, "50");
            std::env::set_var(VARS.stall_seconds, "30");
            std::env::set_var(VARS.max_catchup_blocks, "0");
            std::env::set_var(VARS.ban_cooldown_seconds, "900");
            std::env::set_var(VARS.stream_retry_seconds, "120");
            std::env::set_var(VARS.max_attempts, "3");
        }
        let config = from_test_vars().expect("valid overrides apply");
        assert!(!config.enabled);
        assert_eq!(config.transport, TransportMode::Poll);
        assert_eq!(config.poll_seconds, 5);
        assert_eq!(config.poll_max_rate_per_second, 50);
        assert_eq!(config.stall_seconds, 30);
        assert_eq!(config.max_catchup_blocks, 0, "0 always starts at the tip");
        assert_eq!(config.ban_cooldown_seconds, 900);
        assert_eq!(config.stream_retry_seconds, 120);
        assert_eq!(config.max_attempts, 3);
        clear_vars();
    }

    #[test]
    fn env_overrides_reject_garbage_and_values_out_of_range() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        for (var, bad) in [
            (VARS.enabled, "yes"),
            (VARS.transport, "grpc"),
            (VARS.transport, "Stream"),
            (VARS.poll_seconds, "0"),
            (VARS.poll_seconds, "301"),
            (VARS.poll_max_rate_per_second, "0"),
            (VARS.poll_max_rate_per_second, "1001"),
            (VARS.poll_max_rate_per_second, "2.5"),
            (VARS.stall_seconds, "4"),
            (VARS.stall_seconds, "3601"),
            (VARS.max_catchup_blocks, "-1"),
            (VARS.max_catchup_blocks, "1000001"),
            (VARS.ban_cooldown_seconds, "0"),
            (VARS.ban_cooldown_seconds, "86401"),
            (VARS.stream_retry_seconds, "0"),
            (VARS.max_attempts, "0"),
            (VARS.max_attempts, "1001"),
            (VARS.poll_seconds, "18446744073709551616"),
        ] {
            unsafe { std::env::set_var(var, bad) };
            let error = from_test_vars().expect_err(&format!("{var}={bad} must fail startup"));
            assert!(error.contains(var), "the error names the variable: {error}");
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn an_unset_switch_follows_the_release_sweep_and_an_explicit_one_wins() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        let following = |release_sweep_enabled| {
            BlockWatchConfig::default()
                .with_env_overrides_following(&VARS, release_sweep_enabled)
                .unwrap()
                .enabled
        };
        assert!(following(true));
        assert!(
            !following(false),
            "the release sweep's kill switch keeps block watch off on upgrade"
        );
        unsafe { std::env::set_var(VARS.enabled, "true") };
        assert!(
            following(false),
            "block watch with the rotation off, on request"
        );
        unsafe { std::env::set_var(VARS.enabled, "false") };
        assert!(!following(true));
        clear_vars();
    }

    #[test]
    fn a_poll_interval_not_shorter_than_the_stall_timeout_fails_startup() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        for (poll, stall) in [("60", "60"), ("120", "60"), ("10", "5")] {
            unsafe {
                std::env::set_var(VARS.poll_seconds, poll);
                std::env::set_var(VARS.stall_seconds, stall);
            }
            let error = from_test_vars().expect_err(&format!("poll {poll} / stall {stall}"));
            assert!(error.contains("must be less than"), "{error}");
        }
        unsafe {
            std::env::set_var(VARS.poll_seconds, "59");
            std::env::set_var(VARS.stall_seconds, "60");
        }
        assert!(from_test_vars().is_ok());
        clear_vars();
        assert!(BlockWatchConfig::default().validate().is_ok());
    }

    #[test]
    fn env_overrides_trim_whitespace_and_accept_the_bounds() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        unsafe {
            std::env::set_var(VARS.enabled, " true ");
            std::env::set_var(VARS.transport, " stream\n");
            std::env::set_var(VARS.poll_seconds, "1");
            std::env::set_var(VARS.poll_max_rate_per_second, "1000");
            std::env::set_var(VARS.stall_seconds, " 5");
            std::env::set_var(VARS.max_catchup_blocks, "1000000");
            std::env::set_var(VARS.ban_cooldown_seconds, "86400");
            std::env::set_var(VARS.max_attempts, "1000");
        }
        let config = from_test_vars().expect("bounds are inclusive");
        assert!(config.enabled);
        assert_eq!(config.transport, TransportMode::Stream);
        assert_eq!(config.poll_seconds, 1);
        assert_eq!(config.poll_max_rate_per_second, MAX_POLL_RATE_PER_SECOND);
        assert_eq!(config.stall_seconds, MIN_STALL_SECONDS);
        // The largest poll interval with the largest stall timeout.
        unsafe {
            std::env::set_var(VARS.poll_seconds, "300");
            std::env::set_var(VARS.stall_seconds, "3600");
        }
        let config = from_test_vars().expect("bounds are inclusive");
        assert_eq!(config.poll_seconds, MAX_POLL_SECONDS);
        assert_eq!(config.stall_seconds, MAX_STALL_SECONDS);
        assert_eq!(config.max_catchup_blocks, MAX_CATCHUP_BLOCKS);
        assert_eq!(config.ban_cooldown_seconds, MAX_COOLDOWN_SECONDS);
        assert_eq!(config.max_attempts, MAX_ATTEMPTS);
        clear_vars();
    }
}
