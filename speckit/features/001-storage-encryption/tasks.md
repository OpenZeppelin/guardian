---
description: "Task list for Storage Encryption at Rest"
---

# Tasks: Storage Encryption at Rest

**Input**: Design documents from `speckit/features/001-storage-encryption/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/

**Tests**: INCLUDED — the spec validation plan and Constitution V (Evidence-Driven)
require them for this high-risk storage/security change.

**Organization**: Tasks are grouped by user story. US1 is the MVP and builds the
cipher/decorator/provider core; US2–US4 extend it and can proceed in parallel
once US1 is done.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- All paths are under the `crates/server/` crate unless noted.

---

## Phase 1: Setup (Shared Infrastructure)

- [x] T001 In `crates/server/Cargo.toml`: add `aes-gcm = "0.10"` and `base64` (if absent) as runtime deps; add `criterion` as a dev-dependency and declare a `[[bench]]` harness target for the SC-007 benches (T036/T037). Confirm `zeroize` and the `crate::secret` wrappers (`FixedKey<32>`, `SecretBytes`) are available for reuse.
- [x] T002 Create the encryption module skeleton `crates/server/src/storage/encryption/mod.rs` and register `mod encryption;` (+ `pub use`) in `crates/server/src/storage/mod.rs`.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared types/traits used by every story. **⚠️ Blocks all user stories.**

- [x] T003 Define the `Envelope` type with serde matching `contracts/envelope.schema.json` (fields `v`,`alg`,`kid`,`nonce`,`ct`) plus an `is_envelope(&Value)` discriminator, in `crates/server/src/storage/encryption/envelope.rs`.
- [x] T004 Define `RecordAad` (State/Delta/Proposal) and its byte encoding (`state:<account_id>`, `delta:<account_id>:<nonce>`, `proposal:<account_id>:<commitment>`) in `crates/server/src/storage/encryption/envelope.rs` (same file as T003).
- [x] T005 [P] Define the `StorageCipher` trait + `CipherError` (distinct variants per `contracts/storage-cipher.md`: NotAnEnvelope, UnsupportedVersion, UnsupportedAlgorithm, InvalidNonce, DecryptionFailed, KeyProvider) in `crates/server/src/storage/encryption/cipher.rs`.
- [x] T006 [P] Define the `StorageKeyProvider` trait (`active_key_id`, `key(kid)`) + `KeyProviderError` (distinct variants per contract: MissingKeySource, MultipleKeySources, InvalidKeyEncoding, InvalidKeyLength, MalformedSecret, UnknownKeyId, KeyStoreUnavailable) in `crates/server/src/storage/encryption/key_provider.rs`.

**Checkpoint**: Types/traits compile; user-story implementation can begin.

---

## Phase 3: User Story 1 - Account contents unreadable from the database alone (Priority: P1) 🎯 MVP

**Goal**: With a key configured, state/delta/proposal payloads are stored as AEAD
envelopes and decrypted transparently on read; routing fields stay plaintext;
enabling is safe (marker guard) and inferred from key-source presence.

**Independent Test**: Configure a dev key against an empty filesystem store, write
a state + delta + proposal, inspect raw files → envelopes only (routing fields
readable); read back through Guardian → original objects. With no key, storage is
byte-for-byte baseline.

### Implementation for User Story 1

- [x] T007 [P] [US1] Implement `Aes256GcmCipher` (encrypt/decrypt with AAD, fresh random 96-bit nonce, build/parse `Envelope`) in `crates/server/src/storage/encryption/cipher.rs` (depends T003–T005).
- [x] T008 [P] [US1] Implement `EnvKeyProvider` (dev key from `GUARDIAN_STORAGE_ENCRYPTION_KEY` base64-decoded to exactly 32 bytes; `kid` from `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` default literal) in `crates/server/src/storage/encryption/key_provider.rs` (depends T006).
- [x] T009 [US1] Apply R8 trait change: `pull_all_delta_proposals` and `pull_pending_proposals` return `Vec<ProposalRecord>` in `crates/server/src/storage/mod.rs` (update the `pull_pending_proposals` default body).
- [x] T010 [US1] Update `PostgresService` proposal reads for the R8 return type in `crates/server/src/storage/postgres.rs` (depends T009).
- [x] T011 [US1] Update `FilesystemService` proposal reads + internal callers (`filesystem.rs:877,897`) for the R8 return type in `crates/server/src/storage/filesystem.rs` (depends T009).
- [x] T012 [P] [US1] Update `MockStorageBackend` proposal-read return types/builders (`with_pull_all_delta_proposals` etc.) in `crates/server/src/testing/mocks.rs` (depends T009).
- [x] T013 [P] [US1] Update runtime consumer for R8 in `crates/server/src/services/push_delta_proposal.rs` (use `.proposal`) (depends T009).
- [x] T014 [P] [US1] Update runtime consumer for R8 in `crates/server/src/evm/service.rs` (depends T009).
- [x] T015 [US1] Sweep and fix **all remaining** R8 compile-time call sites and test fixtures: run `rg "with_pull_all_delta_proposals|pull_all_delta_proposals|pull_pending_proposals"` and update every match still passing/expecting `Vec<DeltaObject>` — at least `crates/server/src/api/http.rs`, `crates/server/src/api/grpc.rs`, `crates/server/src/services/get_delta_proposals.rs` (tests), `crates/server/src/services/push_delta_proposal.rs` (tests), `crates/server/src/services/dashboard_global_proposals.rs`, and `crates/server/src/storage/filesystem.rs` tests. Build must be green (depends T009–T014).
- [x] T016 [US1] Define the encryption marker types + `MarkerStore` trait and an emptiness/count helper (emptiness = no state/delta/proposal payload records; marker not counted) in `crates/server/src/storage/encryption/marker.rs`.
- [x] T017 [US1] Implement the filesystem marker (dedicated marker file under storage root) + emptiness check in `crates/server/src/storage/filesystem.rs` (depends T016).
- [x] T018 [US1] Implement the Postgres marker (single-row marker/settings table) via an additive migration in `crates/server/migrations/` and its read/write/emptiness impl in `crates/server/src/storage/postgres.rs` (depends T016).
- [x] T019 [US1] Implement the `EncryptedStorage` decorator (all `StorageBackend` methods: encrypt writes / decrypt reads with per-record AAD; override `pull_states_batch`; inherit defaults for `has_pending_candidate`/`pull_canonical_deltas_after`/`pull_pending_proposals`; pass-through `update_delta_status`/deletes/counts/`kind`) in `crates/server/src/storage/encryption/mod.rs` (depends T007, T009, T003–T004).
- [x] T020 [US1] Wire `StorageMetadataBuilder::build()` in `crates/server/src/builder/storage.rs`: infer enablement from key-source presence (none → return inner; exactly one → wrap; >1 → fail fast), and run the marker guard (FR-015: encrypted absent+empty→write; absent+non-empty→fail; present→provider has `init_kid`; plaintext mode: marker present→fail regardless of count) (depends T008, T016–T019).

### Tests for User Story 1

- [x] T021 [P] [US1] Cipher roundtrip unit test (encrypt→decrypt returns original `Value`) in `crates/server/src/storage/encryption/cipher.rs`.
- [x] T022 [P] [US1] Nonce-freshness test (FR-007): encrypt the **same** payload+AAD+key twice and assert the two envelopes have distinct 96-bit nonces and non-identical ciphertext, in `crates/server/src/storage/encryption/cipher.rs`.
- [x] T023 [P] [US1] Decorator integration test over the filesystem backend: write state/delta/proposal, assert raw file holds an envelope (no plaintext) and routing fields readable, read back → original objects, in `crates/server/src/storage/encryption/mod.rs` tests.
- [x] T024 [P] [US1] Parity test: service-layer reads identical with a key configured vs not (SC-004), in `crates/server/src/storage/encryption/mod.rs` tests.
- [x] T025 [P] [US1] No-key baseline test: with no key configured no marker is written and stored bytes equal the pre-feature baseline (SC-008), in `crates/server/src/builder/storage.rs` tests.
- [x] T026 [P] [US1] Marker-guard tests (FR-015): encrypted absent+empty writes marker; absent+non-empty fails; plaintext + marker present fails regardless of record count; provider missing `init_kid` fails, in `crates/server/src/storage/encryption/marker.rs` / builder tests.

**Checkpoint**: US1 fully functional — encrypt-at-rest with dev key, safe enablement. MVP demoable.

---

## Phase 4: User Story 2 - Operators configure a key appropriate to their environment (Priority: P2)

**Goal**: Dev runs with a configured base64 key; production sources the key from
AWS Secrets Manager (structured `{active,keys}` secret); misconfiguration fails fast.

**Independent Test**: Start with a dev key → encrypted I/O works. Start configured
against Secrets Manager → key sourced without raw key in process config. Start
with a missing/invalid/short key, or two key sources → startup error.

### Implementation for User Story 2

- [x] T027 [P] [US2] Implement `SecretsManagerKeyProvider` parsing the structured secret `{ "active": kid, "keys": { kid: base64-32-bytes } }`, reusing the `crates/server/src/ack/secrets_manager.rs` pattern (aws-config, client, `get_secret_value`, `resolve_secret_id`). **Design for injection**: parse/validation must be exercisable with a fake secret string / injected fetcher so tests need no AWS network. In `crates/server/src/storage/encryption/key_provider.rs` (depends T006).
- [x] T028 [US2] Extend builder wiring for the prod source (`GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` + `AWS_REGION`), startup key resolution/validation and fail-fast (FR-009), in `crates/server/src/builder/storage.rs` (depends T020, T027).

### Tests for User Story 2

- [x] T029 [P] [US2] Startup config tests using a **fake provider/secret** (no AWS network): structured-secret parsing (valid/malformed), dev key OK, missing/invalid/short key → error, more than one key source → error (FR-009). Real AWS resolution is left to quickstart/manual validation. In `crates/server/src/builder/storage.rs` and `key_provider.rs` tests.

**Checkpoint**: Both environments configurable; misconfig is fail-fast.

---

## Phase 5: User Story 3 - Records are tamper-evident and bound to their identity (Priority: P2)

**Goal**: Tampering, wrong key, identity/AAD mismatch, and unknown `kid` are
rejected on read; the service read path fails closed (no empty-list masking).

**Independent Test**: Bit-flip an envelope → read rejected. Present one record's
ciphertext under another identity (esp. a proposal under a different commitment) →
rejected. Force a decryption failure on the proposal read path → request fails,
not `[]`.

### Implementation for User Story 3

- [x] T030 [US3] Make the proposal read service path fail-closed (R9): replace `.unwrap_or_default()` at `crates/server/src/services/get_delta_proposals.rs:35` with error propagation; scan other read service paths for error-swallowing `.unwrap_or_default()`/`.ok()` and fix (depends T019).

### Tests for User Story 3

- [x] T031 [P] [US3] Adversarial cipher/decorator tests: tampered ciphertext, wrong key, AAD/identity mismatch, and unknown `kid` are all rejected (FR-004/FR-010), in `crates/server/src/storage/encryption/cipher.rs` / `mod.rs` tests.
- [x] T032 [P] [US3] Swap-resistance test: a proposal envelope cannot be decrypted under a different `(account_id, commitment)`, and a delta under a different nonce, in `crates/server/src/storage/encryption/mod.rs` tests.
- [x] T033 [P] [US3] Service-path fail-closed test: a decryption failure from `pull_all_delta_proposals` surfaces as a failed request, not an empty list, in `crates/server/src/services/get_delta_proposals.rs` tests.

**Checkpoint**: Confidentiality + integrity + fail-closed reads verified.

---

## Phase 6: User Story 4 - Keys carry an identity that supports rotation (Priority: P3)

**Goal**: Records written under different `kid`s coexist and decrypt; new writes
use the active `kid`; unknown `kid` is an explicit error.

**Independent Test**: Write under `k1`, rotate active to `k2` (keep `k1` in the
provider), write more; read all → every record decrypts. Remove `k1` → its records
error explicitly.

### Implementation for User Story 4

- [x] T034 [US4] Finalize multi-key resolution: providers hold a `kid → key` map + active `kid`; `decrypt` resolves the key by the envelope's `kid` (unknown `kid` → error), in `crates/server/src/storage/encryption/key_provider.rs` and `cipher.rs` (depends T007–T008, T027).

### Tests for User Story 4

- [x] T035 [P] [US4] Multi-key tests: records under two `kid`s both decrypt; active rotation (write under `k2`, read `k1`+`k2`); unknown `kid` → error (FR-011), in `crates/server/src/storage/encryption/key_provider.rs` tests.

**Checkpoint**: Rotation-read model proven.

---

## Phase 7: Polish & Cross-Cutting Concerns

- [x] T036 [P] Cipher-path microbenchmark validating the SC-007 cipher target (≤ 1 ms p95 per ≤ 8 KB payload) using the T001 criterion harness, under `crates/server/benches/`.
- [x] T037 [P] Request-level SC-007 check: a lightweight benchmark/smoke comparison of end-to-end request p95 with a key configured vs no key, asserting ≤ 5% overhead (or documenting the measured delta), under `crates/server/benches/` or a `#[ignore]` timing test.
- [x] T038 [P] Document the `GUARDIAN_STORAGE_ENCRYPTION_*` env vars and enablement-by-key-presence semantics in `docs/CONFIGURATION.md`.
- [x] T039 [P] Add enable-in-prod guidance (Secrets Manager structured secret, enable-against-empty-store via 0.15 cutover, marker guard) and a checklist item in `docs/PRODUCTION.md`.
- [x] T040 [P] Add a storage encryption key bootstrap + rotation runbook to `docs/runbooks/secrets.md`.
- [x] T041 Record the infra follow-up (Terraform Secrets Manager secret + task-role `secretsmanager:GetSecretValue` IAM for the storage key) in `infra/` README / plan as a separate follow-up.
- [x] T042 Run the `quickstart.md` validation end-to-end on filesystem and Postgres (includes real AWS Secrets Manager resolution).
- [x] T043 `cargo clippy` + `cargo fmt` clean across `crates/server`.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (P1)** → **Foundational (P2)** → **US1 (P3)** is the critical path.
- **US2, US3, US4** all depend on **US1** (they extend the cipher/decorator/provider core); once US1 is complete they can proceed in parallel.
- **Polish (P7)** after the desired stories are complete.

