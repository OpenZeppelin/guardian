# Specification Quality Checklist: Memory-Resident Secret Hygiene

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-05-29
**Feature**: [spec.md](../spec.md)

## Content Quality

- [x] No implementation details (languages, frameworks, APIs)
  - Note: crate names (`zeroize`, `secrecy`, `subtle`) appear only in the **Input** quote of the user's original request; the spec body intentionally defers package selection to plan.md.
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
  - Note: the audience is security-aware reviewers; behaviour is described in terms of observable disclosure / erasure / timing properties, not internal types.
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [x] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded (explicit In-Scope Inventory + Out-of-Scope)
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows (disclosure, erasure, timing)
- [x] Feature meets measurable outcomes defined in Success Criteria
- [x] No implementation details leak into specification

## Notes

- The user's original prompt named three specific crates (`zeroize`, `secrecy`, `subtle`) and asked the agent to "decide what package/s to use". Per spec discipline (WHAT not HOW), the spec defines required *behaviours* and defers package selection to `/speckit-plan`. The Input field preserves the user's wording verbatim.
- The in-scope inventory was generated from a codebase sweep on 2026-05-29 and lists 9 categories of secret-bearing storage. Future additions require an edit to this spec.
- TLS server private keys are deliberately Out-of-Scope: the server currently does not terminate TLS in-process. Add to scope if that changes.

### Resolved review findings (2026-05-29)

1. **Display/Serialize model — resolved to compile-time refusal.** FR-002 and FR-003 now mandate "wrapper does not implement `Display` / `Serialize`" rather than "renders redacted output at runtime". Acceptance scenarios use `static_assertions::assert_not_impl_any!`. `Debug` is the only allowed formatter trait and renders only a redaction marker. Aligns with the reviewer's open-question preference.
2. **Session-token storage shape — resolved by explicit scope clarification.** FR-004 and User Story 3 now explicitly carve out `HashMap<String, _>`-keyed session lookups (the storage shape used by `DashboardState.sessions` and `EvmSessionState.sessions`) as out of scope for constant-time compare, with rationale and a forward-looking trigger requiring the spec to be amended if a refactor introduces byte-by-byte session equality.
3. **ACK signer external dependency — resolved by adding FR-011 and SC-008.** The in-scope inventory now spells out the `miden-keystore` boundary at the Falcon and ECDSA signer entries. FR-011 requires the plan to either cite upstream zeroization or design a local copy-and-zero adapter. SC-008 makes the verification a release criterion.
4. **Backtrace acceptance criterion — replaced.** The "backtrace frame captures a secret" scenario has been replaced with a panic-payload scenario (acceptance scenario 3 of User Story 1) and an edge case note that Rust does not print locals in standard backtraces.
5. **Reflection over `AppState` — removed.** FR-007 now uses the manual checklist plus audit recipe and explicitly disallows pursuing a reflection-based guard.

### Resolved review findings — second round (2026-05-29)

6. **Challenge nonces / digests — removed from scope.** The previous inventory entry "Operator and EVM challenge nonces" has been deleted from the In-Scope Inventory and moved to Out-of-Scope with explicit rationale: (a) operator challenges store `signing_digest`, not a server-only nonce; (b) `EvmChallenge` doubles as the public response DTO in `api/evm.rs`; (c) the values are intentionally returned to the client. Wrapping them would force a split into internal-storage vs public-DTO types, a refactor disproportionate to the threat. Re-scoping requires a spec amendment.
7. **Debug requirement on enclosing structs — relaxed.** US1 acceptance scenario 2 no longer requires `{:?}` on enclosing structs like the ACK signer or Postgres pool config. The binding requirement is now: wrappers redact their own `Debug`; *already-Debug* enclosing structs inherit that redaction through the field; secret-bearing enclosing structs are not required to gain a `Debug` impl they did not previously have.
8. **Key Entities entity definition — corrected.** The "Secret wrapper type" entity now reads "redacts `Debug` to a non-disclosing marker, and omits `Display` and `serde::Serialize` / `serde::Deserialize` impls entirely", aligning with FR-002 / FR-003. The previous "suppresses Debug / Display / Serialize" wording was internally inconsistent.

### Resolved review findings — third round (post-plan, 2026-05-29)

