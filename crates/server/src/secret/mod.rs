//! Memory-resident secret hygiene wrappers.
//!
//! The bytes of each wrapper are reachable only through `expose_secret()` (and
//! `CredentialUrl::scheme_and_host()` for safe logging). `Display`, `Serialize`,
//! and `Deserialize` are intentionally not implemented; `Debug` renders only a
//! non-disclosing marker. Drop zeroes the backing buffer.
//!
//! # Which wrapper to use
//!
//! - [`FixedKey`]`<N>` — secret key material of a known compile-time length
//!   (HMAC keys, symmetric keys). Equality is constant-time. Use this over
//!   [`SecretBytes`] whenever the length is fixed, so the type carries the size
//!   invariant.
//! - [`SecretBytes`] — secret byte material whose length is only known at
//!   runtime (e.g. hex-decoded key bytes pulled from a secrets backend).
//! - [`SecretString`] — a secret UTF-8 string with no internal structure
//!   worth exposing (API keys, bearer tokens, raw env-var secrets).
//! - [`CredentialUrl`] — a URL that may embed credentials in its userinfo or
//!   query (database URLs, RPC endpoints). Adds [`CredentialUrl::scheme_and_host`]
//!   so the non-secret `scheme://host[:port]` portion can be logged safely.
//!
//! Sibling [`session_digest`] is not a wrapper: it derives the SHA-256 storage
//! key for a session token so the plaintext token is never retained in a
//! long-lived map.
//!
//! When adding a new long-lived secret-bearing field in `crates/server`, reach
//! for one of these instead of a bare `String`/`Vec<u8>`; the compile-time
//! assertions in the `tests` module enforce that they can never be logged or
//! serialized.

mod ct;
mod digest;
mod wrappers;

pub(crate) use ct::eq as ct_eq;
pub(crate) use digest::session_digest;
pub(crate) use wrappers::{CredentialUrl, FixedKey, SecretBytes, SecretString};

#[cfg(test)]
mod tests;