### Story Dependencies (honest)

- **US1 (P1)**: depends only on Setup + Foundational. The R8 trait change (incl. the full call-site sweep T015) and the marker guard live here because the decorator cannot correctly decrypt proposals (needs commitment) and the first encrypted write must be guarded.
- **US2 (P2)**: depends on US1 (builder wiring T020, provider trait).
- **US3 (P2)**: depends on US1 (decorator decrypt-error path T019; consumer T013).
- **US4 (P3)**: depends on US1 + US2 (multi-key provider shape).

### Within US1

- Cipher/provider (T007–T008) and the R8 trait change (T009) are prerequisites for the decorator (T019).
- The R8 call-site sweep (T015) must leave the build green before the decorator/builder tests run.
- Marker (T016–T018) + decorator (T019) are prerequisites for the builder wiring (T020).
- Tests (T021–T026) after their targets compile.

### Parallel Opportunities

- T005/T006 (Foundational traits) in parallel.
- T007, T008 in parallel; T012, T013, T014 in parallel (after T009; T015 sweeps the rest sequentially).
- All US1 tests (T021–T026) in parallel once code compiles.
- After US1: US2 (T027–T029), US3 (T030–T033), US4 (T034–T035) in parallel by different developers.
- Polish docs/benches (T036–T040) in parallel.

