# Phase 0 Research: Memory-Resident Secret Hygiene

**Feature**: `008-zeroize-secrets`
**Date**: 2026-05-29

This document resolves the NEEDS-CLARIFICATION items from `plan.md`'s Technical Context and records the Phase 0 decisions that drive the Phase 1 design.

---

## Decision 1 — Crate selection and exact version pins

**Decision**: Pin the following exact version lines in `Cargo.toml`:

```toml
secrecy        = { version = "0.10", default-features = false }   # NO serde feature
subtle         = "2.5"
zeroize        = { version = "1.7", features = ["derive"] }
static_assertions = "1.1"   # dev-dependency, in [dev-dependencies]
```

**Rationale**:
- `secrecy` 0.10 is the current line on docs.rs (0.10.3 at time of writing); the 0.8 → 0.10 API consolidated around `SecretBox<S: Zeroize>` and removed the older `SecretString` / `SecretVec` aliases in favor of explicit `SecretBox<String>` / `SecretBox<Vec<u8>>`. Our four wrapper newtypes (`FixedKey<N>`, `SecretBytes`, `SecretString`, `CredentialUrl`) wrap `SecretBox<_>` so the API shape is stable to our callers regardless of upstream renames.
- `default-features = false` is critical: `secrecy`'s `serde` feature, when enabled, derives `Deserialize` on `SecretBox<T>` for any `T: Deserialize`. That would violate FR-003 by adding a deserialization path. The exact-pin combined with `default-features = false` makes "no Serialize / no Deserialize on a wrapper" hold without further work; the `static_assertions::assert_not_impl_any!` block (SC-002 / SC-003) is the **backstop** that converts any future regression — including someone enabling the `serde` feature — into a compile error.
- `secrecy` 0.10 provides:
  - No `Display` impl on `SecretBox<T>` — satisfies FR-002.
  - No `Serialize` / `Deserialize` impl unless `serde` feature is enabled — we leave it off; satisfies FR-003.
  - `Debug` redacted to `SecretBox<…>` — satisfies FR-002.
  - `expose_secret()` accessor — satisfies SC-006.
  - `Drop` zeroizes via the inner `T: Zeroize` — satisfies FR-001.
- `subtle` 2.5 — `ConstantTimeEq` returns a `Choice`, which avoids accidental short-circuit branching. Already in the workspace's transitive graph.
- `zeroize` 1.7 with `features = ["derive"]` — needed for `#[derive(Zeroize, ZeroizeOnDrop)]` on `FixedKey<N>`'s `[u8; N]` and for `Zeroizing<Vec<u8>>` in the `miden-keystore` change.
- `static_assertions` 1.1 (dev-dep) — compile-time `assert_not_impl_any!`.

**Alternatives considered**:
- **Allow the `secrecy/serde` feature**: rejected. Even opt-in, enabling a workspace-wide feature is a poor regression boundary (any other crate in the workspace using `secrecy` would inherit the impl). Wrapper bytes are reachable through `expose_secret()` and explicitly serialized at the call site if ever needed.
- **`secrecy` 0.8**: older API; using the current 0.10 line aligns with docs.rs and avoids a future migration.
- **Hand-rolled wrapper**: rejected as in the previous round.
- **`constant_time_eq` instead of `subtle`**: rejected (`subtle` is more misuse-resistant).

---

## Decision 2 — FR-011 keystore-boundary verification (dep-direction safe)

**Decision**: The Falcon and ECDSA signing-key bytes live inside `miden-protocol::crypto::dsa::*::SecretKey`, which is an **external** crate. We will:

1. Inspect the upstream `miden-protocol` `SecretKey` types in Phase 2 (during implementation) and record one of two outcomes in a `// Zeroization: upstream` comment at each load site:
   - **(a) Confirmed upstream**: cite the upstream source line that shows `SecretKey` implements `Zeroize` / `ZeroizeOnDrop` (or has an explicit `Drop` that zeros). No local adapter needed.
   - **(b) Not confirmed**: file a follow-on upstream issue. The local file-read buffer wrap (step 3) still bounds exposure to that one buffer, and the existing per-call load-and-drop pattern bounds the `SecretKey` itself to the signing-call frame.

   **Implementation outcome (recorded 2026-05-29)**: branch **(a)** for both signing schemes. The pinned `miden-protocol 0.14.5` re-exports types from `miden-crypto 0.23.0`. Verified by direct source inspection of the crate cached at `~/.cargo/registry/src/index.crates.io-*/miden-crypto-0.23.0/`:
   - **Falcon** — `src/dsa/falcon512_poseidon2/keys/secret_key.rs` declares `impl ZeroizeOnDrop for SecretKey {}` (around line 80) with a matching `impl Drop for SecretKey { fn drop(&mut self) { self.zeroize(); } }`. The earlier session-memory observation that Falcon "historically did not zeroize" predates this revision; the pinned crate does implement it.
   - **ECDSA** — `src/dsa/ecdsa_k256_keccak/mod.rs` declares `impl ZeroizeOnDrop for SecretKey {}` (around line 119), delegating to the inner `k256::ecdsa::SigningKey`'s `ZeroizeOnDrop`.

   The signer call sites (`crates/server/src/ack/miden_falcon_rpo/signer.rs:sign_with_server_key` and `crates/server/src/ack/miden_ecdsa/signer.rs:sign_with_server_key`) carry citation comments referencing these locations. If `miden-protocol` is later bumped to a version that pulls in a different `miden-crypto` line, the citations must be re-verified.
