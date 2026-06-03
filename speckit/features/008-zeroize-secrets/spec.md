# Feature Specification: Memory-Resident Secret Hygiene

**Feature Branch**: `008-zeroize-secrets`
**Created**: 2026-05-29
**Status**: Draft
**Input**: User description: "Wrap every long-lived secret value held in Guardian server memory in types that zero their backing buffer on drop (zeroize), refuse accidental Debug/Display/serde exposure (secrecy), and use constant-time comparison where equality checks are reachable from untrusted input (subtle). Defense-in-depth against accidental log leaks, coredump exposure, swap-file persistence of stale buffers, serialization mistakes, and timing side-channels — not a substitute for OS-level isolation, but a meaningful raise of the floor."

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Eliminate accidental disclosure of long-lived secrets (Priority: P1)

A Guardian operator is investigating a production incident and pulls server logs, a panic message, or a `serde_json` error trace that includes the offending struct. None of the secret values held in process memory — signing-key material accessed by the ACK signer, the dashboard cursor HMAC secret, operator/EVM session tokens, the database connection URL with embedded password, or EVM RPC URLs with embedded API keys — must appear in the captured output. Any field carrying a long-lived secret cannot be reached by `Display` or by the standard serialization layer *at all*: those impls are absent on the wrapper, so accidental `{}` or `serde_json::to_string` either fails to compile or is impossible to write. `Debug` is allowed only as a non-disclosing redaction. The bytes are reachable solely through an explicit, named "exposure" method.

**Why this priority**: Accidental log/serialize/panic disclosure is the highest-frequency class of secret exfiltration in long-running services. Closing this surface protects every other layer of the system at low implementation cost and lands real value on day one.

**Independent Test**: A reviewer adds a compile-time test (`static_assertions::assert_not_impl_any!`) for each wrapper type asserting it does not implement `Display` and does not implement `serde::Serialize`. A second test renders each wrapper through `{:?}` and a panic message and asserts the output contains only the redaction marker.

**Acceptance Scenarios**:

1. **Given** any wrapper type defined by this feature, **When** a developer writes `format!("{}", secret)` or `serde_json::to_string(&secret)` or derives `Serialize` on an enclosing struct that contains a wrapper, **Then** the code fails to compile (the wrapper does not implement `Display` or `Serialize`).
2. **Given** any wrapper type, **When** it is formatted with `{:?}`, **Then** the output is an opaque redaction marker and never the underlying bytes, hex, or URL credentials. Enclosing structs that *already* derive or implement `Debug` and contain a wrapper field inherit this redaction through the wrapper's `Debug` impl; this feature does not require enclosing structs to gain a `Debug` impl they did not previously have.
3. **Given** a `panic!` whose payload is a `String` produced from a wrapper via `{:?}`, **When** the panic message is rendered to stderr or a structured log, **Then** the secret value is not present in the payload.
4. **Given** the public HTTP response types and configuration types reachable from the server's handlers, **When** a structural test or reviewer audit walks those types' fields, **Then** none of them transitively contain a wrapper — secret types live only on internal state, never on the response/config boundary.
5. **Given** an operator or developer who needs the secret value to use it (signing, HMAC verification, pool construction), **When** they call the explicit `expose_secret()` (or equivalent) method, **Then** they receive the underlying bytes and the call site is grep-able from a security audit.

---

### User Story 2 — Erase secrets from process memory promptly (Priority: P1)

When a long-lived secret is dropped — process shutdown, session expiry/rotation, configuration reload, or simply going out of scope — its backing buffer is overwritten with zeros before the allocator reclaims it. This shrinks the window in which a coredump, swap-file fragment, or post-free heap inspection could yield the secret.

**Why this priority**: Coredump and swap exposure are realistic risks for any process holding cryptographic material; the cost of zeroization-on-drop is negligible and the benefit is concrete.

