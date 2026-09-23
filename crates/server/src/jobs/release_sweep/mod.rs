//! Chain-driven release detection (issue #434).
//!
//! The push-path hook (`services::release_on_switch`) releases an account
//! when a `SwitchGuardian` delta canonicalizes here. Switches that never
//! reach the push path — the offline switch path, a failed best-effort
//! push, a client older than the push mechanism, a switch executed while
//! this server was unreachable — left the old guardian serving the account
//! as active forever: stale reads, dead pending proposals, and a capacity
//! slot held for nothing. This background task closes that gap by asking
//! the chain directly, with two complementary detectors:
//!
//! * **Proposal match.** In the normal online flow the switch proposal is
//!   pushed to the old guardian first and sits there pending once the
//!   switch executes elsewhere. Its summary applied to the stored state
//!   gives the exact post-switch commitment; when the chain sits at that
//!   commitment the switch provably executed. This needs only the
//!   commitment probe, so it covers **private** accounts, and it resolves
//!   the stale proposal at the same time.
//! * **Storage read.** For accounts with public state, the guardian
//!   `pub_key` slot is read straight from published on-chain storage and
//!   compared with this server's key. This covers switches with no
//!   proposal on the old guardian (offline switches, other clients) and
//!   accounts that already moved past the post-switch commitment.
//!
//! Release detection is not latency-sensitive (an undetected switch costs
//! stale reads and a confusing failure for a lagging cosigner, never funds
//! or custody), so the sweep is its own task, independent of the
//! canonicalization loop: it walks the fleet slowly at a bounded RPC rate
//! (one rotation per `rotation_seconds`), and re-probes only the small
//! "hot" set — accounts with an open confirmation streak or a pending
//! `switch_guardian` proposal — on a short cadence. One replica holds the
//! `release_sweep` lease at a time.

mod sweep;
mod worker;

pub use sweep::{ReleaseSweeper, SweepState, SweepSummary, run_release_sweep_now};
pub use worker::start_release_sweep_worker;
