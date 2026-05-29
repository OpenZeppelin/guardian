# Contract: `crates/server/src/secret` Module API

**Feature**: `008-zeroize-secrets`
**Date**: 2026-05-29

> This feature introduces no HTTP / gRPC / SDK surface change. The only "contract" is the internal public API of the new `secret` module within `crates/server`. This document records that contract so future contributors can reason about what is allowed to change without breaking the security invariants.

---

## Module visibility

- Path: `crates/server/src/secret/`
- Visibility: `pub(crate)` — the module's items are reachable from anywhere inside `guardian-server`, but **not** exported from the crate. Nothing in this module appears in the server's public dependency graph for downstream consumers (clients, SDKs, npm packages).

## Public surface (within the crate)

```rust
// crates/server/src/secret/mod.rs
pub(crate) mod ct;
pub(crate) mod wrappers;

pub(crate) use wrappers::{FixedKey, SecretBytes, SecretString, CredentialUrl};
pub(crate) use ct::eq as ct_eq;
```

## `wrappers::FixedKey<const N: usize>`

```rust
pub(crate) struct FixedKey<const N: usize> { /* SecretBox<[u8; N]> */ }

impl<const N: usize> FixedKey<N> {
    pub(crate) fn new(bytes: [u8; N]) -> Self;
    pub(crate) fn expose_secret(&self) -> &[u8; N];
}

// SecretBox<T> is NOT Clone or PartialEq; every impl below is hand-rolled.
impl<const N: usize> Clone for FixedKey<N> { /* fresh SecretBox over a copied [u8; N] */ }
impl<const N: usize> std::fmt::Debug for FixedKey<N> { /* "FixedKey<N>(…)" */ }
impl<const N: usize> PartialEq for FixedKey<N> { /* subtle::ConstantTimeEq over inner bytes */ }
impl<const N: usize> Eq for FixedKey<N> {}
// Display: NOT IMPLEMENTED — compile-time enforced
// Serialize: NOT IMPLEMENTED — compile-time enforced
// Deserialize: NOT IMPLEMENTED — compile-time enforced
```

## `wrappers::SecretBytes`

```rust
pub(crate) struct SecretBytes { /* SecretBox<Vec<u8>> */ }

impl SecretBytes {
    pub(crate) fn new(bytes: Vec<u8>) -> Self;
    pub(crate) fn expose_secret(&self) -> &[u8];
    pub(crate) fn len(&self) -> usize; // safe metadata; does not leak content
}

// SecretBox<T> is NOT Clone or PartialEq; every impl below is hand-rolled.
impl Clone for SecretBytes { /* allocates a new Vec<u8> */ }
impl std::fmt::Debug for SecretBytes { /* "SecretBytes(len=N)" */ }
impl PartialEq for SecretBytes { /* subtle::ConstantTimeEq */ }
impl Eq for SecretBytes {}
// Display: NOT IMPLEMENTED
// Serialize/Deserialize: NOT IMPLEMENTED
```

## `wrappers::SecretString`

```rust
pub(crate) struct SecretString { /* SecretBox<String> */ }

impl SecretString {
    pub(crate) fn new(s: String) -> Self;
    pub(crate) fn expose_secret(&self) -> &str;
    pub(crate) fn len(&self) -> usize; // byte length; does not leak content
}

// SecretBox<T> is NOT Clone or PartialEq; every impl below is hand-rolled.
impl Clone for SecretString { /* allocates a new String */ }
impl std::fmt::Debug for SecretString { /* "SecretString(len=N)" */ }
impl PartialEq for SecretString { /* subtle::ConstantTimeEq over expose_secret().as_bytes() */ }
impl Eq for SecretString {}
// Display: NOT IMPLEMENTED
// Serialize/Deserialize: NOT IMPLEMENTED
```

## `wrappers::CredentialUrl`

```rust
pub(crate) struct CredentialUrl { /* SecretBox<String> */ }

impl CredentialUrl {
    pub(crate) fn new(url: String) -> Self;
    pub(crate) fn expose_secret(&self) -> &str;
    /// Returns `scheme://host[:port]` — safe to log. Returns "<invalid-url>" on parse failure.
    pub(crate) fn scheme_and_host(&self) -> String;
}