9. **Session-token storage shape — restructured.** The previous "wrap the value side" design did not actually wrap the token, because in `DashboardState.sessions` / `EvmSessionState.sessions` the token *is* the `HashMap` key, not a value field. Leaving the key as `String` would leave the long-lived token un-zeroized, violating FR-001. The plan and data-model now restructure the map to `HashMap<[u8; 32], Record>` keyed by `sha256(token)`; the plaintext token is generated, written to `Set-Cookie`, and dropped — not retained. `spec.md` User Story 3 and FR-004 updated accordingly; SC-004 no longer mentions "nonce echo match".
10. **Dependency direction — fixed.** The previous data-model proposed using the server's `secret::SecretBytes` from inside `crates/miden-keystore`. That inverts the dependency graph. The plan, data-model, and contract now use `zeroize::Zeroizing<Vec<u8>>` directly inside `miden-keystore`. Server-only wrappers remain `pub(crate)` and never reachable from a lower crate. Forbidden-Cargo-change list explicitly bans inverting this later.
11. **HMAC verify — kept as `hmac::Mac::verify_slice`.** The previous data-model claimed HMAC verify would route through `secret::ct::eq`. Reviewer correctly pointed out this would replace an audited constant-time MAC verify with a home-rolled byte equality. The plan, data-model, and a new research Decision 8 record that `verify_slice` is kept and cited at the call site. `secret::ct::eq` remains in the module for future byte-equality sites that need it.
12. **`secrecy` version pin — tightened.** Bumped from "latest 0.8.x" to **`"0.10"` with `default-features = false`**. The `secrecy/serde` feature is explicitly forbidden (it would add a `Deserialize` impl on `SecretBox<T>` and violate FR-003); the `static_assertions` block is the compile-time backstop. Recorded in research Decision 1 and contracts/secret-module.md "Forbidden Cargo changes".
13. **SC-004 wording — cleaned.** Removed the "nonce echo match" example (challenges are out of scope from the second round). SC-004 now covers (a) sites using a canonical crypto-crate constant-time primitive and (b) sites using `secret::ct::eq`, and explicitly notes that session-token lookups are structural over digests, not byte-equality.

### Resolved review findings — fourth round (env-var coverage, 2026-05-29)

14. **Env-var read window — bounded.** Added **FR-012** requiring every secret-bearing env-var read to fold `std::env::var(...)` and the wrapper constructor into a single expression — no intermediate `String` local. Inventory of in-scope env vars confirmed by grep: `DATABASE_URL` (`audit/postgres.rs:264`, `builder/storage.rs:79`), `GUARDIAN_DASHBOARD_CURSOR_SECRET` (`dashboard/config.rs:47`), `GUARDIAN_EVM_RPC_URLS` (`evm/config.rs:77`). Migration steps 2–4 of the data-model rewrite the reads to comply. Added **SC-010**, verified by the `quickstart.md` audit-recipe grep and the FR-007 reviewer checklist plus the compile-time assertions.
15. **OS env block — explicitly Out-of-Scope.** The OS process environment block (`/proc/<pid>/environ`, coredumps, fork-inherited env, dotenvy `.env`, ECS task-definition `environment`) is now a named Out-of-Scope item in the spec, with framing: same threat-model line as TLS keys; mitigation is an infra concern (prefer AWS Secrets Manager runtime fetch). The plan explicitly does **not** introduce `unsafe { std::env::remove_var(...) }` calls — small threat reduction, real process-global `unsafe` cost. Quickstart audit recipe now includes a grep for any such calls.
16. **FR-007 guard settled on manual review.** Research Decision 5 documents why the final guard is the manual-review arm plus compile-time assertions. Quickstart keeps the reviewer audit recipe.

### Resolved review findings — fifth round (compile/test rigor, 2026-05-29)

