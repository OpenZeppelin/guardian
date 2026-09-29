//! Chain-driven release detection (issue #434).
//!
//! The push-path hook (`services::release_on_switch`) releases an account
//! when a `SwitchGuardian` delta canonicalizes here. Switches that never
//! reach the push path — the offline switch path, a failed best-effort
//! push, a client older than the push mechanism, a switch executed while
//! this server was unreachable — left the old guardian serving the account
//! as active forever: stale reads, dead pending proposals, and a capacity
//! slot held for nothing. This background task closes that gap by asking
//! the chain directly, with three complementary detectors:
//!
//! * **Proposal match.** In the normal online flow the switch proposal is
//!   pushed to the old guardian first and sits there pending once the
//!   switch executes elsewhere. Its summary applied to the stored state
//!   gives the exact post-switch commitment. When the chain sits at that
//!   commitment, or the account's transaction history holds a transaction
//!   that ended at it, the switch provably executed: a lagging node cannot
//!   invent the commitment, and transaction headers are public for every
//!   account. So it covers **private** accounts, even ones that transacted
//!   again under the new guardian, and resolves the stale proposal at the
//!   same time. Every pending proposal counts, whatever its label and
//!   whatever base it was recorded against (the candidate queue records a
//!   proposal against its tail, issue #17, even when the client built it
//!   on the stored state), and so does a switch delta canonicalization
//!   retained on the stored base (its proposal is gone by then, its
//!   payload is not). A switch delta queued behind another candidate is
//!   not matched yet (issue #504).
//! * **Stored state.** The stored state's own guardian key is checked once
//!   per stored state: a promoted switch whose push-path release was never
//!   written is released here, proved by the ack this server signed for
//!   that delta with its current key, wherever the chain has gone since.
//! * **Storage read.** For accounts with public state, the guardian
//!   `pub_key` slot is read straight from published on-chain storage and
//!   compared with this server's key. This covers switches with no
//!   proposal on the old guardian (offline switches, other clients). A
//!   read of a state older than the stored one is never evidence, and a
//!   foreign key must be seen again at a strictly later block
//!   (`confirmations`) before it releases.
//!
//! A key counts as foreign only when it is neither this server's nor the
//! stored base's. A server whose own ack key changed (a new ack secret, or
//! the ephemeral keys a non-prod server generates on every boot) therefore
//! sees its accounts as bound to a key it does not hold
//! (`own_key_mismatch`) instead of as switched, and releases none of them.
//!
//! Release detection is not latency-sensitive (an undetected switch costs
//! stale reads and a confusing failure for a lagging cosigner, never funds
//! or custody), so the sweep is its own task, independent of the
//! canonicalization loop: one paced loop visits one account at a time,
//! walking the fleet once per `rotation_seconds` and interleaving the few
//! confirmation re-checks, never faster than `max_rate_per_second` in
//! total. One replica holds the `release_sweep` lease at a time.

mod sweep;
mod worker;

pub use sweep::{SweepSummary, run_release_sweep_now};
pub use worker::start_release_sweep_worker;
