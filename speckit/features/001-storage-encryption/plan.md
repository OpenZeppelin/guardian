# Implementation Plan: Storage Encryption at Rest

**Branch**: `001-storage-encryption` | **Date**: 2026-06-16 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/001-storage-encryption/spec.md`

## Summary

Add an opt-in application-layer confidentiality boundary to Guardian's storage
layer. When enabled, the three sensitive JSON payloads — `StateObject.state_json`,
`DeltaObject.delta_payload` (deltas), and the proposal `delta_payload` — are
authenticated-encrypted before they reach the concrete backend and decrypted
immediately after they are read back, so every layer above storage operates on
unchanged `StateObject` / `DeltaObject` values.

The technical approach is a **decorator** (`EncryptedStorage`) that wraps the
chosen `StorageBackend` (filesystem or Postgres) at the single construction
point in `StorageMetadataBuilder::build()`. The decorator replaces only the
payload field of each object with a self-describing JSON **envelope** (version,
algorithm, key id, nonce, ciphertext) and leaves all routing/index fields
(`account_id`, `nonce`, `prev_commitment`, `new_commitment`, `commitment`,
`status_kind`, timestamps) in plaintext. Because the envelope is itself a JSON
value, the existing `jsonb` columns and filesystem JSON files are reused with
**no migration of the payload tables** (the only schema addition is a tiny
encryption-state marker table/file, FR-015). The cipher is AEAD (AES-256-GCM) with a per-encryption
random nonce and AAD bound to record identity (`state:<account_id>`,
`delta:<account_id>:<nonce>`, `proposal:<account_id>:<commitment>`) so a
ciphertext cannot be relocated between records undetected. The 32-byte key is
resolved through a `StorageKeyProvider` — a directly-configured base64 key for
development and an AWS Secrets Manager-backed provider for production, reusing
the existing `crates/server/src/ack/secrets_manager.rs` plumbing.

Encryption is off by default and turns on simply by configuring a key source
(no enable flag); it is validated at startup (fail-fast on missing/malformed/
ambiguous key) and guarded by a persisted store-state marker so encryption can
never silently start writing into a populated plaintext store, or vice versa
(FR-015). The rollout rides the Miden 0.15 cutover, which empties the production
store, so an encryption-enabled deployment starts from an all-encrypted store
with no dual-read/backfill.

## Technical Context

**Language/Version**: Rust (workspace edition as in repo), `crates/server`  
**Primary Dependencies**: NEW `aes-gcm = "0.10"` (AEAD); existing `aws-config`, `aws-sdk-secretsmanager` (key sourcing), `aws-sdk-kms` (already present — reserved for the future wrap/unwrap provider, not used in v1), `zeroize`, `subtle`, `crate::secret` hygiene wrappers (`SecretBytes`, `FixedKey<32>`), `async-trait`, `serde_json`, `base64`, `diesel`/`diesel-async` (Postgres backend)  
**Storage**: Postgres (`states.state_json`, `deltas.delta_payload`, `delta_proposals.delta_payload` — all `jsonb`) and filesystem JSON; decorator sits above both, column/file types unchanged  
**Testing**: `cargo test` in `crates/server`; existing filesystem + Postgres backend suites; `MockStorageBackend` (`crates/server/src/testing/mocks.rs`); NEW cipher unit tests (roundtrip, tamper, wrong-key, AAD-mismatch, unknown-kid), decorator integration test over the filesystem backend, startup-guard tests (missing/invalid/ambiguous key; marker mismatch — FR-015), and a service-path error-propagation test (FR-010, R9); cipher microbenchmark for SC-007  
**Target Platform**: Linux server (ECS/Fargate)  
**Project Type**: web-service (single server crate; consumers are Rust/TS SDKs over HTTP/gRPC, unaffected)  
**Performance Goals**: per-payload AEAD cost negligible (sub-millisecond); SC-007 — no perceptible change to request latency or throughput  
**Constraints**: no migration of existing payload tables (envelope reuses existing `jsonb`); one additive single-row encryption-state marker table/file (FR-015); opt-in inferred from key-source presence (no enable flag), off when no key configured; state fixed per populated store (no mixed plaintext/ciphertext); no silent plaintext fallback; fail-fast at startup when a key source is unresolvable/ambiguous or the marker mismatches  
**Scale/Scope**: one envelope per stored state/delta/proposal payload; decorator implements the full `StorageBackend` trait surface, two proposal-read methods change return type to `ProposalRecord` (R8)

**Unknowns**: none. All spec assumptions (cipher, key store, rollout, opt-in semantics) are resolved in [research.md](./research.md).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

**I. Bottom-Up Change Propagation** — PASS with in-crate propagation. The change
is confined to `crates/server`. **External** consumers (base clients, multisig
SDKs, examples) are **unaffected**: no wire shape, gRPC/HTTP contract, or client
code changes — the decorator decrypts on read so callers receive identical
`StateObject`/`DeltaObject` values. **Within `crates/server`**, the proposal-read
trait change (research R8: `pull_all_delta_proposals` / `pull_pending_proposals`
→ `Vec<ProposalRecord>`) propagates to `services/get_delta_proposals.rs`,
`services/push_delta_proposal.rs`, `evm/service.rs`, the filesystem internal
callers, and `MockStorageBackend`/tests — all updated as part of this work. The
fail-closed fix (R9) updates `services/get_delta_proposals.rs`. Asserted by an
integration test showing service-layer reads are byte-identical with a key
configured vs not (SC-004) and an error-propagation test (FR-010).

**II. Transport and Cross-Language Parity** — PASS / not applicable. No HTTP or
gRPC surface changes; Rust and TypeScript clients are untouched. No intentional
divergence introduced.

**III. Append-Only Integrity and Explicit Lifecycles** — PASS, and directly
reinforced. The feature introduces **no implicit fallback**: encryption mode is
an explicit deployment configuration; a missing/invalid key fails startup
(FR-009); an undecryptable or unexpectedly-plaintext payload is an explicit read
error (FR-010, FR-014), never a silent degrade to plaintext. Lifecycle states
(pending/candidate/canonical/discarded) and append-only records are unchanged —
only the payload field's at-rest representation changes.

**IV. Explicit Authentication and Stable Boundary Errors** — PASS. Decryption
failure surfaces through the existing `Result<_, String>` storage error path and
maps to the same boundary error the service layer already returns for storage
failures (no new HTTP/gRPC error code, no behavior drift). The changed layer
plus one upstream consumer (the read service path / dashboard read) receive
updated tests per the principle.

**V. Evidence-Driven Delivery** — PASS. The spec defines independently testable
user stories; this plan defines a targeted validation plan (cipher unit tests,
decorator roundtrip/tamper/AAD tests, parity test on/off, startup fail-fast
test) and the doc updates (PRODUCTION.md, CONFIGURATION.md,
runbooks/secrets.md). Storage is a high-risk area, so validation lands in the
changed layer and one upstream consumer before completion.

**System Invariants** — Preserved. Filesystem and Postgres keep identical
externally observable semantics *by construction*, because the cipher decorator
wraps both uniformly above the backend boundary. Filesystem remains the dev/test
default (encryption off by default), satisfying the local-dev invariant.

**Result: no violations. Complexity Tracking left empty.**

## Project Structure

### Documentation (this feature)

```text
speckit/features/001-storage-encryption/
├── plan.md              # This file
├── spec.md              # Feature specification
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output — internal Rust trait + envelope contracts
│   ├── storage-cipher.md
│   └── envelope.schema.json
└── checklists/
    └── requirements.md  # Spec quality checklist (already created)
