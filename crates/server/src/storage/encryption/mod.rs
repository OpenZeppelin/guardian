//! Application-layer encryption of sensitive storage payloads.
//!
//! Sensitive payloads (account state, delta/proposal payloads) are
//! authenticated-encrypted into a self-describing [`envelope::Envelope`] before
//! they reach the concrete backend and decrypted on read, so layers above the
//! storage boundary see unchanged objects. Routing/index fields stay plaintext.

pub(crate) mod cipher;
pub(crate) mod decorator;
pub(crate) mod envelope;
pub(crate) mod key_provider;
pub(crate) mod marker;