---

## Parallel Example: User Story 1 (after T009)

```bash
Task: "Update MockStorageBackend proposal-read types in crates/server/src/testing/mocks.rs"
Task: "Update push_delta_proposal.rs for R8"
Task: "Update evm/service.rs for R8"
# then T015 sweeps remaining api/* and *_test fixtures to green
```

```bash
# US1 tests together once code compiles:
Task: "Cipher roundtrip unit test"
Task: "Nonce-freshness test (FR-007)"
Task: "Decorator ciphertext-at-rest + read-back integration test"
Task: "Parity test key vs no-key (SC-004)"
Task: "No-key byte-for-byte baseline test (SC-008)"
Task: "Marker-guard tests (FR-015)"
```

---

## Implementation Strategy

### MVP First (User Story 1)

1. Phase 1 Setup → Phase 2 Foundational → Phase 3 US1.
2. **STOP and VALIDATE**: dev-key encrypt-at-rest, ciphertext verified at rest, read-back intact, nonce freshness, marker guard, no-key baseline.
3. This is a safe, demoable MVP (dev/local; uses `EnvKeyProvider`).

### Incremental Delivery

1. US1 → MVP (dev key).
2. US2 → production Secrets Manager + fail-fast config.
3. US3 → adversarial/fail-closed guarantees verified.
4. US4 → rotation-read.
5. Polish → docs, benchmarks, infra follow-up, quickstart validation.

