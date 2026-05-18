mod allowlist;
mod config;
pub mod cursor;
mod middleware;
pub mod permissions;
mod state;
mod types;
mod util;

pub mod authz;
#[cfg(feature = "authz-test-probe")]
pub mod probe;

pub use config::DashboardConfig;
pub use middleware::{extract_cookie, require_dashboard_session};
pub use state::DashboardState;
pub use types::{
    AuthenticatedOperator, IssuedOperatorSession, OperatorChallenge, OperatorChallengePayload,
};
