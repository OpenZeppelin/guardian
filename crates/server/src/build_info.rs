//! Build identity surfaced via the dashboard info endpoint.
//!
//! Values are captured at compile time by `build.rs`. `GIT_SHA` reads
//! `GUARDIAN_GIT_SHA` (overridable in CI/Docker) and falls back to
//! `git rev-parse --short=12 HEAD`; if neither is available the value is
//! `"unknown"`.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("GUARDIAN_GIT_SHA");

pub fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
