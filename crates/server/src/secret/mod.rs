//! Memory-resident secret hygiene wrappers.
//!
//! The bytes of each wrapper are reachable only through `expose_secret()` (and
//! `CredentialUrl::scheme_and_host()` for safe logging). `Display`, `Serialize`,
//! and `Deserialize` are intentionally not implemented; `Debug` renders only a
//! non-disclosing marker. Drop zeroes the backing buffer.

mod ct;
mod wrappers;

#[cfg(test)]
pub(crate) use ct::eq as ct_eq;
pub(crate) use wrappers::{CredentialUrl, FixedKey, SecretBytes, SecretString};

#[cfg(test)]
mod tests;