```

### Source Code (repository root)

```text
crates/server/src/
├── storage/
│   ├── mod.rs                    # StorageBackend trait (proposal reads → ProposalRecord, R8)
│   ├── filesystem.rs             # FilesystemService (unchanged)
│   ├── postgres.rs               # PostgresService (unchanged)
│   └── encryption/               # NEW module
│       ├── mod.rs                # EncryptedStorage decorator (impl StorageBackend)
│       ├── cipher.rs             # StorageCipher trait + Aes256GcmCipher
│       ├── envelope.rs           # Envelope (serde), record-identity AAD
│       ├── key_provider.rs       # StorageKeyProvider trait + Env + SecretsManager impls
│       └── marker.rs             # encryption-state marker read/verify/write (FR-015)
├── builder/
│   └── storage.rs                # resolve key + verify marker + wrap backend
├── services/
│   ├── get_delta_proposals.rs    # CHANGE: propagate error (R9); ProposalRecord (R8)
│   └── push_delta_proposal.rs    # CHANGE: ProposalRecord (R8)
├── evm/service.rs                # CHANGE: ProposalRecord (R8)
├── secret/                       # EXISTING hygiene wrappers (FixedKey<32>, SecretBytes) — reused
├── ack/
│   └── secrets_manager.rs        # EXISTING AwsSecretsManagerProvider pattern — reused/shared
└── testing/
    └── mocks.rs                  # MockStorageBackend — proposal-read return type (R8)

