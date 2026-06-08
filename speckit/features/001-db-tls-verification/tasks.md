---
description: "Task list for 001-db-tls-verification"
---

# Tasks: Standards-Based Database TLS Certificate Verification

**Input**: Design documents from `speckit/features/001-db-tls-verification/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/tls-verification-contract.md

**Tests**: INCLUDED — the spec mandates parser/resolver tests, cross-stack hostname
tests, redaction test (FR-005a), parsing edge-case tests, and P3 plaintext/
encrypt-only tests on both stacks; Constitution Principle V requires validation
for this high-risk (auth/transport) change.

**Feature gate**: All Postgres code lives under `--features postgres`. Run tests
with `cargo test -p guardian-server --features postgres`.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files / independent, no incomplete deps)
- **[Story]**: US1 / US2 / US3 (setup, foundational, polish have no story label)

## Path Conventions

- Server crate: `crates/server/src/...`, `crates/server/Cargo.toml`
- Infra: `infra/*.tf`
- Docs: `docs/...`
- Unit tests: inline `#[cfg(test)]` module in `crates/server/src/storage/postgres.rs`
  (matching repo convention); live-TLS legs are `#[ignore]` integration tests.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Dependencies needed before any implementation.

- [X] T001 Add `rustls-pemfile = "2"` to the `postgres` feature dependency set in `crates/server/Cargo.toml` (add to the `[dependencies]` as `optional = true` and to the `postgres = [...]` feature list); run `cargo build -p guardian-server --features postgres` to confirm it resolves.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared parsing/verification mechanism used by ALL user stories. No
story can be completed until these exist. **Do the rustls-API spike (T002) first
— it is the single highest-risk item (research D4).**

- [X] T002 [P] Spike + implement the chain-only (hostname-skipping) `ServerCertVerifier` for `verify-ca` in `crates/server/src/storage/postgres.rs`: confirm the exact `rustls 0.23.37` API (delegate chain + signature validation to a `WebPkiServerVerifier` built from a `RootCertStore`, tolerating only the hostname-mismatch error variant), with a unit test proving chain validation still fails on an untrusted issuer. Resolves research D4.
- [X] T003 [P] Implement the CA bundle loader in `crates/server/src/storage/postgres.rs` using `rustls_pemfile::certs`: load the `sslrootcert` file into a `rustls::RootCertStore`, **collecting/propagating every parse error** (`collect::<Result<Vec<_>,_>>()?`, never `filter_map(Result::ok)`); reject an empty result and any partially-malformed bundle (FR-005). Supports multiple roots in one PEM (combined bundle). Unit test: a bundle with one malformed entry is rejected.
- [X] T004 Implement the connection-string parser + `ParsedConnectionConfig` type in `crates/server/src/storage/postgres.rs` using the `url` crate: extract `sslmode`/`sslrootcert`; apply the deterministic parsing rules (reject duplicate `sslmode`/`sslrootcert`, empty `sslrootcert=`, libpq keyword/value DSN, unsupported scheme, multi-host; percent-decode the cert path). Pure function, no I/O.
- [X] T005 Implement the effective-level resolver (`raw (sslmode, sslrootcert) → TlsVerificationLevel`) in `crates/server/src/storage/postgres.rs` per data-model.md: absent→`disable`; `require`+no-rootcert→EncryptOnly; `require`+rootcert→VerifyCa (libpq promotion); `verify-ca`/`verify-full`+rootcert→matching level; reject `allow`/`prefer`/`system`/unknown/verify-without-rootcert with actionable, **credential-redacted** errors (FR-001a/b/c, FR-003a, FR-005a).
- [X] T006 Implement the three URL derivations in `crates/server/src/storage/postgres.rs`: `normalized_sync_url` (inject `sslmode=disable` when absent; ensure an explicit `sslrootcert` is present for verifying modes so libpq never uses `~/.postgresql/root.crt`) and `sanitized_async_url` (strip `sslrootcert` + non-Disable/Prefer/Require `sslmode`; set `sslmode=require` to force TLS). Resolves FR-007a / research D12.
- [X] T007 Add the shared **preflight** entry point (`pub(crate)`) in `crates/server/src/storage/postgres.rs` that runs T004+T005 (+T003 when a verifying mode resolves) and returns the resolved config or a hard error; call it in `crates/server/src/builder/storage.rs` in the `#[cfg(feature = "postgres")]` build path **before `postgres::run_migrations`** (currently line 117), so rejection happens before any connection. Pass `normalized_sync_url` to `run_migrations`.

**Checkpoint**: Parsing, resolution, CA loading, verifier construction, and the
pre-migration gate all exist and are unit-tested. Verifying behavior is not yet
wired into the async pool (US1).

---

## Phase 3: User Story 1 - Authenticated TLS to a managed Postgres provider (P1) 🎯 MVP

**Goal**: A verifying connection (`verify-ca`/`verify-full`) authenticates the
database; forged/untrusted/expired/hostname-mismatched certs are refused.

**Independent test**: Against a local TLS Postgres with a known CA, `verify-full`
starts + runs migrations + builds the pool; an untrusted/hostname-mismatched cert
is refused with a clear error.

- [X] T008 [US1] Build the `rustls::ClientConfig` for each verifying level in `crates/server/src/storage/postgres.rs`: `verify-full` → default `ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth()` (chain+hostname); `verify-ca` → the T002 chain-only verifier; construct once and wrap in `Arc`.
- [X] T009 [US1] Replace `establish_tls_connection`/`postgres_connection_manager` in `crates/server/src/storage/postgres.rs` so the `ManagerConfig::custom_setup` closure captures the `Arc<ClientConfig>` (clone per connection, not rebuild) and connects with `sanitized_async_url`; route `disable` to the plain manager (no `custom_setup`).
- [X] T010 [US1] Remove `NoCertificateVerification` from every verifying path: keep a no-verify verifier reachable ONLY for EncryptOnly (`require` w/o rootcert) and assert (in code structure + a test) it is unreachable from `verify-ca`/`verify-full` (FR-004).
- [X] T011 [P] [US1] Unit tests in `crates/server/src/storage/postgres.rs`: every behavior-matrix row (contracts/tls-verification-contract.md) maps raw input → expected level or rejection, including the `require`+rootcert promotion.
- [X] T012 [P] [US1] Redaction test in `crates/server/src/storage/postgres.rs`: a forced verifying-mode failure (bad CA path) produces an error/log containing no password / secret query params (FR-005a).
- [X] T013 [US1] Live-TLS integration test (`#[ignore]`, documented) in `crates/server/src/storage/postgres.rs`: against a local TLS Postgres, assert `verify-full` success, untrusted-CA refusal, expired-cert refusal; `verify-ca` chain success.
- [X] T014 [P] [US1] Hostname matrix integration tests (`#[ignore]`) in `crates/server/src/storage/postgres.rs` (FR-002a): `verify-full` accepts a DNS-SAN match and an IP-SAN match, refuses a SAN mismatch, refuses a CN-only cert; `verify-ca` accepts despite hostname mismatch.

**Checkpoint**: US1 is independently demonstrable on a local TLS Postgres.

---

## Phase 4: User Story 2 - Provider-agnostic trust configuration (P2)

**Goal**: The same explicit-CA-file model works across providers (incl. a combined
multi-root bundle), with no provider-specific code, and the AWS reference is moved
to authenticated TLS.

**Independent test**: A single combined CA bundle validates two endpoints with
different roots; `sslrootcert=system` is rejected with a clear message.

- [X] T015 [P] [US2] Multi-root bundle test in `crates/server/src/storage/postgres.rs`: a single PEM containing two distinct CA roots loads into the `RootCertStore` and validates certs issued by either (covers the combined RDS + Amazon Trust Services case, FR-009a) — no provider-specific branch.
- [X] T016 [P] [US2] `sslrootcert=system` rejection test in `crates/server/src/storage/postgres.rs`: asserts fail-fast with the actionable "use an explicit CA bundle file" message (FR-003a).
- [X] T017 [US2] Update `infra/data.tf` (`database_url`, line ~137) to `...&sslmode=verify-full&sslrootcert=<fixed-container-path>` for the managed deployment (FR-009).
- [X] T018 [US2] Update `infra/ecs.tf` to mount the combined CA bundle (Amazon RDS roots + Amazon Trust Services roots) into the task container at the fixed path **at deploy time** (no baked image, no app download — FR-009b); document the path as the single source for `sslrootcert`.
- [X] T019 [P] [US2] Document multi-provider trust configuration in `docs/CONFIGURATION.md`: the `sslmode` ladder, `sslrootcert=<path>`, the `require`+rootcert promotion, and explicit examples for AWS RDS (combined bundle + RDS Proxy note), one other managed provider, and local docker compose (FR-008/FR-010).

**Checkpoint**: Provider-agnostic trust config works; AWS reference is authenticated.

---

## Phase 5: User Story 3 - Local and encrypt-only deployments keep working (P3)

**Goal**: Plaintext local and encrypt-only deployments need zero new config, and
behave identically on the migration and pool paths.

**Independent test**: Omitted `sslmode` and `sslmode=disable` connect plaintext;
`require` (no rootcert) encrypts without verification; `require` refuses a non-TLS
server — all on both stacks.

- [X] T020 [P] [US3] Tests in `crates/server/src/storage/postgres.rs` for the non-verifying ladder: absent `sslmode` → plaintext (normalized to `disable`); `disable` → plaintext; `require` w/o rootcert → EncryptOnly resolution (FR-006).
- [X] T021 [US3] Cross-stack parity test in `crates/server/src/storage/postgres.rs`: for scenarios in T020, assert `normalized_sync_url` (libpq inputs) and `sanitized_async_url` + resolved level produce the SAME effective behavior (FR-007). Pure/string-level where possible to avoid live deps.
- [ ] T022 [P] [US3] Live `#[ignore]` test: `require` (no rootcert) against a non-TLS server is refused (no silent plaintext downgrade) on the async path, in `crates/server/src/storage/postgres.rs`.
- [X] T023 [P] [US3] Confirm the existing `detects_sslmode_require` / `ignores_non_tls_database_urls` tests in `crates/server/src/storage/postgres.rs` are updated/replaced to reflect full parsing (no substring check) and still pass.

**Checkpoint**: All three stories complete and independently testable.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [X] T024 [P] Add the verification-failure → cause mapping to `docs/TROUBLESHOOTING.md` (sslmode error, sslrootcert error, cert-verification refusal, verify-ca vs verify-full hostname) per FR-010.
- [X] T025 [P] Add local TLS verification instructions (generate CA/cert, `verify-ca` vs `verify-full`) to `docs/LOCAL_DEV.md`, mirroring quickstart.md.
- [X] T026 [P] Document the AWS combined-bundle delivery + **CA rotation procedure** (replace mounted bundle + redeploy; new roots present before tightening) in `docs/SERVER_AWS_DEPLOY.md` and/or `docs/runbooks/secrets.md`, including the RDS Proxy (ACM/Amazon Trust Services) caveat.
- [X] T027 Document the deployment sequencing for FR-009/FR-001c: mount the combined bundle and confirm it is readable BEFORE switching `data.tf` to `verify-full` (avoid breaking ECS→RDS startup).
- [X] T028 Run `cargo fmt`, `cargo clippy -p guardian-server --features postgres -- -D warnings`, and `cargo test -p guardian-server --features postgres`; fix findings. NOTE: the TLS code (and its tests) is clippy-clean. `cargo clippy --all-targets -- -D warnings` additionally surfaces 3 `field_reassign_with_default` warnings in `crates/server/src/delta_object.rs` test code — pre-existing on `main`, unrelated to this branch, and intentionally not fixed here (out of scope). Flag in the PR description.
- [ ] T029 Execute the documented manual smoke (AGENTS.md §6): AWS RDS via the **RDS Proxy endpoint** (proxy enabled AND disabled) + one other managed provider, using the combined bundle — `verify-full` success + a deliberately wrong-CA refusal; record results.

---

## Dependencies & Execution Order

- **Setup (T001)** → blocks everything (dependency must resolve).
- **Foundational (T002–T007)** → blocks all user stories. T002 first (risk). T003/T004 are [P]; T005 depends on T004; T006 depends on T004/T005; T007 depends on T004–T006 (and T003 for verifying-mode loading).
- **US1 (T008–T014)** depends on Foundational. T008→T009→T010 sequential (same file, same code path); T011/T012/T014 [P] tests; T013 after T008–T010.
- **US2 (T015–T019)** depends on Foundational (+US1 for live validation of the verifier). T017/T018 (infra) are independent of the Rust tests; T015/T016/T019 [P].
- **US3 (T020–T023)** depends on Foundational. Mostly [P] tests; T021 after T020.
- **Polish (T024–T029)** after the stories it documents/validates; T024/T025/T026 [P]; T028 before T029.

## Implementation Strategy

- **MVP = Phase 1 + Phase 2 + Phase 3 (US1)**: delivers the actual security fix —
  authenticated `verify-ca`/`verify-full` with fail-closed errors and the
  pre-migration gate. Independently testable on a local TLS Postgres.
- **Increment 2 (US2)**: provider-agnostic combined-bundle trust + the AWS
  reference deployment switch to `verify-full`.
- **Increment 3 (US3)**: explicit regression coverage that local/encrypt-only
  deployments are unchanged and both stacks agree.
- **Polish**: docs, rotation runbook, lint/test, manual smoke.

## Parallel Opportunities

- Foundational: T002 ∥ T003 (different concerns, same file — coordinate edits).
- US1 tests: T011 ∥ T012 ∥ T014.
- US2: T015 ∥ T016 ∥ T019; infra T017/T018 ∥ the Rust work.
- US3: T020 ∥ T022 ∥ T023.
- Docs: T024 ∥ T025 ∥ T026.

> Note: many tasks touch the single file `crates/server/src/storage/postgres.rs`;
> `[P]` there means logically independent (separate functions/tests), but edits
> must be serialized at the file level to avoid conflicts.

## Task count

29 tasks — Setup 1, Foundational 6, US1 7, US2 5, US3 4, Polish 6.