// SecretBox<T> is NOT Clone or PartialEq; every impl below is hand-rolled.
impl Clone for CredentialUrl { /* allocates a new String */ }
impl std::fmt::Debug for CredentialUrl { /* "CredentialUrl(<scheme_and_host>)" */ }
impl PartialEq for CredentialUrl { /* subtle::ConstantTimeEq over expose_secret().as_bytes() */ }
impl Eq for CredentialUrl {}
// REQUIRED by EvmChainConfig / EvmChainRegistry which derive PartialEq, Eq.
// Display: NOT IMPLEMENTED
// Serialize/Deserialize: NOT IMPLEMENTED
```

## `ct::eq`

```rust
/// Constant-time equality over byte slices. Available for future byte-by-byte
/// equality sites; not used by any current call site in this feature.
///
/// The cursor HMAC verify keeps `hmac::Mac::verify_slice` (Decision 8); session
/// lookup is keyed by digest (Decision 6). This function is `#[allow(dead_code)]`
/// at landing and is asserted by a unit test so it does not bit-rot silently.
///
/// Not for use with HashMap-keyed lookups (out of FR-004 scope; see spec.md User Story 3).
#[allow(dead_code)]
pub(crate) fn eq(a: &[u8], b: &[u8]) -> bool;
```

---

## Compile-time invariants (enforced by `wrappers::tests`)

For each wrapper type `T ∈ { FixedKey<32>, SecretBytes, SecretString, CredentialUrl }`:

```rust
static_assertions::assert_not_impl_any!(T: std::fmt::Display);
static_assertions::assert_not_impl_any!(T: serde::Serialize);
static_assertions::assert_not_impl_any!(T: serde::Deserialize<'static>);
```

For a representative sample of public HTTP response DTOs (`D ∈ { dashboard response DTO, EVM session response DTO, storage info response DTO, ... }`):

```rust
// Asserts the DTO still derives Serialize. Combined with the wrapper non-impl
// assertions above, this transitively makes "a wrapper field in a public DTO"
// a compile error: the #[derive(Serialize)] on the DTO would fail to compile
// because the wrapper does not implement Serialize. This satisfies SC-009
// without a bespoke structural-walk test.
static_assertions::assert_impl_all!(D: serde::Serialize);
```

If any of these starts to fail, the build fails — that is the intended signal.

## Runtime invariants (enforced by `wrappers::tests`)

- `format!("{:?}", x)` never contains the underlying bytes/string for any wrapper.
- `Clone` produces an independently-owned buffer (dropping the original does not zero the clone's bytes).
- `secret::ct::eq` returns the same logical result as `==` for equal-length inputs and `false` for differing-length inputs.

## Allowed crate dependencies introduced

| Crate | Version pin | Features | Where | Why |
|---|---|---|---|---|
| `secrecy` | `"0.10"` | `default-features = false` (**no `serde`**) | `crates/server` | Provides `SecretBox<T>`: redacted `Debug`, no `Display`, no `Serialize`/`Deserialize`, `expose_secret()` access, zero-on-drop. The `serde` feature MUST stay disabled to keep FR-003 a compile-time guarantee. |
| `zeroize` | `"1.7"` | `["derive"]` | `crates/server` + `crates/miden-keystore` | Required by `FixedKey<N>`'s derive. Also used by `miden-keystore` for `Zeroizing<Vec<u8>>` on the disk-read buffer. |
| `subtle` | `"2.5"` | (default) | `crates/server` | Constant-time equality primitive. Used by `secret::ct::eq` for any future byte-by-byte equality site; **not** used to replace `hmac::Mac::verify_slice`. |
| `static_assertions` | `"1.1"` | (default) | `crates/server` `[dev-dependencies]` | Compile-time non-impl checks. |

### Forbidden Cargo changes

- Enabling `secrecy/serde` (anywhere in the workspace) — would add a `Deserialize` impl on `SecretBox<T>` and silently violate FR-003. The `static_assertions::assert_not_impl_any!` block is the compile-time backstop, but the feature flag MUST also remain off.
- Switching `miden-keystore` from `zeroize::Zeroizing<Vec<u8>>` to the server's `secret::SecretBytes` — would invert the dependency direction. The server depends on the keystore, not the other way around.

## Forbidden changes (regress without an amendment to this contract)

- Implementing `Display` on any wrapper.
- Implementing `Serialize` or `Deserialize` on any wrapper, or enabling the `secrecy/serde` feature.
- Returning the raw inner value from any method other than `expose_secret()` / `scheme_and_host()`.
- Adding a `From<T>` impl that bypasses the named constructor.
- Replacing `secret::ct::eq` with `==` at a previously-migrated call site.

A reviewer encountering any of these in a PR should refuse the change or require a corresponding spec amendment.