crates/server/migrations/        # NEW additive single-row marker table (Postgres; FR-015)

crates/server/Cargo.toml          # add aes-gcm, base64 (if absent)
docs/PRODUCTION.md                # enable-in-prod guidance + checklist item
docs/CONFIGURATION.md             # GUARDIAN_STORAGE_ENCRYPTION_* env vars
docs/runbooks/secrets.md          # storage key bootstrap/rotation runbook
infra/                            # (follow-up) Secrets Manager secret + IAM for the key
```

**Structure Decision**: Single server crate. The bulk of new code lives in a new
`crates/server/src/storage/encryption/` module plus a wiring change in
`builder/storage.rs`. Two `StorageBackend` proposal-read methods change return
type to `ProposalRecord` (R8), propagating to a handful of in-crate consumers
(services, evm, mock). A single additive marker table (Postgres) / marker file
(filesystem) is the only schema change; existing payload tables are untouched. No
changes outside `crates/server` (client crates, SDKs, examples) — the wire
contract is unchanged, confirmed by Constitution Check I.

## Phase 0 — Research

See [research.md](./research.md). Resolves: AEAD algorithm and crate choice;
envelope format and plaintext-detection; key-provider design (dev env key vs
Secrets Manager) and reuse of ACK plumbing; decorator method-dispatch subtleties
(default-method self-dispatch, `pull_states_batch` override); the AAD record
identity for proposals (commitment, not nonce); rollout via the 0.15 cutover;
and the deferred KMS-wrap/enclave path.

## Phase 1 — Design & Contracts

- [data-model.md](./data-model.md) — Envelope, RecordIdentity/AAD, StorageKey,
  StorageKeyProvider, and the encrypt/decrypt mapping over `StateObject` /
  `DeltaObject` / `ProposalRecord`.
- [contracts/storage-cipher.md](./contracts/storage-cipher.md) — the
  `StorageCipher` and `StorageKeyProvider` Rust trait contracts and the
  decorator's per-method behavior (encrypt / decrypt / pass-through).
- [contracts/envelope.schema.json](./contracts/envelope.schema.json) — the
  on-disk/in-column envelope JSON shape.
- [quickstart.md](./quickstart.md) — how to enable encryption in dev (base64 key)
  and prod (Secrets Manager), generate a key, and verify ciphertext at rest.

This feature has **no external API contract** (no new HTTP/gRPC endpoint); the
"contracts" are the internal storage-layer trait and envelope format that the
implementation must honor.

## Complexity Tracking

> No constitution violations — section intentionally empty.