2. The current keystore code (verified in this research phase) already drops the loaded `SecretKey` at end-of-scope in `sign()` / `ecdsa_sign()` (`crates/miden-keystore/src/keystore.rs:145–150` and `ecdsa_keystore.rs:133–138`). No persistent in-memory cache holds the loaded key between calls. This bounds the exposure window to the duration of one signing call.
3. Wrap the on-disk `key_bytes: Vec<u8>` read in `read_key_file` (`keystore.rs:107`, `ecdsa_keystore.rs:103`) in **`zeroize::Zeroizing<Vec<u8>>`** — **not** the server-side `SecretBytes` wrapper. Rationale: `crates/miden-keystore` is a lower-layer crate that must not depend on the server's private `secret` module (the dependency would invert the layering). `Zeroizing<Vec<u8>>` is a thin `Deref`-based zero-on-drop wrapper from the `zeroize` crate, which `miden-keystore` already pulls in transitively via the crypto stack. It gives us FR-001 (zero on drop) at the boundary; we do not need the no-`Display`/no-`Serialize` posture for a stack-local file-read buffer that is never logged or serialized.

**Rationale**:
- The actual byte storage is in an upstream crate we do not control day-to-day. The constitution does not let us promise behavior we cannot enforce, so the plan records the verification step as a Phase 2 task and provides a fallback (the adapter) that does not depend on upstream changes.
- The disk-read buffer is local; wrapping it is cheap and covers FR-001 without depending on upstream.
- Keeping the existing per-call load-and-drop pattern is already favorable — no persistent in-memory copy of the key bytes outside the signing call.

**Alternatives considered**:
- **Upstream patch to `miden-protocol`**: rejected for this feature. Out of scope, separately owned, and would gate this work on an external release. If verification falls into the "(b) not confirmed" branch, the upstream patch becomes a follow-on issue, not a blocker.
- **Skip the disk-read buffer wrap**: rejected. Cheap to do, closes a real gap independent of upstream.

---

## Decision 3 — Compile-time non-impl assertions: `static_assertions::assert_not_impl_any!`

**Decision**: Use `static_assertions::assert_not_impl_any!` for SC-002 / SC-003. Each wrapper type ships a `tests` module with:
```rust
static_assertions::assert_not_impl_any!(FixedKey<32>: fmt::Display, serde::Serialize, serde::Deserialize);
static_assertions::assert_not_impl_any!(SecretString: fmt::Display, serde::Serialize, serde::Deserialize);
// etc.
```
These are compile-time assertions — adding a `Display` or `Serialize` impl later will fail `cargo build`.

**Rationale**:
- `static_assertions` is small, widely used (`>50M` downloads), and produces a clear compile error pointing at the assertion line.
- It runs at every build (including CI), not only when tests are explicitly invoked.

**Alternatives considered**:
- **`trybuild` compile-fail tests**: rejected as primary mechanism. `trybuild` is excellent for proving a *specific user error* fails to compile, but it requires committing the failing-snippet files and running them in a special harness. `assert_not_impl_any!` is lighter and tests the type directly. We may still use a single `trybuild` test for the "user derives `Serialize` on an enclosing struct that contains a wrapper → compile error" scenario, since that one is about user code shape rather than the wrapper itself.
- **`#[deny(...)]` lints**: no built-in lint covers "this type must not implement this trait".

---

## Decision 4 — `Debug` redaction pattern for enclosing structs

**Decision**: `secrecy`'s `SecretBox<T>` already redacts itself in `Debug`. For enclosing structs that already derive `Debug` (e.g. anything that the `tracing` macros render with `?value`), no change is required — the field renders as `SecretBox<…>` automatically because `SecretBox<T>: Debug` regardless of `T`. For enclosing structs that hand-implement `Debug` (rare in this codebase), the implementer routes the secret field through `?` or skips it; either is acceptable because the wrapper's own `Debug` impl is the binding contract.

