# Contract: `EcdsaSignerBackend` (internal trait)

This feature's primary contract is an **internal Rust trait**, not a network API.
No HTTP/gRPC surface changes (see `configuration.md` and research.md R6).

> **Error type**: `Result` below is `crate::error::Result<T>` = `Result<T, GuardianError>`.
> `crate::ack::Result` is a private re-import of exactly this alias — it does **not**
> resolve to `MidenEcdsaResult`. No new error enum is introduced: startup/config
> failures → `GuardianError::ConfigurationError`, runtime sign failures →
> `GuardianError::SigningError` (both have existing stable boundary mappings).

```rust
// crates/server/src/ack/miden_ecdsa/backend/mod.rs
use async_trait::async_trait;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use crate::error::Result;   // = Result<T, GuardianError>

#[async_trait]
pub(crate) trait EcdsaSignerBackend: Send + Sync {
    /// The secp256k1 public key this backend signs with.
    fn public_key(&self) -> PublicKey;

    /// Sign `message` under the scheme: returns a 65-byte (`r||s||v`) Miden
    /// signature over `Keccak256(message)` that verifies against `public_key()`.
    async fn sign(&self, message: Word) -> Result<Signature>;

    /// Stable backend identifier for diagnostics (`"in-memory"`, `"aws-kms"`).
    fn backend_id(&self) -> &'static str;
}
```

## Contract guarantees

| ID | Guarantee |
|---|---|
| C-1 | For any `message: Word`, `public_key().verify(message, &sign(message).await?)` returns `true`. |
| C-2 | `sign(...).await?.to_bytes()` is exactly 65 bytes (`r[32] \|\| s[32] \|\| v[1]`), format-identical to an in-memory signature. |
| C-3 | `public_key()` is stable for the lifetime of the backend and is the key used by every `sign`. |
| C-4 | Implementations never log, return, or otherwise expose private key material. |
| C-5 | A runtime signing failure returns `Err(..)` (no panic, no placeholder signature). |

## Conformance tests (apply to every implementation)

1. **Verifies**: sign a random `Word`, assert C-1.
2. **Format**: assert C-2 (65 bytes) and that the bytes deserialize back to an equal `Signature`.
3. **Determinism of identity**: `public_key()` constant across calls (C-3).
4. **Failure surfacing**: induced backend error → `Err`, process stays usable (C-5).

**Native-signature normalization (provider-specific, not universal)**: each
backend converts its provider's native signature encoding into Miden's 65-byte
`Signature`. The recovery id is always resolved by choosing the `id` where
`PublicKey::recover_from(message, &sig) == public_key()`. The encoding entry point
differs by provider:

- DER (AWS KMS) → `Signature::from_der`. Tested: low-S and high-S DER both convert to a verifying 65-byte signature.
- Raw `r||s` / SEC1 → `Signature::from_sec1_bytes_and_recovery_id`.
- Other (JOSE, recoverable, etc.) → extract `r,s` and build accordingly.

The AWS KMS implementation also performs a **startup sign probe** to prove
`kms:Sign` (see `configuration.md` / data-model.md), since fetching the public key
alone does not prove signing permission.

## Registration contract (`EcdsaBackendSelection`)

```rust
// Single edit point to add a provider (FR-010 / SC-005).
// `async` because hosted backends await client construction + key validation
// (e.g. aws_config::...load().await and KMS GetPublicKey at startup).
async fn build_ecdsa_backend(/* keystore_path, prod, config */)
    -> Result<Arc<dyn EcdsaSignerBackend>>
{
    match backend_id.as_deref() {
        None | Some("in-memory") => /* InMemoryEcdsaBackend (filesystem or Secrets-Manager-imported per `prod`) */,
        Some("aws-kms")          => /* AwsKmsEcdsaBackend::connect(...).await — no Secrets-Manager fetch */,
        Some(other)              => /* fail fast: ConfigurationError, unknown id `{other}`, supported: in-memory, aws-kms */,
    }
}
```

| ID | Guarantee |
|---|---|
| C-6 | Unset or `"in-memory"` → in-memory backend (default; no new config required). |
| C-7 | Unknown id → fail-fast error naming the id and listing supported ids. |
| C-8 | Adding a provider requires editing only its new module + this one match arm. |