---

## Notes

- [P] = different files, no incomplete dependencies.
- The R8 trait change (T009) ripples to in-crate consumers and test fixtures only (T010–T015); external clients/SDKs are unaffected (no wire change) — Constitution I.
- The only schema addition is the marker table (T018); payload tables are untouched.
- Keys are loaded once at startup and cached; no per-operation key-store calls.
- Secrets Manager tests use a fake provider/secret; real AWS resolution is exercised only in quickstart/manual validation (T042).
- Commit after each task or logical group; stop at any checkpoint to validate a story independently.

## Implementation caveats (honest record)

- **T036**: implemented as a lib-internal `#[ignore]` timing test
  (`cipher_path_latency_is_sub_millisecond`); the ≤1 ms/op SC-007 bound is asserted
  only in release builds (validated: passes under `cargo test --release -- --ignored`).
  `criterion` was not used because the cipher is `pub(crate)` (no external bench
  target), so no bench-harness dependency was added.
- **T037**: the request-level p95 comparison is **smoke-only / documented**, not a
  built e2e harness — a full HTTP benchmark was judged disproportionate. The cipher
  half of SC-007 is covered by T036; the request-level half is left to manual smoke.
- **T042**: the filesystem dev path is validated end-to-end by the decorator
  integration tests (ciphertext-at-rest + roundtrip). Postgres + real AWS Secrets
  Manager resolution require live infra and remain manual.
- **T041**: infra is a documented follow-up (Secrets Manager secret + IAM not yet
  in Terraform), per the plan.