**Independent Test**: A unit test constructs a secret type, drops the value, and asserts the type implements the zeroize contract (via the chosen crate's trait bound) and that every storage site in the inventory uses a zeroizing wrapper.

**Acceptance Scenarios**:

1. **Given** a populated session map containing operator/EVM session tokens, **When** a session expires and is evicted from the map, **Then** the token's backing string buffer is zeroed before deallocation.
2. **Given** a running server, **When** the process receives `SIGTERM` and proceeds through orderly shutdown, **Then** every secret-bearing field in `AppState` is dropped and zeroized.
3. **Given** an ACK signer that loads private-key bytes into memory to perform a single signing operation, **When** the signing call returns, **Then** the intermediate buffer holding the loaded key material is zeroized before the function returns control to the caller.

---

### User Story 3 — Resist timing side-channels on byte-by-byte secret equality checks (Priority: P2)

Anywhere the server compares a secret value to a value derived from untrusted input using **byte-by-byte equality** — HMAC tag verification on signed cursors, nonce match against the stored expected value, any future bearer-token / API-key comparison — the comparison runs in time independent of which byte differs. This denies a network attacker the ability to learn a secret one byte at a time by measuring response latency.

**Scope clarification (binding)**: this feature changes the session-token map storage shape so that the **key is a non-secret digest** of the token (e.g. `sha256(token)`) and the token itself is not retained in memory after the Set-Cookie response. Lookup is structural over the digest, not byte-by-byte against a stored secret, and no constant-time compare is needed on that path. The mechanics of the storage-shape change are recorded in `plan.md` / `data-model.md`. If a future refactor reintroduces storing tokens by-value, byte-equality compare against untrusted input, or any other shape that does perform byte-by-byte equality against a stored token, **that refactor must add a constant-time compare**, and this spec must be amended accordingly.

**Why this priority**: Timing-side-channel attacks against high-entropy random tokens have low realistic payoff, but the cost of a constant-time compare on the in-scope sites is negligible and the change is mechanical.

**Independent Test**: An audit greps for `==`, `eq_ignore_ascii_case`, `String::eq`, and `[u8]::eq` against any value held in a wrapper type at the in-scope sites (HMAC verification, nonce echo match), and confirms every such site routes through a single named constant-time helper.

**Acceptance Scenarios**:

1. **Given** a signed pagination cursor with an attached HMAC tag, **When** the server verifies the tag against the computed tag, **Then** the comparison runs in time independent of the position of the first byte difference. (The existing implementation uses `hmac::Mac::verify_slice`, which RustCrypto documents as constant-time; no change is required here other than to confirm the citation in code and `research.md`.)
2. **Given** a session-cookie lookup, **When** the server resolves it, **Then** the lookup is keyed by a non-secret digest of the candidate token and no byte-by-byte equality against a stored secret occurs.
3. **Given** any future byte-by-byte equality site introduced by later work, **When** it compares a wrapped secret against untrusted input, **Then** it MUST route through the feature's named constant-time helper or the equivalent canonical primitive from the relevant crypto crate.

---

### Edge Cases

- A secret value is `Clone`d to hand to a background task: the clone must own an independent buffer; dropping one must not zero the other's storage.
- A secret is held inside an `Arc`: zeroize-on-drop fires on last-reference drop. This is acceptable but must be documented so reviewers do not assume earlier release.
- A panic occurs while a secret is on the stack between load and use: the wrapper's `Drop` must run during unwinding and zero the buffer.
- A secret reaches the logging or tracing layer through `Display` (e.g. someone writes `tracing::info!(token = %tok, ...)`): this MUST fail to compile because the wrapper does not implement `Display`. The redaction marker is reachable only through `{:?}` (`Debug`).
- An `Err(_)` variant carrying a secret-bearing struct is rendered via `Debug` (typically by `tracing::error!(?err, ...)` or `format!("{err:?}")`): the error rendering must surface only the redaction marker for the secret field; the rest of the error struct may render normally.
- A panic payload is constructed from a secret-bearing struct via `format!("{secret:?}")` (the only formatter available): the panic message contains only the redaction marker. Note that Rust does **not** print local variables in standard backtraces, so the panic-message path is the realistic surface to defend.
- A test that needs to assert on a secret's value reaches through the explicit exposure method; such call sites are audit-visible and acceptable.
- A secret value that travels through `String` / `Vec<u8>` before being wrapped: this spec covers the wrapped form going forward but does not retroactively zeroize bytes that lived in transient containers prior to wrapping. The design should minimize such transient containers.
- A field name is ambiguous between "credential URL" and "configuration URL" (e.g. an RPC endpoint that may or may not embed an API key): the spec treats it as a secret by default; downgrading requires explicit reviewer sign-off.

## Requirements *(mandatory)*

### In-Scope Inventory

These long-lived secret-bearing locations are in scope. Names are authoritative; cited paths may drift with refactors.

- **Falcon signing-key material** accessed via `MidenFalconRpoSigner.keystore` (`crates/server/src/ack/miden_falcon_rpo/signer.rs`) — loaded private-key bytes for the duration of a sign call. **Dependency boundary**: the actual key bytes live inside `miden-keystore`'s `FilesystemKeyStore` and are handled by external `miden-*` types during `self.keystore.sign(...)`. This feature owns only what this repository controls — the `Arc<FilesystemKeyStore<...>>` field, any cached metadata, and any local copy made *before* the call into the external API. The external types' zeroization is out of this feature's hands; see FR-011 for the verification requirement.
- **ECDSA signing-key material** accessed via `MidenEcdsaSigner.keystore` (`crates/server/src/ack/miden_ecdsa/signer.rs`). Same dependency-boundary caveat as Falcon: the sign path delegates to `self.keystore.ecdsa_sign(...)`; this feature owns the surrounding field and any local copies, not the keystore-internal buffers.
- **Transient key material inside `AwsSecretsManagerProvider`** (`crates/server/src/ack/secrets_manager.rs`): the `secret_hex: String` returned by `secret_string()` and the `secret_bytes: Vec<u8>` decoded inside `parsed_secret_key()`. These are stack-local within a single fetch+parse call (no cache exists today) but carry full Falcon/ECDSA private-key material in the call frame and warrant defense-in-depth wrapping as an explicit exception to the request-scoped Out-of-Scope rule. The *secret IDs* themselves are not secrets. If a cache is added later, it must use the same wrappers.
- **Dashboard cursor HMAC secret** (`CursorSecret`, `crates/server/src/dashboard/cursor.rs`) — 32-byte process-scoped HMAC key; currently has manual `Debug` redaction but no zeroize.
- **Operator session tokens** held in `DashboardState.sessions` (`crates/server/src/dashboard/state.rs`).
- **EVM session tokens** held in `EvmSessionState.sessions` (`crates/server/src/evm/session.rs`).
- **Database connection URL** flowing through `StorageMetadataBuilder.database_url` (`crates/server/src/builder/storage.rs`) and any field that retains the URL after pool construction.
- **EVM RPC URLs** in `EvmChainConfig.rpc_url` (`crates/server/src/evm/config.rs`) — treated as credentials because they often embed API keys.

### Out-of-Scope

- Request-scoped stack-local values that do not outlive a single function call (e.g. a signature byte slice received in a request and verified within the same handler).
- TLS server private keys: the server does not currently terminate TLS in-process. If TLS termination is added later, that material must be added to this scope as a follow-on.
- Anything held only in the Rust / TypeScript SDKs consumed by external users (`crates/client`, `crates/miden-multisig-client`, npm packages). The SDKs have their own threat model.
- Disk-at-rest encryption of keystore files.
- OS-level mitigations (`mlock`, secure enclaves, KMS-side signing). Explicitly framed as out of scope; this feature is "raise the floor", not "replace OS isolation".
- **The OS process environment block.** Linux exposes the entire env block at `/proc/<pid>/environ` for the lifetime of the process; coredumps capture it; child processes fork-inherit it by default; ECS task definitions, `docker run -e`, and dotenvy-loaded `.env` files all populate this block before Rust runs. Wrapping a secret env-var value *after* `std::env::var` reads it cannot retroactively zero the env block. Mitigating this is an infrastructure concern (prefer AWS Secrets Manager runtime fetch over env-var injection, which is already how the Falcon and ECDSA keys are loaded in prod). This feature deliberately does not call `unsafe { std::env::remove_var(...) }` after reads — the threat reduction is small relative to the process-global `unsafe` cost.
- **Operator pending challenges** (`dashboard/types.rs`'s `signing_digest` and the surrounding `PendingChallenge`) and **EVM pending challenges** (`EvmChallenge` in `evm/session.rs`). Rationale: (1) these values are intentionally sent to the client in the challenge response (otherwise the client cannot sign them), so they are not memory-only secrets in the same sense as session tokens or HMAC keys; (2) the EVM stored type *is* the public response DTO (`api/evm.rs`), and FR-003 forbids wrappers from crossing the response/config boundary. Wrapping them would force a split into separate internal-storage vs public-DTO types, which is a larger refactor than this feature warrants for a value that is already disclosed to the holder by design. **Forward-pointer for reviewers**: the existing `pending.challenge.nonce.eq_ignore_ascii_case(nonce)` compare at `crates/server/src/evm/session.rs:113` is a non-constant-time equality against an untrusted-input nonce. This is a known consequence of keeping challenges out of scope (the nonce is already disclosed to the client in the prior challenge response, so a timing oracle reveals only a value the holder already has). It is deliberately not addressed here. If a future change converts a challenge into a server-only secret (e.g. a server-side proof material that is never returned), both the challenge type and that comparison must be brought into scope by an amendment to this spec.

### Functional Requirements

- **FR-001**: Every in-scope secret MUST be stored as a type that overwrites its backing buffer with zeros when dropped.
- **FR-002**: Every in-scope secret wrapper MUST NOT implement `std::fmt::Display`. `Debug` MAY be implemented but MUST render only a non-disclosing redaction marker; the underlying bytes/string MUST NOT be reachable through any formatter trait.
- **FR-003**: Every in-scope secret wrapper MUST NOT implement `serde::Serialize` or `serde::Deserialize`. Enclosing structs that derive `Serialize` MUST NOT contain a wrapper field (this is structurally enforced: the derive will fail to compile because the wrapper does not implement the trait). The only path to the bytes is an explicit named exposure method.
- **FR-004**: Every **byte-by-byte equality** check that compares a wrapped secret against a value derived from an untrusted input source MUST run in time independent of the position of the first byte difference. Sites that already use a canonical constant-time primitive from a crypto crate (e.g. `hmac::Mac::verify_slice`) satisfy this requirement without further change, provided the constant-time property is documented at the call site. Session-token lookups satisfy this requirement structurally because the map is keyed by a non-secret digest (see User Story 3); the spec MUST be amended if any future refactor reintroduces byte-by-byte equality against a stored token.
- **FR-005**: Cloning a secret-bearing wrapper MUST produce an independently-owned buffer such that dropping one does not zero the other's storage.
- **FR-006**: Where a secret is held inside a shared smart pointer, zeroization MUST fire on last-reference drop; the wrapper choice MUST be compatible with shared ownership.
- **FR-007**: A documented review guard MUST exist for any new field whose name matches secret-ish patterns (`*_key`, `*_secret`, `*_token`, `password`, `*_hmac`, and credential-bearing `*_url`) and is not wrapped in an approved secret type. The guard is the manual reviewer checklist in `CONTRIBUTING.md` plus the `quickstart.md` audit recipe. "Reflection over `AppState`" is **not** an acceptable mechanism — Rust does not provide runtime field reflection without custom derive/codegen, and chasing that path is out of scope.
- **FR-008**: Every legitimate "expose" call site MUST be grep-able by a single fixed substring so a security review can enumerate all such sites in seconds.
- **FR-009**: Removal of the wrapper from an in-scope field MUST require a change visible at the type level — i.e. you cannot regress by editing a single line in the field definition without it being obvious to a reviewer.
- **FR-010**: The chosen crate dependencies MUST be widely-used, actively maintained, and acceptable under the project's existing dependency policy.
- **FR-011**: For each in-scope secret whose actual byte storage lives in an external dependency (notably the `miden-keystore` crate path used by the Falcon and ECDSA ACK signers), the plan MUST either (a) document a verification that the external type already zeroizes its buffers on drop, with a citation to the relevant upstream code, or (b) introduce a constrained local adapter that copies the bytes into a local wrapper for the duration of use and zeroizes the copy on drop, with the original boundary documented. The spec does not promise zeroization of memory owned by external crates beyond this verification.
- **FR-012**: Every read of a secret-bearing environment variable MUST construct the wrapper in a single expression at the read site (e.g. `CredentialUrl::new(std::env::var(NAME)?)`). No intermediate `String` may be held in a config-struct field or local binding between the `env::var` call and the wrapper construction. This minimizes the in-process window during which the env-var value lives as an un-zeroized `String`. The OS-level env block remains exposed (see Out-of-Scope) — this requirement bounds the *Rust-side* exposure window only.

### Key Entities

- **Secret wrapper type** — A named newtype around a byte buffer (or string buffer) that owns its bytes, zeroizes them on drop, **redacts `Debug` to a non-disclosing marker, and omits `Display` and `serde::Serialize` / `serde::Deserialize` impls entirely**, and exposes the inner value only through an explicit named method. Distinct variants may exist for "fixed-size key", "variable-length token", and "credential URL".
- **Constant-time-equality helper** — A named function or trait method that takes two byte slices (or two wrapped secrets) and returns equality computed in constant time.
- **Exposure call site** — A specific audit-visible expression of the form `<wrapper>.expose_secret()` (or chosen equivalent) that hands out the underlying bytes for a *single* legitimate operation (signing, HMAC, pool construction).
- **In-scope inventory** — The enumerated list above. Treated as the authoritative scope; future additions require a follow-on spec or a security-review-blessed edit to this document.

### Assumptions

- The Rust ecosystem provides crates that cover all three behaviours (zeroization, non-disclosing wrapper, constant-time equality). Specific package selection is a planning-phase decision recorded in `plan.md`, not here.
- The performance overhead of zeroization-on-drop is negligible at the rates these secrets are dropped (session expiry, shutdown).
- The codebase is willing to give up the ability to print configuration structs verbatim in startup logs; any startup logging that previously dumped a config struct will be replaced with explicit field-by-field logging that names only safe fields.
- Manual redaction of `Debug` impls on enclosing structs is acceptable; alternatively, the enclosing struct opts into a derive provided by the chosen crate.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of the in-scope inventory is wrapped in a secret type that zeroizes on drop, with a single audit-visible exposure method per access site.
- **SC-002**: For each wrapper type, a compile-time assertion (`static_assertions::assert_not_impl_any!`) confirms it does not implement `Display`. Manual `Debug` impls on every wrapper render only the redaction marker; this is asserted by a runtime test in the standard server test suite.
- **SC-003**: For each wrapper type, a compile-time assertion confirms it does not implement `serde::Serialize` or `serde::Deserialize`. A reviewer running a one-line grep for `#[derive(...Serialize...)]` on enclosing structs that hold a wrapper finds zero results (and would not compile if added).
- **SC-004**: Every byte-by-byte equality site in the in-scope inventory either (a) uses the existing constant-time primitive provided by its canonical crypto crate (e.g. `hmac::Mac::verify_slice` for HMAC tag verification, which is documented constant-time) or (b) routes through a single named constant-time helper in this feature's module. Both paths are grep-able and reviewed. Session-token map-lookup sites are out of scope per the storage-shape decision in `plan.md` (sessions are keyed by a non-secret digest, so equality is structural, not against a stored secret).
- **SC-005**: A test in the standard server test suite asserts that, for each wrapper type, formatting it with `{:?}` produces only the redaction marker. The same test confirms (by virtue of compilation) that `Display` and `Serialize` are not available paths.
- **SC-006**: A security reviewer can enumerate every legitimate exposure of a wrapped secret by grepping for a single token (e.g. `expose_secret`); the resulting list matches the inventory above with no surprises.
- **SC-007**: At least one verification — a manual heap-inspection check during development, or a test exercising the wrapper's drop behaviour — confirms that secret buffers are zeroed before the process exits. Recorded as a sanity check, not an ongoing assertion.
- **SC-008**: For every in-scope secret whose bytes live inside `miden-keystore` or another external crate, the plan records either an upstream-code citation showing the external type already zeroizes on drop, or the design of the local adapter that copies-and-zeros bytes at the boundary (per FR-011).
- **SC-009**: The compile-time non-impl assertions from SC-002 / SC-003 (wrappers do not implement `Serialize`) **combine transitively** with the existing `#[derive(Serialize)]` on every public HTTP response DTO to make "a wrapper field in a response DTO" a compile error. SC-009 is satisfied by: (a) keeping the existing `#[derive(Serialize)]` on response DTOs (asserted by `static_assertions::assert_impl_all!(Dto: serde::Serialize)` for a representative sample of DTOs, so removing the derive also fails CI); plus (b) the SC-002 / SC-003 non-impl assertions on wrappers. No separate "structural walk" is needed — the assertion mechanism is already a compile error.
- **SC-010**: For each secret-bearing env var read by the server (`DATABASE_URL`, `GUARDIAN_DASHBOARD_CURSOR_SECRET`, `GUARDIAN_EVM_RPC_URLS`, plus any added later), `env::var("<NAME>")` is followed directly by a wrapper constructor call in the same expression — the env-var result is never bound to a local `String` or passed through an intermediate non-wrapper builder field. This is verified by the audit-recipe grep in `quickstart.md` and the reviewer checklist.
