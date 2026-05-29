# Phase 1 Data Model: Memory-Resident Secret Hygiene

**Feature**: `008-zeroize-secrets`
**Date**: 2026-05-29

This document describes the type entities introduced by this feature and maps the in-scope inventory from `spec.md` to a concrete wrapper variant per site. It also defines the migration order.

> The wrappers do **not** model new persisted entities. This is a Rust type-system refactor, not a schema change. "Data model" here means the *type-level* shape of the wrappers and how existing fields are re-typed.

---

## Wrapper Types

All four wrapper types live in `crates/server/src/secret/wrappers.rs` and re-export through `crates/server/src/secret/mod.rs`. They share a common contract enforced by both their `secrecy`-based implementation and a compile-time assertion suite.

> **Implementation note for all four wrappers (Clone, PartialEq, Eq)**: `secrecy::SecretBox<T>` is **not** `Clone` — `#[derive(Clone)]` will not work. Every wrapper's `Clone` impl is **hand-rolled**, routing through `expose_secret()` and constructing a fresh `SecretBox` over an independently-allocated buffer. Similarly, `SecretBox<T>` is **not** `PartialEq` — but several enclosing structs that hold a wrapper (`EvmChainConfig` and `EvmChainRegistry` at `evm/config.rs:9, 16`, plus test fixtures) currently derive `PartialEq, Eq`. Every wrapper therefore implements `PartialEq` + `Eq` **using `subtle::ConstantTimeEq` internally**, so any `==` on a wrapper is automatically constant-time. This is defense-in-depth — wrapper equality is rarely against untrusted input in practice (mostly config-struct equality in tests), but making `==` constant-time eliminates a future foot-gun and lets enclosing structs keep their `PartialEq, Eq` derives.

### `FixedKey<const N: usize>`

| Property | Value |
|---|---|
| Inner storage | `secrecy::SecretBox<[u8; N]>` |
| Constructor | `pub fn new(bytes: [u8; N]) -> Self` |
| Exposure | `pub fn expose_secret(&self) -> &[u8; N]` |
| `Drop` | Zeroizes the `[u8; N]` |
| `Debug` | Renders `FixedKey<N>(…)` |
| `Display` | **Not implemented** |
| `Serialize` / `Deserialize` | **Not implemented** |
| `Clone` | Hand-rolled; constructs a fresh `SecretBox` over a copied array |
| `PartialEq` / `Eq` | Hand-rolled via `subtle::ConstantTimeEq` over the inner bytes |

Used for fixed-size symmetric key material (HMAC keys).

### `SecretBytes`

| Property | Value |
|---|---|
| Inner storage | `secrecy::SecretBox<Vec<u8>>` |
| Constructor | `pub fn new(bytes: Vec<u8>) -> Self` |
| Exposure | `pub fn expose_secret(&self) -> &[u8]` |
| `Drop` | Zeroizes the `Vec<u8>` |
| `Debug` | Renders `SecretBytes(len=N)` |
| `Display` | **Not implemented** |
| `Serialize` / `Deserialize` | **Not implemented** |
| `Clone` | Hand-rolled; allocates a new `Vec<u8>` |
| `PartialEq` / `Eq` | Hand-rolled via `subtle::ConstantTimeEq` |

Used for variable-length opaque bytes (cloud-fetched secret material; decoded hex bytes inside the AWS Secrets Manager flow).

### `SecretString`

| Property | Value |
|---|---|
| Inner storage | `secrecy::SecretBox<String>` |
| Constructor | `pub fn new(s: String) -> Self` |
| Exposure | `pub fn expose_secret(&self) -> &str` |
| `Drop` | Zeroizes the `String`'s buffer |
| `Debug` | Renders `SecretString(len=N)` |
| `Display` | **Not implemented** |
| `Serialize` / `Deserialize` | **Not implemented** |
| `Clone` | Hand-rolled; allocates a new `String` |
| `PartialEq` / `Eq` | Hand-rolled via `subtle::ConstantTimeEq` over `expose_secret().as_bytes()` |

Used for variable-length secret string values (the hex secret returned by AWS Secrets Manager before decode; any future bearer/refresh tokens).

### `CredentialUrl`

| Property | Value |
|---|---|
| Inner storage | `secrecy::SecretBox<String>` |
| Constructor | `pub fn new(url: String) -> Self` |
| Exposure | `pub fn expose_secret(&self) -> &str` |
| Additional accessor | `pub fn scheme_and_host(&self) -> String` — returns `scheme://host[:port]` with userinfo, path, and query stripped; safe to log |
| `Drop` | Zeroizes the `String`'s buffer |
| `Debug` | Renders `CredentialUrl(scheme://host[:port])` (or `CredentialUrl(…)` if parse fails) |
| `Display` | **Not implemented** |
| `Serialize` / `Deserialize` | **Not implemented** |
| `Clone` | Hand-rolled; allocates a new `String` |
| `PartialEq` / `Eq` | Hand-rolled via `subtle::ConstantTimeEq` over `expose_secret().as_bytes()` — **required**, because `EvmChainConfig` and `EvmChainRegistry` (`evm/config.rs:9, 16`) derive `PartialEq, Eq` |