17. **`PartialEq` / `Eq` added to all four wrapper types.** Without this, `EvmChainConfig` and `EvmChainRegistry` (`evm/config.rs:9, 16`), which derive `PartialEq, Eq`, would fail to compile after `rpc_url: String` is re-typed to `CredentialUrl`. `secrecy::SecretBox<T>` does not implement `PartialEq`. The wrapper impls route through `subtle::ConstantTimeEq` internally — defense-in-depth that makes any `==` on a wrapper automatically constant-time. Recorded in data-model wrapper tables and contracts/secret-module.md.
18. **`Clone` is hand-rolled on every wrapper.** `secrecy::SecretBox<T>` is not `Clone` either. Every wrapper's `Clone` allocates a new buffer through `expose_secret()`. Data-model and contracts now spell this out so implementers don't try `#[derive(Clone)]` and get a confusing error.
19. **AWS Secrets Manager inventory entry corrected.** Previous wording ("fetched bytes; cache field if present") was speculative and misaligned with the actual code. There is no cache; the secret-bearing values are the transient `secret_hex: String` and `secret_bytes: Vec<u8>` inside `parsed_secret_key`/`secret_string`. Both are stack-locals carrying full private-key material — explicit Out-of-Scope exception in the spec inventory. Migration step 7 rewritten accordingly: wrap `secret_hex` in `SecretString`, `secret_bytes` in `SecretBytes`; no return-type change.
20. **SC-009 mechanism rewritten.** The previous "compile a function that takes the DTO by value" was a no-op (didn't exercise `Serialize`). New mechanism: `static_assertions::assert_impl_all!(Dto: Serialize)` on a representative sample of public response DTOs, combined with the existing `assert_not_impl_any!` on wrappers (SC-002/003), transitively makes "wrapper field in a DTO" a compile error.
21. **`secret::ct::eq` is `#[allow(dead_code)]`.** It has zero callers in this feature (HMAC verify stays on `verify_slice`; session lookup is digest-keyed). Marked `pub(crate)` with `#[allow(dead_code)]` so `-D warnings` does not break CI on step 1; a unit test exercises it so it does not bit-rot silently.
22. **FR-007 review posture finalized.** Migration step 9 now documents the reviewer checklist / quickstart audit posture only.
23. **Session-token lifetime framing softened.** Research Decision 6 previously claimed the plaintext token lives "hundreds of nanoseconds" — inaccurate, because the token is embedded in `Set-Cookie` and the response payload and persists for the issuing request's duration. Reworded to "request-scoped, out of strict zeroization scope per general Out-of-Scope rule". Migration steps 5/6 reworded to drop the implication that `String::drop` zeroizes (it does not).
24. **Forward-pointer added for `evm/session.rs:113` `eq_ignore_ascii_case`.** The existing non-constant-time challenge-nonce compare is a known consequence of keeping challenges out of scope (nonce is already disclosed to the holder in the prior challenge response). Spec Out-of-Scope now names the call site so a future auditor does not re-litigate it.

### Resolved review findings — post-implementation round (2026-05-29)

25. **FR-011 / SC-008 citations added at the signer call sites.** The Falcon and ECDSA `sign_with_server_key` comments now read "Verified: miden-crypto 0.23.0 (pulled in by miden-protocol 0.14.5) implements `impl ZeroizeOnDrop for SecretKey {}` in `src/dsa/falcon512_poseidon2/keys/secret_key.rs` / `src/dsa/ecdsa_k256_keccak/mod.rs`". Re-verified by direct grep against the cached crate source. Research.md Decision 2 now records branch **(a)** as the outcome with the same citations. Earlier session memory that flagged Falcon as "historically not zeroizing" pre-dated the current pinned revision; the current pinned crate does implement it.
26. **Module visibility reverted to `pub(crate)` per the contract.** `crates/server/src/lib.rs` is `pub(crate) mod secret;`, `mod.rs` is `pub(crate) use wrappers::{…}`, and every wrapper type + method is `pub(crate)`. The previous transient `pub` was a clippy `private-interfaces` shortcut; the proper fix was to make `EvmChainConfig.rpc_url` a `pub(crate)` field instead (no external consumer accesses it). The contract doc and constitution check are once again literally true: wrappers live only inside `guardian-server`.
27. **SC-009 sample broadened to span wrapper-adjacent modules.** `secret/tests.rs` now asserts `Serialize` impls for `DashboardInfoResponse` (storage/info layer adjacent to `StorageMetadataBuilder`) and the EVM HTTP DTOs `VerifySessionResponse` + `ChallengeResponse` (under `#[cfg(feature = "evm")]`), in addition to the original five dashboard sample DTOs. Matches T018's "sample spans wrapper-bearing modules" intent. The transitive compile-time guarantee (any DTO with a wrapper field fails its `#[derive(Serialize)]`) was always crate-wide; this just tightens the explicit tripwire.
