# Tasks: Guardian Proves and Commits Transactions

**Feature Key**: `254-guardian-prove-and-commit`
**Spec**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md) | **Data model**: [data-model.md](./data-model.md)
**Contracts**: [contracts/execution-api.md](./contracts/execution-api.md) | [contracts/sdk-api.md](./contracts/sdk-api.md)
**Generated**: 2026-07-28

## Format

`- [ ] [TaskID] [P?] [Story?] Description with file path`

`[P]` = parallelizable (different files, no dependency on an incomplete task).

## What is already done

**The proving architecture is ratified — do not re-plan it.** The Gate 0 spike landed
`crates/server/src/network/miden/execution/` (a `DataStore` over Guardian's own state,
`PartialBlockchain` assembly from node RPC) and proved a witness through the remote prover
against public testnet — with **no new dependencies**. Seven offline tests and three live
tests exist and pass.

**Gate 0 is narrowed, not fully passed.** P2ID and configuration executed and proved under
`MockChain`; `consume_notes` ran only as prepared execution; the custom family (#266) is
unrun; live submission is unvalidated. That residue is deferred coverage tracked in
`validation-matrix.md` — it cannot falsify the architecture, but do not restate it as
"all four families validated".

These tasks are the **lifecycle machinery** around it. Every task below runs
in-process with **no node required**, except the already-written live tests and
the deferred Stage 2 submission check.

## Two deliberate deviations from strict story-by-story ordering

Both are forced by the spec, and stating them here beats discovering them mid-phase:

1. **Foundational is heavy.** FR-023 requires a durable reservation for the
   *whole* span from acceptance to terminal state, so US1 cannot execute a single
   proposal without the reservation and its admission primitive. That machinery is
   therefore Phase 2, not US4. US4 keeps what is genuinely separable: the
   concurrency behaviors, lease expiry, recovery, and fault injection **on top of**
   that foundation.

2. **US1 implements FR-045 steps 1–2; US2 hardens them.** The signature-subset
   selection (FR-006) and binding check (FR-007) are steps 1 and 2 of the
   execution sequence, so the US1 happy path structurally requires them to exist
   and pass. US2's phase owns the **negative paths**: every mismatch caught,
   distinguishable by error code, with no side effects. The spec itself says US2
   "is not separable from US1 in value" — this split respects that while keeping
   each phase independently testable.

## Test placement convention

`crates/server` has **no `tests/` directory** — all 54 test-bearing files use
inline `#[cfg(test)]` modules colocated with the code, and larger test bodies get
a sibling file declared as `#[cfg(test)] mod`, exactly as the proving spike did
(`network/miden/execution/{mod,tests,live_tests}.rs`). Tasks below follow that,
which is why `execute_proposal` is a directory module. Postgres-dependent tests
are gated `#[ignore]` and read `DATABASE_URL`, matching the existing convention.

**Tests are in scope**, not optional: Constitution Principle V, 39 success
criteria, and the fault-injection rows in
[validation-matrix.md](./validation-matrix.md) all require them. The fault
injection in Phase 5 is the substance of the feature's safety argument.

---

## Phase 1: Setup (Shared Infrastructure)

- [ ] T001 Create migration `crates/server/migrations/2026-07-28-000001_execution_reservations/{up.sql,down.sql}` with the three tables, the `execution_reservations_one_active_per_account` partial unique index, the `execution_submissions_account_nonce` index, and the `UNIQUE (account_id, proposal_id, attempt)` constraints exactly as specified in `data-model.md` § Migration
- [ ] T002 [P] Add Diesel table definitions for `execution_reservations`, `execution_submissions`, and `execution_outcomes` to `crates/server/src/schema.rs`
- [ ] T003 [P] Scaffold empty modules with `pub use` re-exports and wire each into its parent `mod.rs`: `crates/server/src/services/ack_delta_internal.rs`, `crates/server/src/services/execute_proposal/mod.rs`, `crates/server/src/services/execution_status.rs`, `crates/server/src/jobs/execution_reconcile/mod.rs`. `execute_proposal` is a **directory module** so its test bodies live in sibling files, matching `network/miden/execution/{mod,tests,live_tests}.rs`
- [ ] T004 [P] Add the execution config block in `crates/server/src/config/` for the eight variables in `contracts/execution-api.md` § Configuration (`GUARDIAN_TX_PROVER_URL` — typed as `CredentialUrl` (`crates/server/src/secret/wrappers.rs:126`) so credentials embedded in the URL are redacted in `Debug`, matching `evm/config.rs`'s `rpc_url` — `GUARDIAN_TX_PROVER_TIMEOUT_SECS`, `GUARDIAN_PROVING_ENABLED`, `GUARDIAN_MAX_PROPOSAL_REQUEST_BYTES`, `GUARDIAN_MAX_ACCOUNT_REQUEST_BYTES`, `GUARDIAN_EXECUTION_LEASE_SECS`, `GUARDIAN_EXECUTION_RECONCILE_INTERVAL_SECS`, `GUARDIAN_EXECUTION_EXPIRATION_HORIZON_BLOCKS`), with unit tests for defaults. `GUARDIAN_TX_PROVER_TIMEOUT_SECS` MUST default well above the client library's 10 s (FR-020)

---

## Phase 2: Foundational (BLOCKING — must complete before any user story)

**Gate**: T024 and T025 must pass before any phase-3 task starts. If the
admission primitive is wrong, every later phase inherits a concurrency bug that
integration tests surface only intermittently.

- [ ] T005 Add `ExecutionReservation`, `SubmissionEvidence`, and `ExecutionOutcome` types to `crates/server/src/storage/mod.rs` per `data-model.md`, including **all four mandatory FR-039 evidence fields** — `transaction_id`, `expected_commitment`, `reference_block`, `expiration_block` — plus `base_commitment` and `attempt`. `expiration_block` MUST come from `ProvenTransaction::expiration_block_num()` and **never** from `TransactionRequest::expiration_delta`. `expected_commitment` is what makes FR-040's landed-vs-superseded distinction possible at all
- [ ] T006 Add exhaustive outcome enums `ReservationWrite { Created, AlreadyReserved { holder_id, proposal_id }, CandidateExists, StaleLease, ClaimSuperseded }` and `AdmissionWrite { Admitted, NotAuthorized, CandidateExists, StaleLease }`, and `ResolveWrite { Resolved, NotAuthorized, AlreadyResolved, StaleLease }` to `crates/server/src/storage/mod.rs`, mirroring `CanonicalWrite` (`storage/mod.rs:157`), and add the `ProtectedByExecution` variant to `CanonicalWrite` itself. No catch-all variant — a new case must break every match site, which is how every existing discard call site gets forced to handle protection
- [ ] T007 Declare the new `StorageBackend` methods in the canonicalization-writes block of `crates/server/src/storage/mod.rs` (after `update_candidate_status`, ~line 571) **with no default bodies**, extending the existing comment at `storage/mod.rs:509-516`: `create_execution_reservation`, `renew_execution_reservation`, `release_execution_reservation`, `claim_execution_reservation` (FR-052 compare-and-set ownership transfer), `load_execution_reservation`, `admit_execution_candidate`, `resolve_execution` (the post-boundary failure resolution — discard, outcome, release, in one transaction), `fail_execution` (**pre-boundary failures only** — upserts the outcome and releases the reservation as one commit; `landed` is written by the extended `promote_candidate` and post-boundary failures by `resolve_execution`), `list_unresolved_submissions`. There is deliberately no operation that writes an outcome without also releasing (FR-053)
- [ ] T008 Add per-account execution-lease acquisition in `crates/server/src/coordination/mod.rs` and `crates/server/src/coordination/postgres/lease.rs`: an `execution_lease_name(account_id) -> String` helper producing `execution:{account_id}`, plus acquire/renew through the existing lease machinery. Add a unit test asserting the name is never equal to `CANONICALIZATION_LEASE` (FR-038)
- [ ] T009 Implement the reservation methods on `PostgresService` in `crates/server/src/storage/postgres.rs`, each as **one transaction**: `lock_account_metadata` → `lease_fence_is_current` → conditional write, returning `unfenced_write_error("<op>")` when the fence is absent, following `discard_candidate` (`postgres.rs:1652`) exactly. The fence MUST be the **account-scoped** `execution:{account_id}` lease, never `CANONICALIZATION_LEASE` — that name admits one holder cluster-wide (`coordination/postgres/lease.rs:59`) and would serialize every account in the deployment
- [ ] T010 Implement the reservation methods on `FilesystemService` in `crates/server/src/storage/filesystem.rs` as read-modify-write under **`delta_write_lock`** (`filesystem.rs:25`) — not a new mutex, since admission must be atomic with respect to the candidate writes that lock serializes. Fence parameters are ignored, consistent with `filesystem.rs:783`/`:793`
- [ ] T011 [P] Add pass-through implementations of the new methods to `EncryptedStorage` in `crates/server/src/storage/encryption/decorator.rs`
- [ ] T012 [P] Add pass-through implementations with metric instrumentation to `InstrumentedStorage` in `crates/server/src/metrics/storage.rs`
- [ ] T013 [P] Add implementations to `MockStorageBackend` in `crates/server/src/testing/mocks.rs`, with settable canned outcomes so service-layer tests can drive every `ReservationWrite` / `AdmissionWrite` variant
- [ ] T014 Implement `admit_execution_candidate` as the FR-045 step 9 commit in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: persist the candidate, set `has_pending_candidate`, set `candidate_nonce`, and insert `SubmissionEvidence` **in one atomic unit**. Returns `AdmissionWrite`
- [ ] T015 Add the **owner-authorized admission exception** (FR-037) to the admission path in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: admission succeeds for the caller presenting the matching reservation's `holder_id` and a valid fence, and is refused as `NotAuthorized` for every other caller. Without this, Guardian deadlocks admitting its own candidate
- [ ] T016 Implement `claim_execution_reservation` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` as a **compare-and-set** on `holder_id` + `fence_token` of a live reservation, returning `ClaimSuperseded` rather than stealing on a stale expectation, and **never** releasing as part of the handover (FR-052, FR-028)
- [ ] T017 Implement `resolve_execution` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: the post-boundary failure resolution, which validates execution ownership and fence, then discards the candidate, **deletes its matching proposal**, upserts `ExecutionOutcome`, and releases the reservation **in one transaction**, returning `ResolveWrite`. Proposal deletion is part of this commit, not a follow-up step as in `remove_candidate` (`processor.rs:1092-1130`) — a surviving proposal reports `proposal_exists: true`, which FR-042 defines as a permitted retry, so a definitely-rejected transaction would be advertised as retryable. This is the **only** operation permitted to discard a boundary-crossed candidate (FR-053, SC-025, SC-037)
- [ ] T018 Extend the fenced `promote_candidate` primitive in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` to upsert `ExecutionOutcome { state: landed }` and release the reservation **inside its own transaction**, taking the same per-account lock every other reservation write takes. Promotion is explicitly authorized to release while holding the **canonicalization** fence rather than the execution fence (FR-054) — it cannot be expected to hold a lease belonging to a worker that may have died. This primitive must exist before Phase 3's T050 can use it
- [ ] T019 Implement `fail_execution` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: upsert `ExecutionOutcome` and release the reservation as **one commit** for pre-boundary failures. Two operations would let a crash between them expose either a terminal execution still holding its account or a released account with no durable outcome (FR-053, SC-036)
- [ ] T020 Make the **canonicalization worker's** `discard_candidate` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` consult `execution_submissions` for the account and nonce inside its own transaction, returning the new `CanonicalWrite::ProtectedByExecution` for a candidate whose execution has crossed the boundary and not yet resolved. Between step 9 and step 11 the candidate looks ordinary to canonicalization, which would otherwise leave a submitted transaction with no candidate to promote. This MUST NOT gate `resolve_execution` (T017) — the protection is scoped to the caller, not the row, or the account becomes permanently unresolvable (FR-049)
- [ ] T021 Extend `submit_candidate` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` with the reservation predicate: reservation creation fails when a candidate exists (`CandidateExists`), and non-owner candidate admission fails when a reservation is active — decided inside the **same** lock/transaction, never as a separate check-then-act (FR-037)
- [ ] T022 Add the FR-022 synchronous refusal codes and FR-024 asynchronous failure codes to `crates/server/src/error.rs` per `contracts/execution-api.md` § Error codes, each with its HTTP status and gRPC status mapping from the parity table
- [ ] T023 [P] Add state-read authorization tests in `crates/server/src/services/execution_status.rs`: a cosigner of the account may read execution state; a non-cosigner is refused with `GUARDIAN_AUTHENTICATION_FAILED` on all three endpoints (FR-004)
- [ ] T024 Add storage-layer admission tests in `crates/server/src/storage/execution_reservation_tests.rs`, declared as a `#[cfg(test)] mod` from `crates/server/src/storage/mod.rs`, covering, on **both** backends: reservation created when clean; refused when a candidate exists; refused when a reservation is already active with `AlreadyReserved.proposal_id` naming the blocker; owner-authorized admission succeeds; non-owner admission returns `NotAuthorized`; candidate and evidence are both present or both absent after admission
- [ ] T025 Add Postgres-only concurrency tests in `crates/server/src/storage/execution_reservation_tests.rs` (gated `#[ignore]`, `DATABASE_URL`, matching the existing Postgres test convention): two concurrent `create_execution_reservation` calls for one account yield exactly one `Created` and one `AlreadyReserved`; a stale fence yields `StaleLease` and writes nothing; the partial unique index rejects a second active reservation even when the service-layer check is bypassed
- [ ] T026 [P] Add a promotion-versus-resolution race test in `crates/server/src/storage/execution_reservation_tests.rs` (Postgres, `#[ignore]`): `promote_candidate` and `resolve_execution` issued concurrently for one account produce exactly one terminal outcome and one release, with the loser observing `AlreadyResolved` and writing nothing (SC-038, FR-054)
- [ ] T027 [P] Add terminal-atomicity fault injection in `crates/server/src/storage/execution_reservation_tests.rs`: for each of the three terminal operations, a crash injected mid-transaction leaves neither a terminal execution holding a reservation nor a released reservation without an outcome (SC-036, FR-053)
- [ ] T028 [P] Add a per-account lease test in `crates/server/src/storage/execution_reservation_tests.rs`: two accounts hold execution leases simultaneously on one replica, while two executions for the same account serialize. Written so it **fails** if `CANONICALIZATION_LEASE` is substituted (SC-034)
- [ ] T029 [P] Add an ownership-transfer test in `crates/server/src/storage/execution_reservation_tests.rs`: a reconciliation owner claims a live reservation by compare-and-set; a claim with a stale holder or fence token returns `ClaimSuperseded` and writes nothing; the reservation is never observed released mid-handover (SC-035)
- [ ] T030 Add startup validation in `crates/server/src/builder/` (FR-043): refuse to start with execution enabled while canonicalization is disabled or the server runs in optimistic delta-commit mode, naming the misconfiguration. FR-040 depends on canonicalization entirely
- [ ] T031 Implement the transaction-request envelope codec in `crates/server/src/services/` (new `execution_codec.rs`): verify `checksum` **before** any deserialization attempt, check `format_version` and `protocol_line`, enforce the FR-016 size caps, and map failures to `GUARDIAN_EXECUTION_REQUEST_CODEC` / `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH`, with unit tests for each rejection path
- [ ] T032 Add `serializer_id` to the envelope codec in `crates/server/src/services/execution_codec.rs`: the exact `miden-protocol` version **including prerelease**, checked against a configured allowlist. A matching `protocol_line` with a mismatched `serializer_id` MUST be refused as `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` — `MAJOR.MINOR` alone treats every 0.16 alpha as compatible, and upstream prereleases have changed serialization (FR-015)

---

## Phase 3: User Story 1 — Hand a threshold-met proposal to Guardian (P1) 🎯 MVP

**Story goal**: A cosigner hands Guardian a threshold-met proposal; Guardian
verifies, proves, submits, and the account advances exactly as under
self-execution.

**Independent test**: Create and sign a proposal to threshold, call execute as a
cosigner, poll to a terminal state, and confirm the resulting delta moved through
the normal candidate → canonical lifecycle with no new status values.

### Tests for User Story 1

- [ ] T033 [P] [US1] Add integration test `crates/server/src/services/execute_proposal/tests.rs`: threshold-met Guardian-executable proposal → execute → poll to `landed`, asserting the delta canonicalized through the existing lifecycle and that **no new delta status value** appears (US1 scenarios 1–3)
- [ ] T034 [P] [US1] Add test in `crates/server/src/services/execute_proposal/tests.rs` asserting the reported state progression is one of the permitted transitions only (`pending → proving → submitted → landed`) and never absent or ambiguous (US1 scenario 2, FR-024)
- [ ] T035 [P] [US1] Add test in `crates/server/src/services/execute_proposal/tests.rs`: a below-threshold proposal is refused synchronously with `GUARDIAN_PROPOSAL_NOT_READY`, and a non-cosigner caller with `GUARDIAN_AUTHENTICATION_FAILED` — neither creating a reservation nor an execution record (US1 scenarios 4–5, FR-022)
- [ ] T036 [P] [US1] Add execution-not-found tests in `crates/server/src/services/execution_status.rs`: a status read for a proposal that exists but has never been executed returns `404` / `GUARDIAN_EXECUTION_NOT_FOUND`, distinct from `GUARDIAN_PROPOSAL_NOT_FOUND`, while `GET /delta/execution/current` still answers `200 {"execution": null}` for the same account (SC-039)

### Implementation for User Story 1

- [ ] T037 [US1] Extract the verify-and-acknowledge span of `push_delta` (`crates/server/src/services/push_delta.rs:31-118`) into `crates/server/src/services/ack_delta_internal.rs`, and have `push_delta` call it then commit through the existing `DeltaCommitStrategy` seam (`push_delta.rs:120-134`). Behavior of `push_delta` must be unchanged — its existing tests stay untouched and passing
- [ ] T038 [US1] Make `crates/server/src/services/ack_delta_internal.rs` satisfy FR-044: apply the same delta verification and acknowledgment signing, persist **no** candidate, and do **not** set `has_pending_candidate`. Restrict visibility so no transport can reach it, and add a test in `crates/server/src/builder/handle.rs` asserting the router exposes no route that reaches it
- [ ] T039 [US1] Implement FR-045 steps 1–2 in `crates/server/src/services/execute_proposal/mod.rs`: select the valid signature subset and confirm the effective per-procedure threshold (FR-005, FR-006), then reproduce the transaction and verify the binding to the signed summary (FR-007). Step 2 **must** precede step 3 — never acknowledge a transaction that does not reproduce the signed summary
- [ ] T040 [US1] Implement FR-045 steps 3–6 in `crates/server/src/services/execute_proposal/mod.rs`: issue the acknowledgment via the internal path, inject signature and acknowledgment advice, execute the authorized transaction through `network/miden/execution/`, and re-verify the binding on the executed result
- [ ] T041 [US1] Implement FR-045 step 7 in `crates/server/src/services/execute_proposal/mod.rs`: prove via `RemoteTransactionProver` with the configured explicit timeout (FR-019, FR-020). On error, walk the `std::error::Error::source` chain into the log — the outermost `Display` is only "failed to prove transaction" and discards the gRPC status. Classify each failure transient-versus-permanent and retry transient failures with capped backoff under the held, renewed reservation (FR-055): the transient class MUST match transport-level failures (connection errors, i/o timeouts, deadline-exceeded) from the walked source chain, not just structured prover errors; stop retrying and fail-and-release when the failure is permanent or `ExecutedTransaction`'s expiration block can no longer be met within the FR-046 horizon. Add stub-prover tests injecting both failure families (SC-040)
- [ ] T042 [US1] Implement FR-045 step 8 in `crates/server/src/services/execute_proposal/mod.rs`: re-check admissibility against freshly read state (FR-048) and confirm the expiration is finite and within the horizon (FR-046), reading `ProvenTransaction::expiration_block_num()`. Both are **before** the boundary, so failure here is an ordinary fail-and-release
- [ ] T043 [US1] Implement FR-045 step 9 in `crates/server/src/services/execute_proposal/mod.rs`: validate the fence, then call `admit_execution_candidate` as **one** storage call. This is the no-retry boundary (FR-047) — add an explicit comment that no code path may treat its failure as retryable, and ensure none does
- [ ] T044 [US1] Implement FR-045 steps 10–11 in `crates/server/src/services/execute_proposal/mod.rs`: re-validate the fence and, if stale, abort **without sending and without writing anything** (FR-049); otherwise submit, then classify the result via T045. From step 11 the existing canonicalization lifecycle owns the **promotion** path; a `Definite(rejection)` is resolved by the execution itself via T046, and an `Unknown` is left to reconciliation
- [ ] T045 [US1] Add a typed submission-result adapter in `crates/server/src/services/execute_proposal/submission_result.rs`: map node and transport outcomes to `Definite(rejection)` versus `Unknown`, where **only** an explicit application-level rejection is definite and every ambiguous transport failure (timeout, dropped connection, unavailable) defaults to `Unknown`. Do not branch on free-form error text. Misclassifying an ambiguous failure as definite would discard the candidate for a transaction that actually landed
- [ ] T046 [US1] Consume `Definite(rejection)` in `crates/server/src/services/execute_proposal/mod.rs`: call `resolve_execution` to discard the candidate, persist `failed` / `GUARDIAN_EXECUTION_SUBMISSION_REJECTED`, and release the reservation — one atomic resolution. An `Unknown` result instead leaves the reservation held for reconciliation. This is the FR-032 carve-out that lets a definitely-rejected transaction free its account immediately instead of waiting for expiration
- [ ] T047 [US1] Implement request orchestration in `crates/server/src/services/execute_proposal/mod.rs`: synchronous admission checks → per-account lease acquisition → reservation creation → background dispatch → immediate `202` response, so the FR-022 refusals happen on the caller's thread and everything after step 1 runs off it. Include a restart test asserting a dispatched-but-unstarted execution is recovered or failed, never silently lost
- [ ] T048 [US1] Implement attempt identity across `crates/server/src/storage/postgres.rs`, `crates/server/src/storage/filesystem.rs`, and `crates/server/src/services/execution_status.rs`: rows are keyed `(account_id, proposal_id, attempt)`; the next attempt number is allocated **inside reservation creation, under the account lock** as `max(attempt) + 1` — never in the status layer, where two concurrent retries could compute the same number before either inserted; and an unqualified status read reports the **most recent** attempt. The wire handle stays `(account_id, proposal_id)` (FR-003, FR-042)
- [ ] T049 [US1] Implement reported-state derivation in `crates/server/src/services/execution_status.rs`: derive the five states (FR-024) from the reservation, the delta, and `ExecutionOutcome`, using the internal-phase mapping table in `data-model.md`. Pre-terminal states are derived and never persisted
- [ ] T050 [US1] Wire the `landed` outcome through the extended `promote_candidate` in `crates/server/src/jobs/canonicalization/processor.rs`, using the storage primitive from T018. Promotion is what makes `landed` true, so it persists the outcome and releases the reservation inside its own transaction; `execution_reconcile` MUST NOT also write `landed` on observing `canonical` — two writers for one fact reintroduces the `remove_candidate` race FR-041 exists to prevent. Reconciliation covers only superseded and expired, which are US4 (FR-041, FR-053, SC-025)
- [ ] T051 [US1] Add `POST /delta/proposal/execution` to `crates/server/src/api/http.rs` with `#[utoipa::path]`, per-status responses and `security(...)`: `202` with `newly_accepted: true` on acceptance, `200` with `newly_accepted: false` when idempotently returning an active execution
- [ ] T052 [US1] Add `GET /delta/proposal/execution` and `GET /delta/execution/current` to `crates/server/src/api/http.rs` with `#[utoipa::path]`. The current-execution read returns `200` with `{"execution": null}` when nothing is in flight — **never** `404`; a terminal execution is not in flight (FR-036)
- [ ] T053 [US1] Add the three gRPC methods to `crates/server/proto/guardian.proto` and implement them in `crates/server/src/api/grpc.rs` with semantics identical to HTTP, carrying `newly_accepted`, `proposal_exists`, and `ignored_signatures` on the envelope (FR-034)
- [ ] T054 [US1] Wire the routes in `crates/server/src/builder/handle.rs` following the existing flat-path, query-parameter style (compare `/delta/proposal/single`, `/delta/candidate/abandon`), and derive `ToSchema` / `IntoParams` on the new wire types

**Checkpoint**: MVP. A cosigner can hand Guardian a signed proposal and it lands.
Requires Phases 1–2.

---

## Phase 4: User Story 2 — Execution is bound to what the cosigners actually signed (P1)

**Story goal**: Every mismatch — superseded state, tampered request, insufficient
*valid* signatures — stops execution before any proving or submission, with a
distinguishable cause and no side effects.

**Independent test**: Drive execution against (a) an account advanced past the
proposal's base, (b) a mutated stored request, (c) a bad entry alongside enough
valid signatures (proceeds, records the ignored count), (d) a valid set below
threshold (refused as not-ready). Confirm each outcome and that refusals leave no
trace.

### Tests for User Story 2

- [ ] T055 [P] [US2] Add `crates/server/src/services/execute_proposal/binding_tests.rs`: an account advanced past the proposal's base is refused with `GUARDIAN_EXECUTION_STATE_MISMATCH` before any proving or submission (US2 scenario 1)
- [ ] T056 [P] [US2] Add to `crates/server/src/services/execute_proposal/binding_tests.rs`: a stored request mutated so it no longer reproduces the signed summary is refused with `GUARDIAN_EXECUTION_BINDING_MISMATCH`, and the failure is **distinguishable** from not-ready and state-mismatch (US2 scenario 2)
- [ ] T057 [P] [US2] Add to `crates/server/src/services/execute_proposal/binding_tests.rs`: a proposal carrying an invalid, duplicate, or non-cosigner entry **alongside enough valid signatures** executes, with the bad entry ignored and counted in `ignored_signatures`; and one whose *valid* set is below threshold is refused as `GUARDIAN_PROPOSAL_NOT_READY` — not as a signature or binding error (US2 scenario 3, FR-006)
- [ ] T058 [P] [US2] Add to `crates/server/src/services/execute_proposal/binding_tests.rs`: after **any** refusal, assert the account is not locked, no delta was recorded, no reservation is held, and the proposal is still executable by its cosigners (US2 scenario 4, FR-032)
- [ ] T059 [P] [US2] Add a codec-rejection test to `crates/server/src/services/execute_proposal/binding_tests.rs`: a bad checksum is rejected before deserialization is attempted, and an incompatible `protocol_line` yields `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` (FR-014, FR-015)

### Implementation for User Story 2

- [ ] T060 [US2] Harden signature-subset selection in `crates/server/src/services/execute_proposal/mod.rs`: verify each stored entry against the signed commitment and the registered cosigner set, excluding invalid, duplicate, and non-cosigner entries rather than failing, and surface the count as `ignored_signatures` (FR-006). There is deliberately **no** signature-invalid error code — the only signature-related outcome is an insufficient valid set
- [ ] T061 [US2] Compute the **effective per-procedure** threshold in `crates/server/src/services/execute_proposal/mod.rs` rather than the account-level default (FR-005), and refuse below it synchronously as `GUARDIAN_PROPOSAL_NOT_READY` (FR-022)
- [ ] T062 [US2] Ensure every FR-022 refusal in `crates/server/src/services/execute_proposal/mod.rs` returns **before** any reservation is created or execution record written, and that each maps to a distinct stable code per `contracts/execution-api.md`
- [ ] T063 [US2] Add `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` handling in `crates/server/src/services/execute_proposal/mod.rs`: a proposal with no stored transaction request is refused naming that cause, and Guardian never attempts to rebuild the transaction from semantic parameters (FR-013)
- [ ] T064 [US2] Enforce the FR-016 size caps at proposal creation in `crates/server/src/services/push_delta_proposal.rs`, rejecting an oversized or malformed envelope there rather than at execution time
- [ ] T065 [US2] Implement the FPI refusal in `crates/server/src/services/execute_proposal/mod.rs`: detect that a transaction requires foreign-account inputs from the **typed** request shape, before any execution, and refuse with `GUARDIAN_EXECUTION_FOREIGN_INPUTS_UNSUPPORTED`. Do not branch on `DataStoreError` text (FR-050)
- [ ] T066 [P] [US2] Add an FPI refusal test in `crates/server/src/services/execute_proposal/binding_tests.rs`: a proposal whose transaction requires foreign-account inputs is refused before any execution or proving, with the FPI error code (SC-032)
- [ ] T067 [US2] Define and enforce the FR-016 aggregate account cap in `crates/server/src/services/push_delta_proposal.rs`: account it over **decoded** request bytes (not base64 length), count only proposals in non-terminal states, and enforce the check-plus-insert atomically under the same account lock so concurrent creations cannot both pass
- [ ] T068 [P] [US2] Add a signature-provenance test in `crates/server/src/services/execute_proposal/binding_tests.rs`: signatures Guardian already holds are used directly, while a proposal whose threshold is met only by offline-collected signatures not present server-side is refused as not-ready rather than treated as signed (FR-018)
- [ ] T069 [P] [US2] Add a concurrent aggregate-cap test in `crates/server/src/services/push_delta_proposal.rs`: two proposals created concurrently against an account one slot below the cap result in exactly one acceptance, proving the check-plus-insert is atomic rather than check-then-act (FR-016)
- [ ] T070 [US2] Implement FR-017 stored-request cleanup in `crates/server/src/jobs/canonicalization/processor.rs`: when a proposal is deleted on promotion or discard, its stored transaction request is removed with it, so the aggregate cap cannot be consumed by dead proposals

**Checkpoint**: delegating execution is now safe, not merely convenient.

---

## Phase 5: User Story 4 — One submission, even under concurrency and crashes (P1)

**Story goal**: At most one lease-authorized proving attempt and at most one
on-chain submission, with no account left indefinitely locked.

**Independent test**: Concurrent execution requests from multiple callers and
replicas yield exactly one submission; a replica killed mid-proof releases its
reservation and the account becomes usable; a submission timeout reports
`submitted`, retains the reservation, and refuses retry until the chain is
observed.

### Tests for User Story 4

- [ ] T071 [P] [US4] Add `crates/server/src/services/execute_proposal/concurrency_tests.rs` (Postgres, `#[ignore]`): two concurrent execute requests for one proposal create exactly one reservation, and the second observes the first execution rather than starting a new one (US4 scenario 1, SC-005)
- [ ] T072 [P] [US4] Add to `crates/server/src/services/execute_proposal/concurrency_tests.rs`: two replicas observing the same queued execution — exactly one proves and submits (US4 scenario 2)
- [ ] T073 [P] [US4] Add `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a crash injected **immediately before** the step-9 write fails-and-releases; a crash **immediately after** it reconciles and never retries (SC-024, SC-030). This is the feature's central safety property
- [ ] T074 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: the fence is stolen between step 9 and step 11 — nothing is sent, the durable candidate is left to reconciliation (SC-031, FR-049)
- [ ] T075 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a lease expires mid-flight, ownership transfers, the new owner resolves the outcome, and the original worker resumes without submitting (SC-019, FR-038)
- [ ] T076 [P] [US4] Add the **self-deadlock regression test** to `crates/server/src/services/execute_proposal/concurrency_tests.rs`: Guardian admits its own candidate under its own reservation and the sequence completes without deadlock (SC-028, FR-037/FR-044). This was a real defect in an earlier spec revision
- [ ] T077 [P] [US4] Add to `crates/server/src/services/execute_proposal/concurrency_tests.rs`: while a reservation is active, a client `push_delta` for the same account is refused with `GUARDIAN_EXECUTION_CONFLICT` (US4 scenario 3, FR-027)
- [ ] T078 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a replica killed while proving has its lease expire, the execution reports `failed` with `GUARDIAN_EXECUTION_LEASE_EXPIRED`, the reservation is released, and the proposal remains executable (US4 scenario 4, FR-028)
- [ ] T079 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a submission timeout reports `submitted`, refuses retry, retains the reservation, and resolves only after chain observation (US4 scenario 5, FR-030)
- [ ] T080 [P] [US4] Add a finite-expiration refusal test to `crates/server/src/services/execute_proposal/tests.rs`: a proven transaction whose expiration is unbounded or outside the horizon is refused with `GUARDIAN_EXECUTION_NO_FINITE_EXPIRATION` **before** submission (SC-027, SC-033, FR-046)
- [ ] T081 [P] [US4] Add a definite-rejection test in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: the node explicitly rejects the proven transaction, so the candidate is discarded and the reservation released, reporting `GUARDIAN_EXECUTION_SUBMISSION_REJECTED`; an **ambiguous** transport failure in the same position instead reports `submitted` and retains the reservation (FR-030, FR-048)
- [ ] T082 [P] [US4] Add a proposal-deletion test in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: after a definite rejection resolves, the matching proposal is gone and the execution reports `proposal_exists: false`, so no client is told a definitely-rejected transaction may be retried (SC-037)
- [ ] T083 [P] [US4] Add a `SwitchGuardian` fault-injection test in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a `SwitchGuardian` canonicalizing during proving causes the execution to fail at the pre-boundary admissibility re-check, with nothing submitted (SC-026)
- [ ] T084 [P] [US4] Add terminal-persistence tests in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a post-submission failure whose candidate **and proposal** were deleted still reports `failed` with a cause and `proposal_exists: false`; and promotion and discard each persist the outcome inside their own transaction (SC-021, SC-025)
- [ ] T085 [US4] Implement lease renewal ownership: the **execution worker** heartbeats its own `execution:{account_id}` lease for the whole pre-boundary span including proving, in `crates/server/src/services/execute_proposal/mod.rs`; after a fenced ownership transfer **only the reconciliation owner** heartbeats, in `crates/server/src/jobs/execution_reconcile/mod.rs`. Reconciliation MUST NOT renew a pre-boundary worker's lease — that would keep a dead worker's reservation alive and defeat FR-028's expiry path. Test both: a proof outlasting one lease period keeps its reservation, and a killed pre-boundary worker's lease expires on schedule (FR-023, FR-028)

### Implementation for User Story 4

- [ ] T086 [US4] Add the reservation lease loop to `crates/server/src/jobs/execution_reconcile/`: renew while an execution is in flight, and on expiry branch on whether the boundary was crossed — release and fail before it, transfer to reconciliation after it (FR-028)
- [ ] T087 [US4] Add the reservation refusal to `crates/server/src/services/push_delta.rs`: while a reservation is active for the account, refuse with `GUARDIAN_EXECUTION_CONFLICT` (FR-027), with `meta.blocking_proposal_id` from `AlreadyReserved.proposal_id`
- [ ] T088 [US4] Implement the **superseded** evidence path in `crates/server/src/jobs/execution_reconcile/`: the account moved to a commitment that is neither `base_commitment` nor this transaction's result → `failed` / `GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` (FR-040)
- [ ] T089 [US4] Implement the **expired** evidence path in `crates/server/src/jobs/execution_reconcile/`: the chain passed `expiration_block` with the account still at `base_commitment` → `failed` / `GUARDIAN_EXECUTION_EXPIRED` (FR-040). This is what makes termination guaranteed rather than best-effort
- [ ] T090 [US4] Implement restart recovery in `crates/server/src/jobs/execution_reconcile/`: an execution whose `SubmissionEvidence` exists is **never** retried, only reconciled; one without it fails-and-releases (FR-031). Recovery reads the evidence, not the phase column — the evidence is the boundary
- [ ] T091 [US4] Implement stale-result rejection in `crates/server/src/services/execute_proposal/mod.rs`: a proving result produced by a worker whose fence is no longer current MUST NOT be carried forward (FR-029). Two proofs may compute concurrently — the external prover cannot be cancelled — but only the fence-current result may proceed
- [ ] T092 [US4] Add a single-writer assertion for terminal outcomes in `crates/server/src/jobs/execution_reconcile/`: reconciliation observing the account at `expected_commitment` waits for promotion and writes **nothing**, and a test asserts no code path other than the extended `promote_candidate` (T018) ever writes `landed`. Post-boundary *failures* go through `resolve_execution` (T017), not this path (FR-053, SC-036)
- [ ] T093 [US4] Implement `proposal_exists` on the envelope in `crates/server/src/services/execution_status.rs` as a **fact** read from storage, never retry advice, matching the truth table in `contracts/execution-api.md` (FR-042)

**Checkpoint**: the custody-safety argument holds under concurrency, crashes, and
ambiguous submissions.

---

## Phase 6: User Story 3 — Configuring the client, and self-execution staying intact (P2)

**Story goal**: One client-level setting decides whether proposals are
Guardian-executable. Default off. No existing method signature changes.
Self-execution is untouched.

**Independent test**: Create the same transaction type through a default client
and a Guardian-executable client; confirm the default payload is byte-identical
to today's and its Guardian execution is refused; confirm the other succeeds;
confirm local execution works for both; confirm creation succeeds against a
server with proving disabled.

### Tests for User Story 3

- [ ] T094 [P] [US3] Add a payload-shape test in `crates/miden-multisig-client/` asserting a default-client proposal carries **no** `transaction_request` and is shape-identical to a pre-feature proposal (US3 scenarios 1, 5; SC-009)
- [ ] T095 [P] [US3] Add a test in `crates/miden-multisig-client/` asserting proposal **identity is unchanged** by the new field, since the id derives from `tx_summary` alone via `delta_proposal_id` (`services/push_delta_proposal.rs:124-151`, FR-012)
- [ ] T096 [P] [US3] Add TS equivalents of T094 and T095 in `packages/miden-multisig-client/`
- [ ] T097 [P] [US3] Add a cross-SDK parity test in `packages/miden-multisig-client/tests/execution-parity.test.ts` asserting the Rust and TS SDKs produce the same observable behavior and outcomes when both are configured Guardian-executable (US3 scenario 7, FR-033)
- [ ] T098 [P] [US3] Add tests in `crates/miden-multisig-client/` and `packages/miden-multisig-client/` confirming list, review, sign, and export behave identically for proposals with and without the stored request (US3 scenario 6)

### Implementation for User Story 3

- [ ] T099 [US3] Add client-level `ProposalExecutionMode` to `crates/miden-multisig-client/`, defaulting to **not attached**, with **no** new per-call methods and no changes to existing signatures, per `contracts/sdk-api.md`
- [ ] T100 [US3] Attach the `transaction_request` envelope when the mode is enabled in `crates/miden-multisig-client/`: `format_version`, `protocol_line`, `serializer_id` (full package version including prerelease), `checksum`, base64 `bytes`. The client never asks the server what it supports (US3 scenario 4)
- [ ] T101 [US3] Apply the **shared 256-block finite expiration policy** to every built-in Guardian-executable proposal family in `crates/miden-multisig-client/` (FR-051): pass it while constructing send scripts, use `TransactionRequestBuilder::expiration_delta` for no-script requests, and add `tx::update_expiration_block_delta` to Guardian-owned custom scripts. Preserve opaque custom-producer request bytes unchanged and document that their scripts must set a finite expiration
- [ ] T102 [P] [US3] Mirror T099–T101 in `packages/miden-multisig-client/`, including the same family-specific mechanisms and unchanged custom-producer bytes
- [ ] T103 [P] [US3] Add cross-SDK parity tests: both SDKs emit the same `serializer_id`, use the same 256-block built-in default, preserve summary/proposal identity across execution modes for the same effects and salt, and accept/reject finite/non-finite custom requests consistently (FR-012, FR-014, FR-051)
- [ ] T104 [P] [US3] Add the execution request and status calls to the Rust base client `crates/client/` (the `guardian-client` crate), which has no Miden dependency (FR-034)
- [ ] T105 [P] [US3] Mirror T104 in the TS base client `packages/guardian-client/`
- [ ] T106 [US3] Add **every** new error code to the TS client error-code vocabulary in `packages/guardian-client/`. This is exactly the gap that produced #353, where `candidate_landed` was missing
- [ ] T107 [P] [US3] Commit cross-language envelope fixtures under `fixtures/miden-multisig-client/`: identical `checksum`, `protocol_line`, and `serializer_id` produced by both SDKs for the same transaction, plus a protocol-mismatch fixture. Wire them into both SDK test suites (SC-029, SC-010)
- [ ] T108 [P] [US3] Add an override-versus-default test in `crates/miden-multisig-client/` and `packages/miden-multisig-client/`: a per-proposal threshold override is honored where present and the account default applies where absent (SC-004)
- [ ] T109 [US3] Add a no-regression suite for the untouched paths in `examples/demo` (Rust) and `examples/smoke-web` (TS), covering a built-in **and** a custom proposal type: self-execution, export, offline signing, and local execution of an *imported* proposal. This is the evidence that adding Guardian execution changed nothing for clients that do not use it (SC-012, FR-035)
- [ ] T110 [US3] Create the `examples/execution-smoke` harness (new artifact): request Guardian execution and poll to `landed` using **only** `packages/guardian-client` plus a Rust counterpart over `crates/client`, constructing no Miden client and connecting to no node. This is the **only** artifact that can evidence the no-Miden guarantee (SC-001, FR-034)
- [ ] T111 [US3] Update `examples/demo` and `examples/smoke-web` for the full Guardian-execution lifecycle — propose as Guardian-executable, sign to threshold, request execution, observe landing — for a built-in **and** a custom proposal type. `examples/smoke-web` MUST NOT be claimed for SC-001, since it constructs a Miden client (SC-015)

**Checkpoint**: integrators opt in explicitly; nothing changes for anyone who does not.

---

## Phase 7: User Story 5 — Operators control whether the capability exists (P3)

**Story goal**: An operator decides whether this deployment offers
prove-and-commit and where proving happens; a server without a prover refuses
explicitly rather than attempting.

**Independent test**: Start with no prover → refused with capability-unavailable,
nothing attempted. Start with the capability disabled → same error class
regardless of prover reachability. Start with a reachable prover → succeeds.

### Tests for User Story 5

- [ ] T112 [P] [US5] Add `crates/server/src/services/execute_proposal/capability_tests.rs`: with no prover configured, execution is refused with `GUARDIAN_PROVING_UNAVAILABLE` and nothing is proven or submitted (US5 scenario 1, FR-021)
- [ ] T113 [P] [US5] Add to `crates/server/src/services/execute_proposal/capability_tests.rs`: with the capability explicitly disabled, the same error class is returned regardless of prover reachability, and **100%** of execution requests are refused with no proving attempted in either configuration (US5 scenario 2, SC-014)
- [ ] T114 [P] [US5] Add to `crates/server/src/services/execute_proposal/capability_tests.rs`: with a configured but unreachable prover, the request is **accepted** and the execution reports `failed` / `GUARDIAN_EXECUTION_PROVING_FAILED` — an unreachable prover is an async failure, not a synchronous refusal (US5 scenario 3)
- [ ] T115 [P] [US5] Add a startup test in `crates/server/src/services/execute_proposal/capability_tests.rs`: execution enabled with canonicalization disabled refuses to start, naming the misconfiguration (SC-022, FR-043)
- [ ] T116 [P] [US5] Add a proving-disabled creation test in `crates/server/src/services/push_delta_proposal.rs`: with the server's proving capability disabled, creating a Guardian-executable proposal still **succeeds** and stores the envelope; the mismatch surfaces only when execution is requested (US3 scenario 4, FR-009)

### Implementation for User Story 5

- [ ] T117 [US5] Gate the execution endpoints on the capability in `crates/server/src/services/execute_proposal/mod.rs`: unset prover, disabled kill-switch, or optimistic mode → `GUARDIAN_PROVING_UNAVAILABLE` with **no fallback** to local proving (FR-021)
- [ ] T118 [US5] Ensure the `proving` Cargo feature gates compilation cleanly in `crates/server/Cargo.toml`: a server built without it reports the capability as unavailable rather than failing to build or panicking
- [ ] T119 [US5] Add `proving` to the server feature list for published images: update `Dockerfile` (`ARG GUARDIAN_SERVER_FEATURES=postgres`), the compose guides under `docs/guides/`, and `docs/SERVER_AWS_DEPLOY.md`. Without this every published-image deployment answers execute with `GUARDIAN_PROVING_UNAVAILABLE` forever, with no log line naming a build-time cause (FR-021)

**Checkpoint**: safe to ship to a shared deployment.

---

## Phase 8: Polish & Cross-Cutting Concerns

- [ ] T120 [P] Regenerate the committed OpenAPI specs with `cargo run --features evm --bin gen-openapi -- docs` (AGENTS.md §4) and commit the result
- [ ] T121 [P] Document the three endpoints, the five-state vocabulary, and every error code in `spec/api.md`
- [ ] T122 [P] Document all eight configuration variables in `docs/CONFIGURATION.md`
- [ ] T123 [P] Add the prover-timeout failure mode to `docs/TROUBLESHOOTING.md`: the client library's 10 s default (`tx_prover.rs:45`) sits below observed proving times of 6.2–20.1 s and surfaces as an intermittent "failed to prove transaction" that never mentions a timeout
- [ ] T124 [P] Add execution metrics in `crates/server/src/metrics/`: executions by terminal state, proving duration, reservation age, and reconciliation outcomes by evidence path. Internal phases are diagnosable from metrics and logs, never from the wire (FR-025)
- [ ] T125 [P] Add transport-parity tests in `crates/server/src/api/execution_tests.rs` covering **every** case in the HTTP↔gRPC table, verified per case rather than by inspection (SC-023)
- [ ] T126 Run the full validation matrix in [validation-matrix.md](./validation-matrix.md) and confirm every success criterion has a passing test or a recorded, justified exception
- [ ] T127 Update `docs/MULTISIG_SDK.md` with the client-level execution mode and the finite-expiration requirement
- [ ] T128 [P] Document the execution service and its sequence diagram in `spec/processes.md`, matching the endpoints added to `spec/api.md`
- [ ] T129 [P] Annotate feature 008's FR-015 in `speckit/features/008-custom-proposal-producer/spec.md` with the narrowing this feature introduces: the serialized transaction request is now persisted, but only for proposals from a Guardian-executable client

---

## Dependencies

```text
Phase 1 (Setup)
   ↓
Phase 2 (Foundational) ─── GATE: T024 + T025 must pass
   ↓
Phase 3 (US1, P1) ── MVP
   ↓
Phase 4 (US2, P1) ── hardens the checks US1 introduced
   ↓
Phase 5 (US4, P1) ── MUST NOT be deferred past US2
   ↓
Phase 6 (US3, P2) ── clients; needs the server contract from Phase 3
   ↓
Phase 7 (US5, P3)
   ↓
Phase 8 (Polish)
```

**Ordering constraints that are not negotiable:**

- **Phase 2 before everything.** The admission primitive is the foundation; a bug
  there is inherited by every phase and surfaces only intermittently.
- **Phase 5 must not be deferred.** Reconciliation is what makes the no-retry
  boundary survivable. Shipping Phase 3 without Phase 5 leaves accounts held
  until expiration with no resolution path — a custody outage, not a rough edge.
- **T037 before T039.** The internal ack path must exist before the sequence can
  use it.
- **T043 is the boundary.** Every task after it must treat submission as authorized and
  prepared, forbid another proof or send, and reconcile only.
- **Phase 6 after Phase 3.** Constitution Principle I — the server contract
  drives the clients, not the reverse.

## Parallel opportunities

| Phase | Parallel set |
|---|---|
| 1 | T002–T004 |
| 2 | T011–T013 (decorators + mock), T023, T026–T029 (lease, transfer, race, atomicity tests) |
| 3 | T033–T036 (tests, before implementation) |
| 4 | T055–T059, T066, T068, T069 (test tasks, independent files) |
| 5 | T071–T084 (the whole fault-injection and concurrency test set) |
| 6 | T094–T098, T102–T105, T107, T108 |
| 7 | T112–T116 |
| 8 | T120–T125, T128, T129 |

T009 and T010 are **not** parallel with each other in practice: the Postgres
implementation establishes the semantics the filesystem one must match, so write
Postgres first and treat T024 as the parity check.

## Implementation strategy

**MVP = Phases 1 + 2 + 3.** That is heavier than a typical MVP because FR-023
requires the durable reservation for the whole execution span, so there is no
smaller increment that executes even one proposal correctly. Stopping after
Phase 3 would give a working happy path with unsafe failure handling — useful as
a demo, not shippable.

**Smallest shippable increment = Phases 1–5.** US1, US2, and US4 are all P1 and
together form the correctness argument: the capability, its binding guarantee,
and its single-submission guarantee. Phases 6 and 7 add integrator ergonomics
and operator control on top.

**Recommended sequence**: complete Phase 2 with T024/T025 green before writing
any service code. Then run Phase 3 to a working `landed`, and immediately follow
with Phase 5's fault injection — the boundary's crash-safety is far cheaper to
get right while the sequence is fresh than to retrofit.

## Task count

| Phase | Tasks | Of which tests |
|---|---|---|
| 1 — Setup | 4 | 2 |
| 2 — Foundational | 28 | 10 |
| 3 — US1 (P1) | 22 | 7 |
| 4 — US2 (P1) | 16 | 8 |
| 5 — US4 (P1) | 23 | 16 |
| 6 — US3 (P2) | 18 | 8 |
| 7 — US5 (P3) | 8 | 5 |
| 8 — Polish | 10 | 2 |
| **Total** | **129** | **58** |

Phase 5 remains the largest and is mostly tests. That ratio is correct: the
concurrency and crash guarantees are the requirements most likely to be
implemented incorrectly and least likely to fail visibly.

**Grew from 91 to 129 across three review rounds.** The additions were not scope creep — they were
requirements that had no task at all: the FPI refusal (FR-050), per-account lease
acquisition and fenced ownership transfer (FR-038, FR-052), submission-outcome
classification, request orchestration, attempt identity, the mandatory cross-language
fixtures, and the example harnesses — including `examples/execution-smoke`, the only
artifact that can evidence the no-Miden guarantee.