**Rationale**:
- Matches the spec's relaxed acceptance scenario 2: enclosing structs are not required to gain a `Debug` impl they don't already have, and where they do have one they inherit redaction through the wrapper.
- No bespoke macro or proc-macro derive needed in the server crate.

**Alternatives considered**:
- **`#[derive(Zeroize, ZeroizeOnDrop, Debug)]` with a custom `Debug` per wrapper**: redundant; `SecretBox<T>` already does this.

---

## Decision 5 — FR-007 guard mechanism: manual review checklist + documented audit recipe

**Decision**: Implement FR-007 via the spec's **manual-review arm**:

1. **Reviewer checklist**: a bullet in `CONTRIBUTING.md` asking reviewers to confirm "new secret-bearing fields are wrapped in `secret::*` types, and new env-var reads use the single-expression construct-and-wrap pattern".
2. **Documented audit recipe**: the grep recipe in `quickstart.md` lets any reviewer enumerate, in ~30 seconds, every `expose_secret(` site, confirm no `Display`/`Serialize` on a wrapper, and check that the three secret env vars wrap in a single expression (FR-012 / SC-010).

**Rationale**:
- The **hard** enforcement already lives in the compile-time `static_assertions::assert_not_impl_any!` (no `Display`/`Serialize` on wrappers) and `assert_impl_all!` (response DTOs keep `Serialize`) checks — these make the dangerous regressions (logging, serializing, or DTO-embedding a secret) a build failure.
- FR-007 is satisfied by the manual checklist plus the compile-time assertions. The spec-forbidden reflection path is still ruled out.

**Alternatives considered**:
- **Custom clippy lint**: rejected for this feature; too large a step for the value (this is a finite inventory).
- **`cargo deny` rule**: not the right tool — `cargo deny` checks dependencies, not field types.

---

## Decision 6 — Session-token storage: key by `sha256(token)`, do not retain the plaintext

**Decision**: Restructure the operator and EVM session maps to key by a non-secret 32-byte digest of the token. Specifically:

```rust
type SessionKey = [u8; 32]; // sha256(token)
type DashboardSessions = HashMap<SessionKey, OperatorSessionRecord>;
```

The plaintext token is generated, written to the `Set-Cookie` header for the response, and **dropped** — never stored as a map key, never retained in a record field. On subsequent requests, the server computes `sha256(candidate)` and uses standard `HashMap::get`.

**Rationale**:
- Resolves the dep-FR-001 conflict the previous design carried. Leaving the token as the `HashMap<String, _>` key would leave it un-zeroized in heap memory — the spec's FR-001 covers any long-lived secret in memory, including map keys. The previous "wrap only the value side" plan did not actually wrap the token because the token *is* the key, not a field of the value.
- A 32-byte digest is not a secret: given the 256-bit-entropy random token, the digest reveals nothing about the token that the token's entropy doesn't already shield. The digest cannot be used to authenticate without inverting SHA-256.
- No constant-time compare is needed: `HashMap::get` is a structural lookup over the digest, not a byte-by-byte equality against a stored secret. The digest equality check happens on a value derived from a hashed transform of the candidate; the timing oracle that motivates constant-time compare does not apply.
- Side benefit: a coredump or heap inspection of the long-lived session map captures only digests, not tokens. The plaintext token is still constructed by the cookie-issue handler — it is `format!`-embedded into the `Set-Cookie` header string and included in the `IssuedOperatorSession` / `VerifiedEvmSession` response payload — so it exists in memory for the duration of that request handler (request-scoped, not "hundreds of nanoseconds"). That request-scoped lifetime is out of strict zeroization scope per the spec's general Out-of-Scope. The point of the digest-keyed map is to ensure the token does not persist *beyond* the issuing request.

**Implementation notes**:
- `sha256(token)` uses the existing `sha2` dep (already in the workspace via the crypto stack).
- The plaintext token does not need to be a `SecretString` because its lifetime is now strictly local to one request handler (generation → digest → cookie write → drop). If the implementation discovers a code path that retains the plaintext (e.g. for refresh tokens), that path is wrapped in `SecretString` at that time.
- Test fixtures that previously held the plaintext token to assert insertion success now insert via the public constructor and look up via the same constructor — the digest is an implementation detail of the store.