Used for URLs that may embed credentials. The `scheme_and_host` accessor gives a deliberately narrow safe-to-log view without exposing userinfo or query parameters.

### `secret::ct::eq`

Not a type — a single named function:

```rust
#[allow(dead_code)]   // kept available for future byte-by-byte equality sites
pub(crate) fn eq(a: &[u8], b: &[u8]) -> bool
```

Internally uses `subtle::ConstantTimeEq` and converts the resulting `Choice` to `bool` only as the function's final action. Single audit-grepable site for any future constant-time-equality requirement.

> **Note**: as of this feature, `secret::ct::eq` has **zero callers**. The cursor HMAC verify keeps `hmac::Mac::verify_slice` (Decision 8); session-token lookup is keyed by digest (Decision 6). The function is published `pub(crate)` with `#[allow(dead_code)]` so it does not break CI under `-D warnings` and is available immediately for any future call site that needs it. Its existence is also asserted by a unit test (so the dead-code allow does not let it bit-rot silently).

---

## Site Mapping

Each in-scope site from `spec.md` is mapped to a wrapper variant.

| Site | File | Current type | Wrapper variant | Notes |
|---|---|---|---|---|
| `CursorSecret` (HMAC key) | `dashboard/cursor.rs` | newtype around `[u8; 32]` with manual `Debug` redaction | `FixedKey<32>` | Manual `Debug` impl is **removed** post-migration (replaced by wrapper's). HMAC verify path is **unchanged** — it continues to use `hmac::Mac::verify_slice` at `cursor.rs:303`, which RustCrypto documents as constant-time. A short citation comment is added at the call site. |
| AWS Secrets Manager transient key material | `ack/secrets_manager.rs` (`parsed_secret_key` and `secret_string`) | local `secret_hex: String` (full key as hex) and local `secret_bytes: Vec<u8>` (decoded raw key) — both stack-local within the fetch fn; no cache exists today | `secret_hex` → `SecretString`; `secret_bytes` → `SecretBytes` | **Explicit Out-of-Scope exception**: these are single-call stack-locals (the spec's general Out-of-Scope excludes such values), but they hold full Falcon/ECDSA private-key material in the call frame between fetch and parse. The defense-in-depth value warrants wrapping. If a cache is added in the future, it MUST use `SecretBytes` for parsed-key bytes (or `SecretString` for hex). |
| `miden-keystore::read_key_file` disk-read buffer | `crates/miden-keystore/src/keystore.rs:107`, `ecdsa_keystore.rs:103` | local `Vec<u8>` | **`zeroize::Zeroizing<Vec<u8>>`** (direct `zeroize` dep — not the server's `SecretBytes`) | The keystore crate is a lower layer and MUST NOT depend on the server's private `secret` module. `Zeroizing<T>` provides the zero-on-drop contract; the no-`Display` / no-`Serialize` posture is not needed for this stack-local buffer. Wraps immediately after the disk read; `&[u8]` view passed to `SecretKey::read_from_bytes`. |
| Session tokens (operator + EVM) | `dashboard/state.rs`, `evm/session.rs` | `HashMap<String, OperatorSessionRecord>` / `HashMap<String, EvmSessionRecord>` keyed by the token string | **Storage-shape change**: `HashMap<[u8; 32], OperatorSessionRecord>` keyed by `sha256(token)`. The plaintext token is **not retained in memory** after the Set-Cookie response is written — generation produces the token, the server inserts `(sha256(token), record)`, and returns the token to the client; subsequent requests are looked up by `sha256(candidate)`. No constant-time compare is needed (lookup is structural over the digest, and the digest is not a secret). | This is the only structural change in this feature. Rationale: leaving the token as the map key would leave it un-zeroized in heap memory, violating FR-001. Wrapping only the value side was the previous data-model design; that was rejected per second-round review. See research.md Decision 6 (revised). |
| Postgres connection URL | `builder/storage.rs` (`StorageMetadataBuilder.database_url`) and any downstream `String` field retaining it after pool build | `Option<String>` | `Option<CredentialUrl>` | After pool construction, drop the wrapper as early as possible (do not retain in `PostgresService` unless needed). |
| EVM RPC URL | `evm/config.rs` (`EvmChainConfig.rpc_url`) | `String` | `CredentialUrl` | Public-DTO check: confirmed that `EvmChainConfig` is not in any HTTP response payload. |
| Falcon signer keystore field | `ack/miden_falcon_rpo/signer.rs` (`MidenFalconRpoSigner.keystore`) | `Arc<FilesystemKeyStore<…>>` | **No change to field type** | The keystore type itself does not change; the disk-read buffer inside the keystore call is wrapped as above. FR-011 verification noted in research.md. |
| ECDSA signer keystore field | `ack/miden_ecdsa/signer.rs` (`MidenEcdsaSigner.keystore`) | `Arc<FilesystemEcdsaKeyStore<…>>` | **No change to field type** | Same as Falcon. |

### Sites explicitly OUT of scope (per spec)

- Operator `PendingChallenge.signing_digest` (returned to client, public DTO collision).
- `EvmChallenge` (storage type *is* the public response DTO in `api/evm.rs`).
- TLS server private keys (no in-process TLS termination today).
- All SDK crates (`crates/client`, `crates/miden-multisig-client`, npm packages).

---

## Migration Order

Smallest blast radius first; each step is an independent commit on the feature branch.

1. **Introduce the `secret` module** (`secret/mod.rs`, `wrappers.rs`, `ct.rs`, `tests.rs`). No callers yet. Adds compile-time assertions for the four wrapper types. Adds `secret::ct::eq` with unit tests. CI green. **Note**: `secret::ct::eq` exists for future byte-by-byte equality sites; it is **not** used by the cursor HMAC verify, which already uses the constant-time `hmac::Mac::verify_slice`.
2. **Migrate `CursorSecret`** → `FixedKey<32>`. HMAC verify path is **unchanged** — confirm `hmac::Mac::verify_slice` is still the call at `cursor.rs:303` and add a one-line citation comment. Remove the manual `Debug` redaction in `cursor.rs:240-246` (now redundant). Per FR-012, `dashboard/config.rs:47`'s `std::env::var("GUARDIAN_DASHBOARD_CURSOR_SECRET")` read folds into a single expression that decodes the hex and constructs `FixedKey::<32>::new(...)` — no intermediate `String` or `[u8; 32]` local.
3. **Migrate `EvmChainConfig.rpc_url`** → `CredentialUrl`. Per FR-012, the env-var parser at `evm/config.rs:77` constructs the wrapper in a single expression: `CredentialUrl::new(std::env::var(RPC_URLS_ENV).ok().unwrap_or_default())` (or equivalent — no intermediate `String` local). Adjust the few call sites that build the HTTP client to pass `expose_secret()`. Replace any startup logging that printed the full URL with `cfg.rpc_url.scheme_and_host()`.
4. **Migrate `StorageMetadataBuilder.database_url`** → `Option<CredentialUrl>`. Per FR-012, the env-var reads at `audit/postgres.rs:264` and `builder/storage.rs:79` are converted to single-expression construct-and-wrap; the `unwrap_or_default()` chain in `builder/storage.rs:79` is rewritten so the env-var `String` is consumed by `CredentialUrl::new` in the same expression. The `StorageMetadataBuilder.database_url` field type becomes `Option<CredentialUrl>` end-to-end.
5. **Restructure operator session storage**: change the map to `HashMap<[u8; 32], OperatorSessionRecord>` keyed by `sha256(token)`. The plaintext token is generated, used to build the `Set-Cookie` response, and goes out of scope at request end — it is **not** persisted in the session map. Lookup on subsequent requests computes `sha256(candidate)` and uses standard `HashMap::get`. Update fixtures, tests, and the cookie-issue path. **Note**: the plaintext token's `String` going out of scope at request end is a plain `String::drop` — it does **not** zeroize (that is the entire premise of the feature). The token is request-scoped and out of strict zeroization scope per the spec's general Out-of-Scope rule for request-locals. Optionally, the cookie-issue handler may wrap its intermediate token in `SecretString` to shrink the window further; this is a judgment call left to the implementer per site.
6. **Restructure EVM session storage**: same pattern as step 5 for `EvmSessionState.sessions`. Same notes about request-scoped token lifetime apply.
7. **Wrap the AWS Secrets Manager transient key material** inside `parsed_secret_key` and `secret_string` (`ack/secrets_manager.rs`): the `secret_hex: String` result of the cloud fetch becomes `SecretString`; the `secret_bytes: Vec<u8>` decoded via `hex::decode` becomes `SecretBytes`. The parser closure receives `secret_bytes.expose_secret()` (`&[u8]`) and returns the parsed `FalconSecretKey` / `EcdsaSecretKey` as before. Both wrappers go out of scope at function return and zeroize. **No return-type change** — the fn still returns the parsed key. This is an explicit Out-of-Scope exception (stack-local but full-key-bearing).
8. **Wrap the keystore disk-read buffer** in both `keystore.rs` and `ecdsa_keystore.rs` with **`zeroize::Zeroizing<Vec<u8>>`** (not the server's `SecretBytes` — dep-direction would be wrong). Record FR-011 verification result in code comments (`// Zeroization: upstream SecretKey verified at …` or `// Zeroization: upstream SecretKey unverified — local Zeroizing<Vec<u8>> handles file-read buffer`).
9. **Document the FR-007 manual-review guard**: add the reviewer checklist wording in `CONTRIBUTING.md` and keep the `quickstart.md` audit recipe current.
10. **Add the SC-009 transitive guard**: add `static_assertions::assert_impl_all!(Dto: serde::Serialize)` for a representative set of public HTTP response DTOs (e.g. the dashboard response shapes, the EVM session response, and the storage info response). Combined with the SC-002 / SC-003 non-impl assertions on the wrapper types, adding a wrapper field to any of these DTOs becomes a compile error (the `#[derive(Serialize)]` on the DTO will fail because the wrapper does not implement `Serialize`). No bespoke "structural walk" test is added — the assertion mechanism is the test.

Each step ships green (`cargo test -p guardian-server` passes) before the next is started.

---

## Test Surface

Tests added as part of this feature:

| Test | Location | Asserts |
|---|---|---|
| `assert_not_impl_any!` block (compile-time) | `secret/tests.rs` | Each of `FixedKey<32>`, `SecretBytes`, `SecretString`, `CredentialUrl` does **not** implement `Display`, `Serialize`, or `Deserialize`. |
| `debug_redacts` | `secret/tests.rs` | `format!("{:?}", x)` for each wrapper contains only the redaction marker and never the underlying bytes/string. |
| `clone_independent` | `secret/tests.rs` | Cloning a wrapper produces an independently-owned buffer (constructed-cloned-dropped-original; clone still readable). |
| `eq_uses_constant_time` | `secret/tests.rs` | For each wrapper, `==` returns true on equal contents and false on differing contents (correctness only; timing property is structural — it routes through `subtle::ConstantTimeEq` per the `PartialEq` impl). |
| `ct_eq_distinguishes` | `secret/tests.rs` | `secret::ct::eq` returns true on equal inputs and false on inputs that differ at any position. Also keeps the function out of dead-code reach so it does not bit-rot silently. (Cannot meaningfully test the *timing* property in a unit test — recorded as a code-review check in quickstart.md.) |
| `cursor_hmac_verify_unchanged` | existing `dashboard/cursor.rs` tests | The existing HMAC verify tests continue to pass after `CursorSecret` is re-typed as `FixedKey<32>`. The `verify_slice` call is unchanged; only the secret-construction path moves to the wrapper. |
| `session_lookup_by_digest` | `dashboard/state.rs` and `evm/session.rs` tests | Inserting a session with a generated token and looking it up by the same token returns the record; a mismatched token returns `None`. Internally exercises the `sha256(token)` keyed lookup. |
| `session_token_not_retained` | `dashboard/state.rs` and `evm/session.rs` tests | After session insertion, the map's keys are not the plaintext token bytes — verified by checking that the map's keys have length 32 and do not equal the issued token's bytes. |
| `response_dto_is_serialize` (SC-009 transitive guard) | `secret/tests.rs` or alongside DTO modules | `static_assertions::assert_impl_all!(Dto: serde::Serialize)` for a representative sample of public HTTP response DTOs. Combined with `assert_not_impl_any!` on wrappers (SC-002/003), this makes "wrapper field in a DTO" a compile error. |
| `credential_url_scheme_and_host_safe` | `secret/tests.rs` | For URLs of the form `postgres://user:pass@host:5432/db` and `https://api.example.com/?key=abc`, `scheme_and_host()` returns the scheme+host(+port) only. |

---

## Validation Matrix

Following `guardian-validation-matrix` discipline:

| Layer | Tests run |
|---|---|
| `crates/server` unit tests | All (the affected modules are `dashboard/cursor`, `dashboard/state`, `evm/config`, `evm/session`, `builder/storage`, `ack/*`, plus the new `secret/*` module). Run with `cargo test -p guardian-server`. |
| `crates/miden-keystore` unit tests | All. Run with `cargo test -p miden-keystore`. The disk-read buffer wrap touches these. |
| Server integration tests | Standard `cargo test --all-features` for the server crate. |
| Cross-language parity | **Not required**. This feature does not touch any wire surface or any SDK; constitution principle II is vacuous (see plan.md Constitution Check). |
| Operator smoke (manual) | Recommended once after migration: run `smoke-test-operator-dashboard` to confirm operator login → list accounts → logout still works end-to-end with the wrapped session-token store. |
