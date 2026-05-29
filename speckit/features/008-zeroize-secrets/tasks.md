# Tasks: Memory-Resident Secret Hygiene

**Feature**: `008-zeroize-secrets`
**Input**: Design documents from `speckit/features/008-zeroize-secrets/`
**Prerequisites**: `plan.md`, `spec.md`, `research.md`, `data-model.md`, `contracts/secret-module.md`, `quickstart.md`

**Tests**: Included where the spec explicitly requires them (`assert_not_impl_any!`, redacted-`Debug` tests, session-storage-shape tests, DTO `assert_impl_all!`, etc.). Test tasks are kept colocated with the implementation tasks that produce the asserted behavior.

**Organization**: Tasks grouped by user story so each P1 story can land as an independently-testable increment. P2 (timing) is a small follow-up.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Parallelizable — touches a different file from the other [P] tasks in the same phase, with no incomplete-task dependencies.
- **[Story]**: `[US1]`, `[US2]`, `[US3]` per spec.md.
- File paths are absolute relative to repo root.

## Path Conventions

Single-project Rust workspace. Server code under `crates/server/src/`; one cross-crate change under `crates/miden-keystore/src/`. No new top-level directories.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Bring in the chosen crate dependencies and create the module skeleton. No behavior change yet.

- [X] T001 Add server-side dependencies to `crates/server/Cargo.toml`: `secrecy = { version = "0.10", default-features = false }`, `subtle = "2.5"`, `zeroize = { version = "1.7", features = ["derive"] }`, and `static_assertions = "1.1"` under `[dev-dependencies]`. Verify the `secrecy/serde` feature is **not** enabled anywhere in the workspace.
- [X] T002 Add `zeroize = { version = "1.7" }` to `crates/miden-keystore/Cargo.toml` (or confirm it is already pulled in by the existing crypto stack; explicit direct-dep is preferred for visibility).
- [X] T003 Create the empty module skeleton at `crates/server/src/secret/mod.rs`, `crates/server/src/secret/wrappers.rs`, `crates/server/src/secret/ct.rs`, and `crates/server/src/secret/tests.rs` (empty stubs; no impls yet). Add `mod secret;` to `crates/server/src/lib.rs` so the module compiles.

**Checkpoint**: `cargo build -p guardian-server -p miden-keystore` succeeds; no behavior change.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Implement the four wrapper types, the `ct::eq` helper, and the compile-time + runtime test suite. This phase delivers the *contract* of US1 + US2 for the wrapper types themselves (no site is migrated yet — that lands in Phases 3 / 4).

⚠️ **CRITICAL**: No user-story phase may start until this phase is complete.