**Alternatives considered**:
- **Wrap only the value side (previous design)**: rejected. Does not address FR-001 because the token *is* the key. Caught by second-round review.
- **`HashMap<SecretString, _>` (wrap the map key with `Hash` + `Eq` derives)**: rejected. Would require implementing `Hash` and `Eq` on a wrapper whose whole point is to be opaque, defeating the no-`Display`/no-`Serialize`/no-equality posture and creating surface for accidental leak.
- **`FixedKey<32>` for tokens (wrap raw bytes)**: rejected. Tokens are currently hex strings; converting the cookie API to raw bytes would expand the change without value.
- **Amend the spec to exclude session-token map keys from FR-001**: rejected as a posture regression. The token is a clear long-lived secret in memory; keeping FR-001 honest is the better answer.

---

## Decision 7 — Wrapper-type taxonomy: four named types

**Decision**: The `secret` module exports four named types:

| Type | Purpose | Mapped sites |
|---|---|---|
| `FixedKey<N>` | Fixed-size symmetric key material (HMAC keys, etc.). Wraps `[u8; N]`. | `CursorSecret` (N=32). |
| `SecretBytes` | Variable-length opaque bytes. Wraps `Vec<u8>`. | AWS Secrets Manager fetched bytes; the disk-read buffer in `miden-keystore::read_key_file`. |
| `SecretString` | Variable-length secret string. Wraps `String`. | Operator session token; EVM session token. |
| `CredentialUrl` | A URL that may embed credentials in userinfo or query. Wraps `String`. | `EvmChainConfig.rpc_url`; `StorageMetadataBuilder.database_url`. |

All four are built on `secrecy::SecretBox<T>` underneath (or `Secret<T>` for `Copy` types) so they share the no-`Display`, no-`Serialize`, redacted-`Debug`, zero-on-drop, `expose_secret()` contract.

**Rationale**:
- Distinct names give grep/audit precision ("how many `FixedKey<32>` instances exist? where are `CredentialUrl`s built?") which a single `SecretBox<T>` everywhere would not.
- Keeps the spec's "Distinct variants may exist for fixed-size key / variable-length token / credential URL" alive in the type system.
- Four is enough to cover the inventory without proliferation.

**Alternatives considered**:
- **One generic wrapper `Secret<T>`**: rejected. Loses the search-ergonomic distinction between credential URLs (which look like config strings) and session tokens (which look like opaque IDs).
- **A separate `SecretJsonValue` for AWS Secrets Manager**: rejected for now. The returned bytes are short-lived and converted to a `SecretKey` (Falcon/ECDSA) at the call site; `SecretBytes` is sufficient.

---

## Decision 8 — Cursor HMAC verification: keep `hmac::Mac::verify_slice`

**Decision**: The cursor HMAC verify at `crates/server/src/dashboard/cursor.rs:303` uses `mac.verify_slice(tag)` from the `hmac` crate. **No change**. We do **not** replace it with `secret::ct::eq`.

**Rationale**:
- The RustCrypto `hmac` crate documents `Mac::verify_slice` as performing the comparison in constant time. It is the canonical and audited primitive for this exact check.
- Replacing a vetted MAC verify with a home-grown byte equality call would weaken the API contract (we'd be inferring intent rather than calling the named verification API) and adds risk for no benefit. The reviewer is right to flag this.
- The migration step for `CursorSecret` is limited to (a) re-typing the field as `FixedKey<32>`, (b) routing the `new_from_slice(secret)` call through `secret.expose_secret()`, and (c) removing the now-redundant manual `Debug` redaction at `cursor.rs:240-246`.

**Code-comment addition (Phase 2)**: at the `verify_slice` call site, add a one-line comment citing the constant-time property to make the audit trail explicit:
```rust
// HMAC tag verification: hmac::Mac::verify_slice is documented constant-time
// (RustCrypto hmac crate). See [feature 008 research.md decision 8].
mac.verify_slice(tag).map_err(|_| ...)
```

**Alternatives considered**:
- **Replace `verify_slice` with `secret::ct::eq` over the computed tag bytes**: rejected. Throws away an audited API for a home-rolled one. The `secret::ct::eq` helper still exists for the cases (current or future) where a raw byte-equality compare against untrusted input is needed *outside* a MAC-verification API.

---

## Open follow-ups (deferred to implementation / future features)

- **TLS server private key**: not in scope; gated on the server adding in-process TLS termination.
- **`miden-protocol::SecretKey` upstream zeroization**: if Decision 2 lands in branch (b), file a follow-on issue against `miden-protocol` asking for `ZeroizeOnDrop` on `SecretKey`. Not a blocker for this feature.
- **Possible cleanup**: `CursorSecret` already has a manual `Debug` redaction (cursor.rs:240-246). After migration, the manual impl is removed in favor of the wrapper's built-in redaction — net code reduction.
