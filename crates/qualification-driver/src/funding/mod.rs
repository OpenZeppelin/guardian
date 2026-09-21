/// Where the treasury keeps the state that must be shared by everything that
/// spends from it: the lock that serializes access and the ledger that enforces
/// the spend cap.
///
/// Both only work while every spender agrees on the directory. The Rust
/// scenarios once passed their per-run account directory here while the
/// TypeScript leg's `fund` subprocess used this default, so the two legs took
/// different locks and kept separate tallies: the cap never saw a whole run and
/// the per-run reset cleared a file the Rust leg never wrote, leaving a tally
/// that only grew.
pub const DEFAULT_TREASURY_DIR: &str = "/tmp/qualification-treasury";

pub mod bootstrap;
pub mod budget;
pub mod fees;
pub mod lock;
pub mod network;
pub mod service;
pub mod summary;
pub mod transfer;
pub mod treasury;
pub mod usability;

pub use treasury::Treasury;