- [X] T004 [P] Implement `FixedKey<const N: usize>` in `crates/server/src/secret/wrappers.rs` per `contracts/secret-module.md`: inner `secrecy::SecretBox<[u8; N]>`, `new`, `expose_secret`, hand-rolled `Clone` (fresh `SecretBox` over copied array), redacted `Debug` (`FixedKey<N>(…)`), hand-rolled `PartialEq`/`Eq` via `subtle::ConstantTimeEq`. No `Display`, no `Serialize`/`Deserialize`.
- [X] T005 [P] Implement `SecretBytes` in `crates/server/src/secret/wrappers.rs` per `contracts/secret-module.md`: inner `secrecy::SecretBox<Vec<u8>>`, `new`, `expose_secret`, `len`, hand-rolled `Clone` / `Debug` (`SecretBytes(len=N)`) / `PartialEq` / `Eq`.
- [X] T006 [P] Implement `SecretString` in `crates/server/src/secret/wrappers.rs` per `contracts/secret-module.md`: inner `secrecy::SecretBox<String>`, `new`, `expose_secret`, `len`, hand-rolled `Clone` / `Debug` (`SecretString(len=N)`) / `PartialEq` / `Eq`.
- [X] T007 [P] Implement `CredentialUrl` in `crates/server/src/secret/wrappers.rs` per `contracts/secret-module.md`: inner `secrecy::SecretBox<String>`, `new`, `expose_secret`, `scheme_and_host` (returns `scheme://host[:port]` with userinfo/path/query stripped; `<invalid-url>` fallback), hand-rolled `Clone` / `Debug` (`CredentialUrl(<scheme_and_host>)`) / `PartialEq` / `Eq` via `subtle`. **Required** for `EvmChainConfig` and `EvmChainRegistry` which derive `PartialEq, Eq`.
- [X] T008 Implement `secret::ct::eq` in `crates/server/src/secret/ct.rs`: `#[allow(dead_code)] pub(crate) fn eq(a: &[u8], b: &[u8]) -> bool` using `subtle::ConstantTimeEq`, returning `bool` only as the final action. Public to the crate; zero callers in this feature (Decisions 6 + 8).
- [X] T009 Add `pub(crate)` re-exports in `crates/server/src/secret/mod.rs` (`pub(crate) use wrappers::{FixedKey, SecretBytes, SecretString, CredentialUrl}; pub(crate) use ct::eq as ct_eq;`). Add `#[cfg(test)] mod tests;`.
- [X] T010 [P] Add compile-time non-impl assertions in `crates/server/src/secret/tests.rs` using `static_assertions::assert_not_impl_any!` for each of `FixedKey<32>`, `SecretBytes`, `SecretString`, `CredentialUrl`: `Display`, `serde::Serialize`, `serde::Deserialize<'static>`. (Satisfies SC-002, SC-003.)
- [X] T011 [P] Add runtime tests in `crates/server/src/secret/tests.rs`: `debug_redacts` (each wrapper's `{:?}` output contains only the redaction marker), `clone_independent` (constructed-cloned-dropped-original, clone still readable), `eq_uses_constant_time` (correctness of `==` on equal and differing contents), `ct_eq_distinguishes` (correctness of `secret::ct::eq` — also keeps it out of dead-code reach), `credential_url_scheme_and_host_safe` (for `postgres://user:pass@host:5432/db` and `https://api.example.com/?key=abc`, `scheme_and_host()` returns scheme+host+port only). (Satisfies SC-005.)

**Checkpoint**: `cargo test -p guardian-server --lib secret` passes. Wrapper types are usable from any module inside `guardian-server`.

---

## Phase 3: User Story 1 — Eliminate Accidental Disclosure (Priority: P1) 🎯 MVP

**Goal**: Migrate the in-scope inventory sites where the dominant threat is accidental log/serde/panic disclosure. After this phase, the wrapper *capability* delivered in Phase 2 is applied at every in-scope production site, and the SC-009 transitive DTO guard is in place.

**Independent Test**: Wrapper types do not implement `Display` or `Serialize` (compile-time, from Phase 2). Each in-scope field that previously held a raw secret is now a wrapper. `format!("{:?}", server_state)` (where available) renders secrets as redacted markers. `#[derive(Serialize)]` on any public DTO that holds a wrapper fails to compile.

### Implementation for User Story 1

- [X] T012 [P] [US1] Migrate `CursorSecret` to wrap `FixedKey<32>` in `crates/server/src/dashboard/cursor.rs`. Replace the existing newtype-around-`[u8; 32]` shape with a struct holding `FixedKey<32>`. **Remove** the manual `Debug` redaction at `cursor.rs:240-246` (now redundant — the wrapper's `Debug` does the redaction). HMAC verify at `cursor.rs:303` continues to call `hmac::Mac::verify_slice`; the call must now pass `cursor_secret.expose_secret()` to `Hmac::<Sha256>::new_from_slice`.
- [X] T013 [P] [US1] In `crates/server/src/dashboard/config.rs:47`, fold the `std::env::var("GUARDIAN_DASHBOARD_CURSOR_SECRET")` read, the hex decode, and the `FixedKey::<32>::new(...)` constructor into a single expression (FR-012) — no intermediate `String` or `[u8; 32]` local between the read and the wrapper.
- [X] T014 [P] [US1] Migrate `EvmChainConfig.rpc_url` to `CredentialUrl` in `crates/server/src/evm/config.rs:9`. In the env-var parser at `evm/config.rs:77`, fold `std::env::var(RPC_URLS_ENV)` and `CredentialUrl::new(...)` into a single expression per FR-012. Adjust the `EvmChainConfig` struct's `#[derive(... PartialEq, Eq)]` — it must still compile because `CredentialUrl` now implements those traits (T007). `EvmChainRegistry`'s `PartialEq, Eq, Default` derives at line 16 also continue to compile.
- [X] T015 [US1] Migrate `StorageMetadataBuilder.database_url` to `Option<CredentialUrl>` in `crates/server/src/builder/storage.rs:32`. Rewrite the env-var read at `storage.rs:79` to fold `std::env::var("DATABASE_URL")` and `CredentialUrl::new(...)` into a single expression (FR-012); the `unwrap_or_default()` chain is restructured so the env-var `String` is consumed by `CredentialUrl::new` in the same expression. Update downstream call sites that pass the URL to pool construction to call `.expose_secret()`.
- [X] T016 [P] [US1] Update `crates/server/src/audit/postgres.rs:264` to read `DATABASE_URL` and construct `CredentialUrl` in a single expression (FR-012). Pool construction reads `.expose_secret()` at the connection-pool builder boundary.
- [X] T017 [P] [US1] Replace any startup logging that printed the full `DATABASE_URL` or any RPC URL with calls to `url.scheme_and_host()`. Search `crates/server/src` for `tracing::info!(.*database_url|.*rpc_url|.*GUARDIAN_EVM_RPC|DATABASE_URL=)`. Each hit logs the safe view instead. (Supports SC-001, SC-002.)
- [X] T018 [US1] Add the SC-009 transitive DTO guard. Identify a representative sample of public HTTP response DTOs reachable from the server's handlers (e.g. dashboard list/detail response, EVM session-create response, storage info response — sample chosen to span the modules that hold wrappers). Add `static_assertions::assert_impl_all!(<DtoName>: serde::Serialize)` for each, placed either in `crates/server/src/secret/tests.rs` or in a sibling `tests` module of each DTO. Combined with T010, this makes "wrapper field in a public DTO" a compile error.
- [X] T019 [US1] Update tests in `crates/server/src/dashboard/cursor.rs`, `crates/server/src/evm/config.rs`, `crates/server/src/builder/storage.rs` that previously constructed fields with plain `String` / `[u8; 32]` to now construct via the wrapper types. Existing assertion semantics (e.g. equality of two `EvmChainConfig` instances) continue to hold because the wrappers implement `PartialEq` (T004–T007).

**Checkpoint**: `cargo test -p guardian-server` passes. Every in-scope disclosure-threat field is wrapped. Public DTO Serialize derives still compile (they hold no wrapper fields).

---

## Phase 4: User Story 2 — Erase Secrets from Process Memory Promptly (Priority: P1)

**Goal**: Migrate the sites whose dominant threat is residual memory exposure (coredump, swap, post-free heap). Includes the **session-storage shape change** (Decision 6 — token is digest-keyed, never retained), the AWS Secrets Manager transient wrap, and the keystore disk-read buffer wrap (FR-011).

**Independent Test**: After this phase, session expiry / eviction leaves no plaintext token bytes in the map's heap representation. The keystore's disk-read buffer zeroes on drop (`Zeroizing<Vec<u8>>`). The AWS Secrets Manager fetch wraps its transient hex/bytes locals.

### Implementation for User Story 2

- [X] T020 [US2] Restructure operator session storage in `crates/server/src/dashboard/state.rs:28-32`: change the map type to `HashMap<[u8; 32], OperatorSessionRecord>` keyed by `sha256(token)` (use the existing `sha2` workspace dep). Update the cookie-issue handler at `state.rs` (around the `Set-Cookie` write, current line ~259-260) so the plaintext token is generated, used to build the `Set-Cookie` header and the response payload, and goes out of scope at end-of-handler — it is **not** inserted as a map key. Update lookup at `state.rs:296` to compute `sha256(candidate)` and use standard `HashMap::get` on the digest. Note: a plain `String::drop` does not zeroize — the token is request-scoped and out of strict zeroization scope per spec; optionally wrap the intermediate plaintext in `SecretString` for the handler's duration as a tightening.
- [X] T021 [P] [US2] Restructure EVM session storage in `crates/server/src/evm/session.rs:17-20` and `evm/session.rs:134-145` (and the lookup at `evm/session.rs:161`) to follow the same digest-keyed pattern as T020. `EvmSessionState.sessions` becomes `HashMap<[u8; 32], EvmSessionRecord>`; the plaintext token is request-scoped.
- [X] T022 [P] [US2] Wrap the AWS Secrets Manager transient values in `crates/server/src/ack/secrets_manager.rs` (`secret_string` and `parsed_secret_key`, around lines 42-78). The `secret_hex: String` result of the cloud fetch becomes `SecretString`; the `secret_bytes: Vec<u8>` decoded via `hex::decode` becomes `SecretBytes`. The parser closure receives `secret_bytes.expose_secret()` (`&[u8]`). **No public-return-type change** — `parsed_secret_key` still returns the parsed `FalconSecretKey` / `EcdsaSecretKey` as before. Both wrappers go out of scope at function return and zeroize. Mark this site as the explicit Out-of-Scope exception (stack-local but full-key-bearing) in a brief code comment.
- [X] T023 [P] [US2] Wrap the disk-read buffer in `crates/miden-keystore/src/keystore.rs:107` with `zeroize::Zeroizing<Vec<u8>>` — **not** the server's `SecretBytes` (dep direction would invert). The wrap is immediate (`let key_bytes = Zeroizing::new(std::fs::read(path)?);`), and `SecretKey::read_from_bytes` is called with `&*key_bytes` (the `Deref` view). Add a code comment recording the FR-011 verification result: `// Zeroization: upstream miden-protocol::SecretKey {verified at <cite> | unverified — local Zeroizing<Vec<u8>> handles file-read buffer}`.
- [X] T024 [P] [US2] Same wrap pattern as T023 for the disk-read buffer at `crates/miden-keystore/src/ecdsa_keystore.rs:103`. Same FR-011 code-comment requirement.
- [X] T025 [US2] Add tests in `crates/server/src/dashboard/state.rs` (test module) and `crates/server/src/evm/session.rs` (test module): `session_lookup_by_digest` (insert with token, look up by same token returns record; mismatched token returns `None`) and `session_token_not_retained` (after insertion, the map's keys have length 32 and are not the issued token's bytes).
- [X] T026 [US2] Phase-2 verification step for FR-011: inspect the upstream `miden-protocol::SecretKey` (Falcon and ECDSA) source and record one of the two outcomes in code comments at `crates/server/src/ack/miden_falcon_rpo/signer.rs` and `crates/server/src/ack/miden_ecdsa/signer.rs` (next to the `self.keystore.sign(...)` / `ecdsa_sign(...)` calls): (a) upstream `SecretKey` is `Zeroize` / `ZeroizeOnDrop` with a citation, or (b) unverified — local `Zeroizing<Vec<u8>>` from T023/T024 handles the file-read buffer; file follow-on issue against upstream. (Records SC-008.)

**Checkpoint**: `cargo test -p guardian-server -p miden-keystore` passes. Session maps no longer contain plaintext tokens. Keystore disk-read buffers zeroize on drop. AWS Secrets Manager transients are wrapped.

---

## Phase 5: User Story 3 — Resist Timing Side-Channels (Priority: P2)

**Goal**: Document the constant-time posture of byte-by-byte equality sites against untrusted input. In practice: cite `hmac::Mac::verify_slice` at the cursor-HMAC site, confirm `ct::eq` is in place for future sites, and document the deliberate non-coverage of `evm/session.rs:113`.

**Independent Test**: A grep + code-review pass over the in-scope inventory confirms each byte-equality site against untrusted input routes through a canonical constant-time primitive (`verify_slice`) or `secret::ct::eq`. The `evm/session.rs:113` `eq_ignore_ascii_case` site is documented in the spec's Out-of-Scope (already in place).

### Implementation for User Story 3

- [X] T027 [US3] Add a one-line code comment at `crates/server/src/dashboard/cursor.rs:303` citing the constant-time property of `hmac::Mac::verify_slice` per RustCrypto's `hmac` crate documentation, with a back-reference to `speckit/features/008-zeroize-secrets/research.md` Decision 8.
- [X] T028 [US3] Confirm the `ct_eq_distinguishes` test from T011 is part of the standing test suite and exercises `secret::ct::eq` with at least one equal-input case and one differing-input case at varied positions. This both validates the helper and prevents the `#[allow(dead_code)]` from masking bit-rot.

**Checkpoint**: All FR-004 sites are accounted for (citation, helper, or documented Out-of-Scope). No code change other than comments and a confirmed-existing test.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: FR-007 / FR-012 manual-review guard, `CONTRIBUTING.md` updates, end-to-end validation, smoke test.

- [~] T029 **Removed.** Reserved for the deleted automated review helper; FR-007 is satisfied by the reviewer checklist (T032) plus the `quickstart.md` audit recipe.
- [~] T030 **Removed.** Reserved for the deleted automated review helper.
- [~] T031 **Removed.** Reserved for the deleted automated review helper.
- [X] T032 [P] Update `CONTRIBUTING.md` with: (a) a one-bullet pointer to the `secret` module and the wrapper-choice TL;DR from `quickstart.md`; (b) a reviewer-checklist bullet for "new secret-bearing fields are wrapped in `secret::*` types, and new env-var reads use the single-expression construct-and-wrap pattern".
- [~] T033 **Removed.** The reviewer checklist lives in `CONTRIBUTING.md`.
- [X] T034 [P] Update `docs/CONFIGURATION.md` to document the new env-var handling expectations: every secret-bearing env var is wrapped at the point of read; the OS env block is explicitly Out-of-Scope; production should prefer the AWS Secrets Manager fetch path (already in use for Falcon/ECDSA keys) for the highest-sensitivity material.
- [X] T035 Run the full validation matrix:
  - `cargo test -p guardian-server --all-features` — all tests pass.
  - `cargo test -p miden-keystore` — all tests pass.
  - `cargo clippy -p guardian-server -p miden-keystore -- -D warnings` — clean (verifies the `#[allow(dead_code)]` on `secret::ct::eq` is needed and present).
- [X] T036 Run the operator-dashboard manual smoke harness via the `smoke-test-operator-dashboard` skill: confirm operator challenge → login → list accounts → detail → logout still works end-to-end against the workspace operator client with the digest-keyed session store from T020 / T021.
- [X] T037 Run the full audit recipe from `quickstart.md` (steps 1–7): enumerate `expose_secret(` call sites; confirm zero `Display` impls or `Serialize` derives on wrappers; enumerate `ct_eq` sites; confirm env-var reads are single-expression; confirm no unauthorized `env::remove_var` / `env::set_var`. Record the result counts so future audits have a baseline.

**Checkpoint**: CI green; smoke test green; audit recipe baseline recorded. Feature ready to merge.

---

## Dependencies

```text
Phase 1 (Setup) ─┐
                 ├─► Phase 2 (Foundational) ─┐
                                              ├─► Phase 3 (US1) ─┐
                                              │                  ├─► Phase 6 (Polish)
                                              ├─► Phase 4 (US2) ─┤
                                              │                  │
                                              └─► Phase 5 (US3) ─┘
```

- **Phase 1 → Phase 2**: deps and module skeleton must exist before wrapper impls.
- **Phase 2 → Phases 3 / 4 / 5**: wrapper types and their tests must exist before any site migration.
- **Phases 3 + 4 are independent of each other** and can be implemented in either order or in parallel by two developers, because they touch disjoint files (URLs/cursor/AWS-hex vs sessions/keystore/AWS-bytes — only T022 touches both flows; sequence T022 after either Phase 3 completion or before T020).
- **Phase 5 (US3)** has no implementation dependency on US1/US2 — it can land any time after Phase 2.
- **Phase 6** depends on Phases 3, 4, 5 being complete (the reviewer audit recipe is checked against the post-migration tree; the smoke test exercises the migrated session-store shape).

### Within-phase parallelization

**Phase 2 (Foundational)**: T004, T005, T006, T007 are marked `[P]` — all four wrapper types touch the same file (`wrappers.rs`), so they are conceptually parallel work-items but must be merged in one commit per file. T008, T010, T011 are independent files.

**Phase 3 (US1)**: T012, T013, T014 touch different files and are fully `[P]`. T015 + T016 touch related code paths (storage / postgres audit) and should be sequenced to keep the `Option<CredentialUrl>` type change consistent across them.

**Phase 4 (US2)**: T020 (dashboard sessions) and T021 (EVM sessions) are `[P]` (different files). T022 (AWS secrets manager), T023 (keystore.rs), T024 (ecdsa_keystore.rs) are all `[P]` against the session work.

**Phase 6 (Polish)**: T029–T034 are all `[P]` (different files, no overlap). T035–T037 are sequenced at the end.

---

## Implementation Strategy

### MVP scope (User Story 1 only)

Ship **Phase 1 + Phase 2 + Phase 3** as the MVP. This delivers:
- The wrapper API with all compile-time and runtime guarantees in place (Phase 2).
- Every in-scope disclosure-threat field (cursor HMAC secret, two URL types) migrated (Phase 3).
- The SC-009 transitive DTO guard (Phase 3, T018).

At this point the spec's US1 ("eliminate accidental disclosure") is satisfied end-to-end. US2's zeroize-on-drop benefit is **also** delivered for those same fields because the wrappers zeroize on drop — Phase 3 incidentally covers a large chunk of US2's surface. What remains for US2 is the session-store restructure + keystore + AWS transients, which is the heart of Phase 4.

### Incremental delivery

After MVP, deliver in this order:
1. **Phase 4 (US2)** completes the erasure story for session tokens, keystore buffers, and cloud-fetched key material. Highest residual security value after MVP.
2. **Phase 5 (US3)** is small (two tasks) and can ride along with Phase 4 or the polish phase.
3. **Phase 6** lands the documentation, reviewer checklist, and audit recipe that prevent future regressions and complete the validation matrix.

Each phase is independently shippable behind a single merge commit. There is no feature flag — the changes are type-system-level and either compile or do not.
