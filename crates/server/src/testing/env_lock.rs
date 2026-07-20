use std::sync::Mutex;

/// Process-wide lock for tests that mutate environment variables. The process
/// environment is one shared global, so per-module locks do not serialize
/// against each other: two test modules that touch the *same* variable under
/// *different* locks race under the multi-threaded test runner. Every test
/// module that reads or writes `GUARDIAN_MAX_REPLICAS` (rate-limit config,
/// dashboard config) must hold this lock; modules whose variables are private
/// to them may keep a local lock.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());
