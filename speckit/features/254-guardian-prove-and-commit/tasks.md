# Tasks: Guardian Proves and Commits Transactions

**Feature Key**: `254-guardian-prove-and-commit`
**Spec**: [spec.md](./spec.md) (revision 11) | **Plan**: [plan.md](./plan.md) | **Data model**: [data-model.md](./data-model.md)
**Contracts**: [contracts/execution-api.md](./contracts/execution-api.md) | [contracts/sdk-api.md](./contracts/sdk-api.md)
**Generated**: 2026-07-28 | **Regenerated**: 2026-09-30 for the Miden 0.17 release-candidate pins
(protocol / standards / tx `0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`, `miden-node-proto-build`
`0.17.0-rc.3`, web SDK `0.17.0-rc.4`). The 0.16 anchored-reproduction tasks (anchor admission,
anchor header comparison, client fee-conversion mirroring, `ANCHOR_EXPIRED`, the FPI refusal) are
gone; see RFC 0001 Appendix A.3.

## Format

`- [ ] [TaskID] [P?] [Story?] Description with file path`

`[P]` = parallelizable (different files, no dependency on an incomplete task).

## Before starting

- **Branch from `main`, not from the spike.** The implementation branch starts from `main`
  (which already carries the 0.17 pins, tip execution in both SDKs, and the multisig auth args).
  The Gate 0 spike lives on `254-execution-spike` at `769e2a90`, built on the 0.16 release
  candidates and 34 commits behind. Its `network/miden/execution/{blockchain,store}.rs` and the
  RPC client's `sync_chain_mmr` / `conversion.rs` are **ported** (Phase 2B), not merged. Read them
  with `git show 769e2a90:<path>`.
- **Cite 0.17 sources.** Crate sources are under `~/.cargo/registry/src/index.crates.io-*/`
  (`miden-protocol-0.17.0-rc.7`, `miden-tx-0.17.0-rc.7`, `miden-standards-0.17.0-rc.7`,
  `miden-client-0.17.0-rc.4`, `miden-node-proto-build-0.17.0-rc.3`). The evidence log is
  [research.md](./research.md) § "Miden 0.17 re-verification".
- **Live environment.** Devnet runs node 0.17.0-rc.2; testnet still runs 0.16, so testnet-style
  live runs use a local `miden-node` from the pinned line. Devnet accounts are funded through
  `scripts/devnet-register-account.sh` (RegisterAccount). Every transaction, including the first,
  needs a balance in the chain's native fee asset.
- **Open decision gating T088.** The FR-016 per-proposer quota configuration and allocation rule
  (T087) must be decided by the maintainers before T088 is implemented.
- **Style.** AGENTS.md §11: no inline comments, no procedural numbered comments, exhaustive
  enums with no catch-all, mandatory fields over optional. No em-dashes in code, docs or
  commit messages.

## What is already done

**The proving architecture is ratified: do not re-plan it.** The Gate 0 spike produced a
`DataStore` over Guardian's own state, assembled a `PartialBlockchain` from node RPC, and proved
a witness through the remote prover against public testnet, with no new dependencies. That
evidence is 0.16 evidence and is re-run after the port (T041). On 0.17 the spike's tip-reference
chain view is the normative construction again (FR-061), because the signed summary binds a
proposer-chosen bound block rather than the reference block.

**Gate 0 is narrowed, not fully passed.** P2ID and configuration executed and proved under
`MockChain`; `consume_notes` ran only as prepared execution; the custom family (#266) is unrun;
live submission is unvalidated. The 0.17 additions (foreign-account loading, `ProtocolConfig`,
bound-block tracking, sealing) have no Gate 0 evidence at all yet. Do not restate any of this as
"all four families validated".

## Three deliberate deviations from strict story-by-story ordering

1. **Foundational is heavy.** FR-023 requires a durable reservation for the whole span from
   acceptance to terminal state, so US1 cannot execute a single proposal without the reservation
   and its admission primitive (Phase 2A). US4 keeps what is genuinely separable: the
   concurrency behaviors, lease expiry, recovery, and fault injection on top of that foundation.
2. **The 0.17 execution seam is foundational too (Phase 2B).** Every user story that executes
   anything depends on the ported `DataStore`, the tip chain view, foreign-account loading and
   sealing, so they are built and tested in isolation before US1 wires them into FR-045.
3. **US1 implements every FR-045 step on the happy path; US2 hardens the negative paths.** The
   signature-subset selection (FR-006), structural checks (FR-056, FR-057, FR-051) and binding
   check (FR-007) are steps 1, 2 and 4, so the US1 happy path structurally requires them. US2
   owns every mismatch caught, distinguishable by error code, with no side effects. The spec
   says US2 "is not separable from US1 in value"; this split keeps each phase testable.

## Test placement convention

`crates/server` has **no `tests/` directory**: test-bearing files use inline `#[cfg(test)]`
modules colocated with the code, and larger test bodies get a sibling file declared as
`#[cfg(test)] mod`, exactly as the spike did (`network/miden/execution/{mod,tests,live_tests}.rs`).
That is why `execute_proposal` is a directory module. Postgres-dependent tests are gated
`#[ignore]` and read `DATABASE_URL`; live-node tests are gated `#[ignore]` behind the `e2e`
feature.

**Tests are in scope**, not optional: Constitution Principle V, 44 success criteria, and the
fault-injection rows in [validation-matrix.md](./validation-matrix.md) all require them.

---

## Phase 1: Setup (Shared Infrastructure)

- [ ] T001 Create migration `crates/server/migrations/2026-07-28-000001_execution_reservations/{up.sql,down.sql}` with the three tables `execution_reservations`, `execution_submissions` and `execution_outcomes` (including the `error_meta JSONB` column on `execution_outcomes`), the `execution_reservations_one_active_per_account` partial unique index, the `execution_submissions_account_nonce` index, and the `UNIQUE (account_id, proposal_id, attempt)` constraints exactly as specified in `data-model.md` § Migration. Rename the directory's date if `main` already has a later migration, so ordering stays monotonic after `2026-09-22-000001_miden_017_irreversible_reset`
- [ ] T002 [P] Add Diesel table definitions for `execution_reservations`, `execution_submissions`, and `execution_outcomes` to `crates/server/src/schema.rs`
- [ ] T003 [P] Scaffold empty modules with `pub use` re-exports and wire each into its parent `mod.rs`: `crates/server/src/services/ack_delta_internal.rs`, `crates/server/src/services/execute_proposal/mod.rs`, `crates/server/src/services/execution_status.rs`, `crates/server/src/services/execution_codec.rs`, `crates/server/src/jobs/execution_reconcile/mod.rs`, and `crates/server/src/network/miden/execution/{mod,blockchain,store,foreign,sealing,tests,live_tests}.rs`. `execute_proposal` is a directory module so its test bodies live in sibling files
- [ ] T004 [P] Add the `proving` Cargo feature to `crates/server/Cargo.toml` as `proving = ["miden-tx"]` (miden-tx is already an optional dependency used by `e2e`), make `e2e` include `proving`, and enable the `miden-client` `tonic` feature the remote prover client needs. A server built without `proving` MUST compile and report the capability as unavailable rather than failing to build or panicking
- [ ] T005 [P] Add the execution config block in `crates/server/src/config/` for the eight variables in `contracts/execution-api.md` § Configuration: `GUARDIAN_TX_PROVER_URL` (typed as `CredentialUrl`, `crates/server/src/secret/wrappers.rs`, so credentials in the URL are redacted in `Debug`, matching `evm/config.rs`'s `rpc_url`), `GUARDIAN_TX_PROVER_TIMEOUT_SECS`, `GUARDIAN_PROVING_ENABLED`, `GUARDIAN_MAX_PROPOSAL_REQUEST_BYTES`, `GUARDIAN_MAX_ACCOUNT_REQUEST_BYTES`, `GUARDIAN_EXECUTION_LEASE_SECS`, `GUARDIAN_EXECUTION_RECONCILE_INTERVAL_SECS`, `GUARDIAN_EXECUTION_EXPIRATION_HORIZON_BLOCKS`, plus the serializer allowlist used by FR-015. Unit tests MUST assert the prover timeout defaults to exactly `300` (FR-020; the upstream default is 10 s, `miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`), the horizon defaults to `512`, and a configured horizon below `256` is rejected at load, since every built-in proposal carries a 256-block transaction delta (FR-046, FR-051)

---

## Phase 2A: Foundational, storage and admission (BLOCKING)

**Gate**: T026 and T027 must pass before any phase-3 task starts. If the admission primitive is
wrong, every later phase inherits a concurrency bug that integration tests surface only
intermittently.

- [ ] T006 Add `ExecutionReservation`, `SubmissionEvidence`, and `ExecutionOutcome` types to `crates/server/src/storage/mod.rs` per `data-model.md`, including all five mandatory FR-039 evidence fields (`transaction_id`, `base_commitment`, `expected_commitment`, `reference_block`, `expiration_block`) plus `attempt`, and `ExecutionOutcome.error_meta`. `reference_block` is the attempt's `R`, not the summary's bound block. `expiration_block` MUST come from `ProvenTransaction::expiration_block_num()` and never from a request field. `TransactionInputs` and the sealed submission blob are deliberately NOT in the evidence (`data-model.md` § "What is deliberately not in the evidence")
- [ ] T007 Add exhaustive outcome enums `ReservationWrite { Created, AlreadyReserved { holder_id, proposal_id }, CandidateExists, StaleLease, ClaimSuperseded }`, `AdmissionWrite { Admitted, NotAuthorized, CandidateExists, StaleLease }`, and `ResolveWrite { Resolved, NotAuthorized, AlreadyResolved, StaleLease }` to `crates/server/src/storage/mod.rs`, mirroring `CanonicalWrite`, and add the `ProtectedByExecution` variant to `CanonicalWrite` itself. No catch-all variant: a new case must break every match site, which forces every existing discard call site to handle protection
- [ ] T008 Declare the new `StorageBackend` methods in the canonicalization-writes block of `crates/server/src/storage/mod.rs` **with no default bodies**, extending the existing comment that explains why canonicalization writes have no defaults: `create_execution_reservation`, `renew_execution_reservation`, `release_execution_reservation`, `claim_execution_reservation` (FR-052 compare-and-set ownership transfer), `load_execution_reservation`, `admit_execution_candidate`, `resolve_execution` (post-boundary failure: discard, outcome, release in one transaction), `fail_execution` (pre-boundary failures only: outcome plus release as one commit), `list_unresolved_submissions`. There is deliberately no operation that writes an outcome without also releasing (FR-053)
- [ ] T009 Add per-account execution-lease acquisition in `crates/server/src/coordination/mod.rs` and `crates/server/src/coordination/postgres/lease.rs`: an `execution_lease_name(account_id) -> String` helper producing `execution:{account_id}`, plus acquire and renew through the existing lease machinery. Add a unit test asserting the name is never equal to `CANONICALIZATION_LEASE` (FR-038)
- [ ] T010 Implement the reservation methods on `PostgresService` in `crates/server/src/storage/postgres.rs`, each as **one transaction**: `lock_account_metadata`, then `lease_fence_is_current`, then the conditional write, returning `unfenced_write_error("<op>")` when the fence is absent, following `discard_candidate` exactly. The fence MUST be the account-scoped `execution:{account_id}` lease, never `CANONICALIZATION_LEASE`, which admits one holder cluster-wide
- [ ] T011 Implement the reservation methods on `FilesystemService` in `crates/server/src/storage/filesystem.rs` as read-modify-write under **`delta_write_lock`**, not a new mutex, since admission must be atomic with respect to the candidate writes that lock serializes. Persist holder, fence, and expiry in the reservation file; every execution-owned mutation validates the supplied holder and fence against the active reservation before writing, and a stale or unfenced call returns `StaleLease`. Do not change the intentionally unfenced client `request_candidate_abandon` annotation or the `AlwaysLeader` canonicalization semantics
- [ ] T012 [P] Add pass-through implementations of the new methods to `EncryptedStorage` in `crates/server/src/storage/encryption/decorator.rs`
- [ ] T013 [P] Add pass-through implementations with metric instrumentation to `InstrumentedStorage` in `crates/server/src/metrics/storage.rs`
- [ ] T014 [P] Add implementations to `MockStorageBackend` in `crates/server/src/testing/mocks.rs`, with settable canned outcomes so service-layer tests can drive every `ReservationWrite`, `AdmissionWrite` and `ResolveWrite` variant
- [ ] T015 Implement `admit_execution_candidate` as the FR-045 **step 12** commit in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: persist the candidate, set `has_pending_candidate`, set `candidate_nonce`, and insert `SubmissionEvidence` in one atomic unit. Returns `AdmissionWrite`
- [ ] T016 Add the **owner-authorized admission exception** (FR-037) to the admission path in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: admission succeeds for the caller presenting the matching reservation's `holder_id` and a valid fence, and is refused as `NotAuthorized` for every other caller. Without this, Guardian deadlocks admitting its own candidate
- [ ] T017 Implement `claim_execution_reservation` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` as a compare-and-set on `holder_id` plus `fence_token` of a live reservation, returning `ClaimSuperseded` rather than stealing on a stale expectation, and never releasing as part of the handover (FR-052, FR-028)
- [ ] T018 Implement `resolve_execution` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: validate execution ownership and fence, then discard the candidate, **delete its matching proposal**, upsert `ExecutionOutcome` (including `error_meta`), and release the reservation in one transaction, returning `ResolveWrite`. Proposal deletion is part of this commit, not a follow-up as in canonicalization's `remove_candidate`, because a surviving proposal reports `proposal_exists: true`, which FR-042 defines as a permitted retry. This is the only operation permitted to discard a boundary-crossed candidate (FR-053, SC-025, SC-037)
- [ ] T019 Extend the fenced `promote_candidate` primitive in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` to upsert `ExecutionOutcome { state: committed }` and release the reservation inside its own transaction, taking the same per-account lock every other reservation write takes. Promotion is explicitly authorized to release while holding the canonicalization fence rather than the execution fence (FR-054)
- [ ] T020 Implement `fail_execution` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs`: upsert `ExecutionOutcome` (code, message, `error_meta`) and release the reservation as one commit for pre-boundary failures (FR-053, SC-036)
- [ ] T021 Make the **canonicalization worker's** `discard_candidate` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` consult `execution_submissions` for the account and nonce inside its own transaction, returning `CanonicalWrite::ProtectedByExecution` for a candidate whose execution has crossed the boundary and not yet resolved. Between step 12 and step 14 the candidate looks ordinary to canonicalization. This MUST NOT gate `resolve_execution` (T018): the protection is scoped to the caller, not the row (FR-049)
- [ ] T022 Extend `submit_candidate` in `crates/server/src/storage/postgres.rs` and `crates/server/src/storage/filesystem.rs` with the reservation predicate: reservation creation fails when a candidate exists (`CandidateExists`), and non-owner candidate admission fails when a reservation is active, decided inside the same lock or transaction, never as a separate check-then-act (FR-037)
- [ ] T023 Add the error codes to `crates/server/src/error.rs` per `contracts/execution-api.md` § Error codes, each with its HTTP status and gRPC status mapping from the parity table: the FR-022 synchronous refusals; the asynchronous causes `GUARDIAN_EXECUTION_BINDING_MISMATCH`, `STATE_MISMATCH`, `REQUEST_CODEC`, `PROTOCOL_MISMATCH`, `REQUEST_INVALID`, `EXPIRATION_REACHED`, `CHAIN_BEHIND`, `FOREIGN_ACCOUNT_UNAVAILABLE`, `INSUFFICIENT_FEE`, `PROVING_FAILED`, `SEALING_FAILED`, `EXPIRATION_BEYOND_HORIZON`, `ACCOUNT_INADMISSIBLE`, `SUBMISSION_REJECTED`, `CANDIDATE_DISCARDED`, `EXPIRED`, `LEASE_EXPIRED`, `ABANDONED` (all prefixed `GUARDIAN_EXECUTION_`); and the status-read `GUARDIAN_EXECUTION_NOT_FOUND`. Model `error.meta` as typed, closed enums: `RequestInvalidReason { BoundBlockNotDeclared, AuthArgsMissing, ApprovalExpirationMissing, InputNotesNotPinned }`, `ForeignAccountUnavailableReason { Private, Unavailable }`, `ExpirationBound { Approval, Transaction }`, serialized as `meta.reason` / `meta.bound`. `ANCHOR_EXPIRED`, `NO_FINITE_EXPIRATION` and `FOREIGN_INPUTS_UNSUPPORTED` MUST NOT exist
- [ ] T024 [P] Add state-read authorization tests in `crates/server/src/services/execution_status.rs`: a cosigner of the account may read execution state; a non-cosigner is refused with `GUARDIAN_AUTHENTICATION_FAILED` on all three endpoints (FR-004)
- [ ] T025 Implement the transaction-request envelope codec in `crates/server/src/services/execution_codec.rs`: verify the SHA-256 `checksum` over the raw bytes **before** any deserialization attempt, check `format_version`, require `protocol_line` to equal the server's own line (`"0.17"`) by exact string comparison, and require `serializer_id` to be admitted by the configured allowlist (T005). `serializer_id` is the exact `miden-client` version, including prerelease, that serialized the bytes (the web SDK's embedded client for TypeScript), not the Guardian SDK version. Map failures to `GUARDIAN_EXECUTION_REQUEST_CODEC` / `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH`. `TransactionRequest` serialization carries no version tag and rc.4 bytes do not decode under rc.3 (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:447-477`, rc.3 `:439-443`), so the allowlist is the only guard (FR-014, FR-015). Unit tests for each rejection path
- [ ] T026 Add storage-layer admission tests in `crates/server/src/storage/execution_reservation_tests.rs`, declared as a `#[cfg(test)] mod` from `crates/server/src/storage/mod.rs`, covering, on **both** backends: reservation created when clean; refused when a candidate exists; refused when a reservation is already active with `AlreadyReserved.proposal_id` naming the blocker; owner-authorized admission succeeds; non-owner admission returns `NotAuthorized`; candidate and evidence are both present or both absent after admission
- [ ] T027 Add Postgres-only concurrency tests in `crates/server/src/storage/execution_reservation_tests.rs` (gated `#[ignore]`, `DATABASE_URL`): two concurrent `create_execution_reservation` calls for one account yield exactly one `Created` and one `AlreadyReserved`; a stale fence yields `StaleLease` and writes nothing; the partial unique index rejects a second active reservation even when the service-layer check is bypassed
- [ ] T028 [P] Add a promotion-versus-resolution race test in `crates/server/src/storage/execution_reservation_tests.rs` (Postgres, `#[ignore]`): `promote_candidate` and `resolve_execution` issued concurrently for one account produce exactly one terminal outcome and one release, with the loser observing `AlreadyResolved` and writing nothing (SC-038, FR-054)
- [ ] T029 [P] Add terminal-atomicity fault injection in `crates/server/src/storage/execution_reservation_tests.rs`: for each of the three terminal operations, a crash injected mid-transaction leaves neither a terminal execution holding a reservation nor a released reservation without an outcome (SC-036, FR-053)
- [ ] T030 [P] Add a per-account lease test in `crates/server/src/storage/execution_reservation_tests.rs`: two accounts hold execution leases simultaneously on one replica, while two executions for the same account serialize, written so it fails if `CANONICALIZATION_LEASE` is substituted (SC-034)
- [ ] T031 [P] Add ownership-transfer tests in `crates/server/src/storage/execution_reservation_tests.rs` on both backends: a reconciliation owner claims a live reservation by compare-and-set; a claim with a stale holder or fence token returns `ClaimSuperseded` and writes nothing; the reservation is never observed released mid-handover. The filesystem case MUST pause the old task on `delta_write_lock`, transfer ownership, then resume it and assert every attempted execution-owned mutation returns `StaleLease` without writing (SC-035)
- [ ] T032 Add startup validation in `crates/server/src/builder/` (FR-043): refuse to start with execution enabled while canonicalization is disabled (`ServerBuilder::with_canonicalization(None)`, `crates/server/src/builder/mod.rs`), naming the misconfiguration. The shipped binary always runs Candidate (`crates/server/src/main.rs`), so this guards embedders

---

## Phase 2B: Foundational, the 0.17 execution seam (BLOCKING)

**Gate**: T041 and T042 must pass before T052 starts. These tasks port the spike from 0.16
rc.9 onto `main` and add what 0.17 requires; nothing here touches the lifecycle.

- [ ] T033 [P] Port `sync_chain_mmr` and the spike's `conversion.rs` from `769e2a90:crates/miden-rpc-client/src/` into `crates/miden-rpc-client/src/lib.rs` and `crates/miden-rpc-client/src/conversion.rs`, re-targeted to the 0.17 protos: `SyncChainMmrRequest` gains `finality_level` (use `COMMITTED`), `block_signatures` are `primitives.Signature`, and the genesis-seeded response carries the target's `protocol_config` (`miden-node-proto-build-0.17.0-rc.3/proto/rpc.proto:673-713`). Follow the crate's existing retry policy for reads; resolve the one textual conflict in `crates/miden-rpc-client/README.md` by keeping `main`'s text and adding the new call
- [ ] T034 [P] Add header reads with the protocol configuration to `crates/miden-rpc-client/src/lib.rs`: a `get_block_header` variant that sets `include_protocol_config` and `include_mmr_proof` and returns the header, the optional MMR path with its `chain_length`, and the optional `ProtocolConfig` (`rpc.proto:270-295`). Today both `get_block_header` and `get_chain_tip` hardcode `include_protocol_config: None`; keep existing callers' behavior unchanged
- [ ] T035 [P] Add `get_account_at(account_id, block_num, details)` to `crates/miden-rpc-client/src/lib.rs` using `GetAccount` with `block_num` set and the storage-map and vault witnesses the executor asks for (`rpc.proto:33,345-412`), mapping the node's pruned-block error (node code 5, as `miden-client-0.17.0-rc.4/src/rpc/errors/node/account.rs:28-45` decodes it) to a typed `BlockPruned` error and a private account to a typed `PrivateAccount` result. Today `get_account_with_details` hardcodes `block_num: None`
- [ ] T036 [P] Add `get_transaction_encryption_key()` to `crates/miden-rpc-client/src/lib.rs` returning the key id, key and attestations (`rpc.proto:74`, `miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:51-97`). Confirm `submit_transaction` keeps taking `submission::ProvenTransactionSubmission` and stays never-retried
- [ ] T037 Port `769e2a90:crates/server/src/network/miden/execution/blockchain.rs` into `crates/server/src/network/miden/execution/blockchain.rs` as the FR-061 tip chain view: given one reference block `R` (the node's committed tip at attempt start) and a tracked set, build the peaks at forest `R` from a genesis-seeded `sync_chain_mmr` (T033) and each tracked block's path from T034, **adjusting any path returned at a later chain length to forest `R`** (copy miden-client's crate-private `adjust_merkle_path_for_forest` logic), and return the reference header, the `ProtocolConfig`, and a `PartialBlockchain`. Refuse, with a typed error the service maps to `GUARDIAN_EXECUTION_BINDING_MISMATCH`, a `ProtocolConfig` whose commitment differs from `header.protocol_config_commitment()` (FR-060), and a bound block whose commitment in the partial blockchain differs from the summary's `block_commitment`. Return a typed `ChainBehind { tip, bound_block }` when the tip is below the bound block (FR-056)
- [ ] T038 Port `769e2a90:crates/server/src/network/miden/execution/store.rs` into `crates/server/src/network/miden/execution/store.rs` as `ExecutionDataStore` implementing `miden_tx::DataStore` and `MastForestStore` at 0.17 (`miden-tx-0.17.0-rc.7/src/executor/data_store.rs:19-96`): `get_transaction_inputs` returns `(PartialAccount, BlockHeader, ProtocolConfig, PartialBlockchain)` from Guardian's stored account and the T037 chain view, and **adds the request's declared `block_numbers()` to the tracked set** because the executor builds `ref_blocks` from note blocks and `R` only (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:280-281`; lazy block-witness loading is a TODO, `src/host/tx_event.rs:491-494`), exactly as `ClientDataStore` does (`miden-client-0.17.0-rc.4/src/store/data_store/mod.rs:115-121,335-342`). Vault and storage-map witnesses come from the public `AccountSmtForest`, as in the spike. `get_foreign_account_inputs` delegates to T039
- [ ] T039 Implement `crates/server/src/network/miden/execution/foreign.rs` (FR-050): answer `get_foreign_account_inputs(id, ref_block)` for a **public** account from T035 at `R` with the witnesses requested, never caching across executions; return typed `ForeignAccountUnavailable { Private }` for a private account and `ForeignAccountUnavailable { Unavailable }` when the node cannot serve the state (including `BlockPruned`). This covers the fee faucet's asset callback (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/callbacks.masm:102-123`) and FEE_SPONSORSHIP pricing of network notes (`miden-standards-0.17.0-rc.7/asm/standards/fee/mod.masm:349-352`)
- [ ] T040 Implement `crates/server/src/network/miden/execution/sealing.rs` (FR-059): fetch the validator encryption key (T036), validate its attestations following miden-client's own flow (`miden-client-0.17.0-rc.4/src/transaction/mod.rs:779-845`), and seal the executed transaction's `TransactionInputs` with the public `seal_transaction_inputs` (`miden-client-0.17.0-rc.4/src/rpc/encryption.rs:445-460`), returning a `ProvenTransactionSubmission` ready to send. Every failure is a typed `SealingFailed` cause. Keep the attestation policy in one function so a stricter production policy (RFC question 10) replaces it in one place
- [ ] T041 Port the spike's offline tests from `769e2a90:crates/server/src/network/miden/execution/tests.rs` into `crates/server/src/network/miden/execution/tests.rs` at the 0.17 pins, and add MockChain tests: (a) unsigned reproduction, signature-advice injection, the Guardian authorization gate, authorized execution and proving pass for P2ID and a configuration change; (b) a summary bound to block `B` reproduces at `R > B + 50` with the bound block tracked and proven through the MMR at forest `R`; (c) omitting the bound block from the tracked set fails with `TransactionSummaryUnknownBlockNumber` (`miden-tx-0.17.0-rc.7/src/host/mod.rs:504-516`); (d) a `ProtocolConfig` mismatch is refused; (e) a path fetched at a longer chain is adjusted to forest `R` and verifies; (f) a public foreign faucet with the callback flag loads through T039 and a private one is refused. Reuse the existing contracts-test helpers that build guarded-multisig accounts with fee-asset balances
- [ ] T042 Port the spike's live tests from `769e2a90:crates/server/src/network/miden/execution/live_tests.rs` into `crates/server/src/network/miden/execution/live_tests.rs` (`#[ignore]`, `e2e`), pointed at devnet or a local 0.17 node: cold-start `sync_chain_mmr` to the tip, tracked-block paths adjusted to that tip, the `ProtocolConfig` for devnet's fee asset matching the header commitment, a public foreign account read at `R`, the encryption key fetched and its attestations validated, and a Guardian-assembled witness proved through the remote prover. Record the cold-start time, the seeding overhead and the proving times for SC-011 in `validation-matrix.md`

---

## Phase 3: User Story 1, hand a threshold-met proposal to Guardian (P1) 🎯 MVP

**Story goal**: A cosigner hands Guardian a threshold-met proposal; Guardian verifies, proves,
submits, and the account advances exactly as under self-execution.

**Independent test**: Create and sign a proposal to threshold, call execute as a cosigner, poll
to a terminal state, and confirm the resulting delta moved through the normal candidate to
canonical lifecycle with no new status values.

### Tests for User Story 1

- [ ] T043 [P] [US1] Add integration test `crates/server/src/services/execute_proposal/tests.rs`: a threshold-met Guardian-executable proposal is executed, polled to `committed`, and its delta canonicalized through the existing lifecycle, with **no new delta status value** (US1 scenarios 1 to 3)
- [ ] T044 [P] [US1] Add a test in `crates/server/src/services/execute_proposal/tests.rs` asserting the reported state progression is one of the permitted transitions only (`pending`, `proving`, `submitted`, `committed`) and never absent or ambiguous (US1 scenario 2, FR-024)
- [ ] T045 [P] [US1] Add a test in `crates/server/src/services/execute_proposal/tests.rs`: a below-threshold proposal is refused synchronously with `GUARDIAN_PROPOSAL_NOT_READY`, and a non-cosigner caller with `GUARDIAN_AUTHENTICATION_FAILED`, neither creating a reservation nor an execution record (US1 scenarios 4 and 5, FR-022)
- [ ] T046 [P] [US1] Add execution-not-found tests in `crates/server/src/services/execution_status.rs`: a status read for a proposal that exists but was never executed returns `404` / `GUARDIAN_EXECUTION_NOT_FOUND`, distinct from `GUARDIAN_PROPOSAL_NOT_FOUND`, while `GET /delta/execution/current` still answers `200 {"execution": null}` (SC-039)
- [ ] T047 [P] [US1] Add a tip-reproduction test in `crates/server/src/services/execute_proposal/tests.rs`: a proposal whose bound block is more than 50 blocks behind the tip executes at a fresh `R` and reaches `submitted`, and a test asserts the service never reads the proposal's `chain_anchor` metadata field (a proposal with the anchor removed executes identically) (SC-041, FR-056)

### Implementation for User Story 1

- [ ] T048 [US1] Extract the verify-and-acknowledge span of `push_delta` in `crates/server/src/services/push_delta.rs` (the part before it commits through the `DeltaCommitStrategy` seam) into `crates/server/src/services/ack_delta_internal.rs`, and have `push_delta` call it then commit exactly as today. `push_delta`'s existing tests stay untouched and passing
- [ ] T049 [US1] Make `crates/server/src/services/ack_delta_internal.rs` satisfy FR-044: apply the same delta verification and acknowledgment signing, persist **no** candidate, and do **not** set `has_pending_candidate`. Restrict visibility so no transport can reach it, and add a test in `crates/server/src/builder/handle.rs` asserting the router exposes no route that reaches it
- [ ] T050 [US1] Implement FR-045 **step 1** in `crates/server/src/services/execute_proposal/mod.rs`: select the distinct, valid, currently-registered cosigner signatures and confirm the effective per-procedure threshold read from account state (FR-005, FR-006)
- [ ] T051 [US1] Implement FR-045 **step 2** in `crates/server/src/services/execute_proposal/mod.rs`: decode the envelope through T025, deserialize the request, then refuse with `GUARDIAN_EXECUTION_REQUEST_INVALID` and the matching `meta.reason` a request whose `block_numbers()` lacks the summary's `block_number` (`bound_block_not_declared`), whose `auth_arg` is empty or whose preimage is absent from its advice map (`auth_args_missing`), whose summary user parameter 0 is zero (`approval_expiration_missing`), or that consumes notes not pinned through `explicit_input_notes` (`input_notes_not_pinned`). Take the bound block from the signed summary only, never from `chain_anchor`. Then read the chain tip and stop with `GUARDIAN_EXECUTION_EXPIRATION_REACHED` (`meta.bound: approval`) when the tip is at or past the approval expiration. No chain assembly happens before these checks pass (FR-051, FR-056, FR-057, FR-058)
- [ ] T052 [US1] Implement FR-045 **step 3** in `crates/server/src/services/execute_proposal/mod.rs`: choose `R` as the node's committed tip, build the T037 chain view with the bound block plus every authenticated note's creation block tracked, and construct the T038 `ExecutionDataStore`. A `ChainBehind` result stops the attempt with the retryable `GUARDIAN_EXECUTION_CHAIN_BEHIND` (FR-060, FR-061)
- [ ] T053 [US1] Implement FR-045 **step 4** in `crates/server/src/services/execute_proposal/mod.rs`: reproduce the transaction unsigned at `R` through `TransactionExecutor::execute_transaction(account_id, R, notes, tx_args)` with the stored auth arg passed through unchanged (Guardian never derives, attaches or overwrites fee conversion info, FR-057), and verify the reproduced summary commitment equals the signed one (FR-007). Step 4 **must** precede step 5
- [ ] T054 [US1] Implement FR-045 **steps 5 to 7** in `crates/server/src/services/execute_proposal/mod.rs`: issue the acknowledgment via the internal path (T049), inject the selected signature advice and the acknowledgment, and execute the authorized transaction at the same `R`, loading foreign public accounts lazily through T039. Keep the executed transaction's `TransactionInputs` in memory for step 10
- [ ] T055 [US1] Implement FR-045 **step 8** in `crates/server/src/services/execute_proposal/mod.rs`: re-verify the binding on the executed result, then read the chain height and stop with `GUARDIAN_EXECUTION_EXPIRATION_REACHED` (`meta.bound: transaction`) when the executed transaction's expiration block is at or below it (FR-058; the client applies the same check only on its anchored path, `miden-client-0.17.0-rc.4/src/transaction/mod.rs:379-389`)
- [ ] T056 [US1] Implement FR-045 **step 9** in `crates/server/src/services/execute_proposal/mod.rs`: prove via the remote prover with the configured explicit timeout (FR-019, FR-020). On error, walk the `std::error::Error::source` chain into the log, since the outermost `Display` is only "failed to prove transaction". Classify each failure transient or permanent and retry transient failures with capped backoff under the held, renewed reservation (FR-055): the transient class MUST match transport-level failures (connection errors, i/o timeouts, deadline-exceeded) found in the walked source chain, not only structured prover errors. Before each retry re-read the chain height and stop with `EXPIRATION_REACHED` (`meta.bound: transaction`) once the executed expiration is at or below it; stop and fail-and-release on a permanent error. Add stub-prover tests injecting both failure families (SC-040)
- [ ] T057 [US1] Implement FR-045 **step 10** in `crates/server/src/services/execute_proposal/mod.rs`: seal the retained `TransactionInputs` through T040, mapping any failure to `GUARDIAN_EXECUTION_SEALING_FAILED`, an ordinary fail-and-release. Sealing MUST complete before step 12 (FR-059)
- [ ] T058 [US1] Implement FR-045 **step 11** in `crates/server/src/services/execute_proposal/mod.rs`: re-check admissibility against freshly read state (FR-048: still at the base commitment, not paused or released, still guarded by this Guardian's acknowledgment key), then refuse with `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON` when `ProvenTransaction::expiration_block_num() - R` exceeds the configured horizon (FR-046). Both are before the boundary, so failure is fail-and-release; nothing waits for a far expiration to come within range
- [ ] T059 [US1] Implement FR-045 **step 12** in `crates/server/src/services/execute_proposal/mod.rs`: validate the fence, then call `admit_execution_candidate` (T015) as one storage call with evidence whose `reference_block` is `R` and whose `expiration_block` comes from the proven transaction. This is the no-retry boundary (FR-047); no code path may treat its failure as retryable
- [ ] T060 [US1] Implement FR-045 **steps 13 and 14** in `crates/server/src/services/execute_proposal/mod.rs`: re-validate the fence and, if stale, abort **without sending and without writing anything** (FR-049); otherwise send the step-10 `ProvenTransactionSubmission` once through `submit_transaction`, then classify the result via T061. From step 14 the canonicalization lifecycle owns promotion, the execution itself resolves a definite rejection via T062, and reconciliation owns an unknown outcome
- [ ] T061 [US1] Add a typed submission-result adapter in `crates/server/src/services/execute_proposal/submission_result.rs`: map node and transport outcomes to `Definite(rejection)` versus `Unknown`, where only an explicit application-level rejection is definite and every ambiguous transport failure (timeout, dropped connection, unavailable) defaults to `Unknown`. Do not branch on free-form error text
- [ ] T062 [US1] Consume `Definite(rejection)` in `crates/server/src/services/execute_proposal/mod.rs`: call `resolve_execution` (T018) to discard the candidate, delete the proposal, persist `failed` / `GUARDIAN_EXECUTION_SUBMISSION_REJECTED`, and release the reservation. An `Unknown` result leaves the reservation held for reconciliation (FR-032)
- [ ] T063 [US1] Implement request orchestration in `crates/server/src/services/execute_proposal/mod.rs`: synchronous FR-022 admission checks, per-account lease acquisition, reservation creation, background dispatch, immediate `202`, so refusals happen on the caller's thread and everything from step 1 on runs off it. Every pre-boundary failure path calls `fail_execution` (T020) with its code and `error_meta`. Include a restart test asserting a dispatched-but-unstarted execution is recovered or failed, never silently lost
- [ ] T064 [US1] Implement attempt identity across `crates/server/src/storage/postgres.rs`, `crates/server/src/storage/filesystem.rs`, and `crates/server/src/services/execution_status.rs`: rows are keyed `(account_id, proposal_id, attempt)`; the next attempt number is allocated inside reservation creation, under the account lock, as `max(attempt) + 1`; an unqualified status read reports the most recent attempt. The wire handle stays `(account_id, proposal_id)` (FR-003, FR-042)
- [ ] T065 [US1] Implement reported-state derivation in `crates/server/src/services/execution_status.rs`: derive the five states (FR-024) from the reservation, the delta, and `ExecutionOutcome`, using the internal-phase mapping table in `data-model.md`, and surface `error.code`, `error.message` and the typed `error.meta` on `failed`. Pre-terminal states are derived and never persisted
- [ ] T066 [US1] Wire the `committed` outcome through the extended `promote_candidate` in `crates/server/src/jobs/canonicalization/processor.rs`, using T019. `execution_reconcile` MUST NOT also write `committed` on observing `canonical` (FR-041, FR-053, SC-025)
- [ ] T067 [US1] Add `POST /delta/proposal/execution` to `crates/server/src/api/http.rs` with `#[utoipa::path]`, per-status responses and `security(...)`: `202` with `newly_accepted: true` on acceptance, `200` with `newly_accepted: false` when idempotently returning an active execution
- [ ] T068 [US1] Add `GET /delta/proposal/execution` and `GET /delta/execution/current` to `crates/server/src/api/http.rs` with `#[utoipa::path]`. The current-execution read returns `200 {"execution": null}` when nothing is in flight, never `404` (FR-036)
- [ ] T069 [US1] Add the three gRPC methods to `crates/server/proto/guardian.proto` and implement them in `crates/server/src/api/grpc.rs` with semantics identical to HTTP, carrying `newly_accepted`, `proposal_exists`, `ignored_signatures`, and `error.meta` on the envelope (FR-034)
- [ ] T070 [US1] Wire the routes in `crates/server/src/builder/handle.rs` following the existing flat-path, query-parameter style (compare `/delta/proposal/single`, `/delta/candidate/abandon`), and derive `ToSchema` / `IntoParams` on the new wire types

**Checkpoint**: MVP. A cosigner can hand Guardian a signed proposal and it lands. Requires
Phases 1, 2A and 2B.

---

## Phase 4: User Story 2, execution is bound to what the cosigners actually signed (P1)

**Story goal**: Every mismatch (superseded state, tampered request, structurally unexecutable
request, reached expiration, insufficient valid signatures, unavailable foreign state) stops
execution before any proving or submission, with a distinguishable cause and no side effects.

**Independent test**: Drive execution against each cause below and confirm the outcome, the
code and `meta`, and that refusals leave no trace.

### Tests for User Story 2

- [ ] T071 [P] [US2] Add `crates/server/src/services/execute_proposal/binding_tests.rs`: an account advanced past the proposal's base is refused with `GUARDIAN_EXECUTION_STATE_MISMATCH` before any proving or submission (US2 scenario 1)
- [ ] T072 [P] [US2] Add to `crates/server/src/services/execute_proposal/binding_tests.rs`: a stored request mutated so it no longer reproduces the signed summary is refused with `GUARDIAN_EXECUTION_BINDING_MISMATCH`, distinguishable from not-ready and state-mismatch (US2 scenario 2)
- [ ] T073 [P] [US2] Add to `crates/server/src/services/execute_proposal/binding_tests.rs`: a proposal with an invalid, duplicate, or non-cosigner entry alongside enough valid signatures executes, with the bad entry ignored and counted in `ignored_signatures`; one whose valid set is below threshold is refused as `GUARDIAN_PROPOSAL_NOT_READY`, not as a signature or binding error (US2 scenario 3, FR-006, SC-020)
- [ ] T074 [P] [US2] Add to `crates/server/src/services/execute_proposal/binding_tests.rs`: after **any** refusal or pre-boundary failure, the account is not locked, no delta was recorded, no reservation is held, and the proposal is still executable by its cosigners (US2 scenario 4, FR-032, SC-006)
- [ ] T075 [P] [US2] Add codec-rejection tests to `crates/server/src/services/execute_proposal/binding_tests.rs`: a bad checksum is rejected before deserialization; a `protocol_line` other than `"0.17"`, an unsupported `format_version`, and a same-line unallowlisted `serializer_id` (for example rc.3 bytes on an rc.4 server) are each refused before deserialization with the contract's codes (FR-014, FR-015, SC-010, SC-029)
- [ ] T076 [P] [US2] Add `REQUEST_INVALID` tests to `crates/server/src/services/execute_proposal/binding_tests.rs`, one per `meta.reason` (`bound_block_not_declared`, `auth_args_missing` for both an empty auth arg and a missing preimage, `approval_expiration_missing`, `input_notes_not_pinned`), each asserting no chain read happened and the reservation was released (SC-041, FR-056, FR-057, FR-051)
- [ ] T077 [P] [US2] Add expiration-reached tests to `crates/server/src/services/execute_proposal/binding_tests.rs` for all three placements: the tip already at or past the approval expiration at step 2; the auth procedure's `ERR_MULTISIG_APPROVAL_EXPIRED` abort when the tip passes the expiration between step 2 and reproduction; and an executed expiration reached before a proving retry. Each reports `GUARDIAN_EXECUTION_EXPIRATION_REACHED` with the matching `meta.bound` (SC-042, FR-058)
- [ ] T078 [P] [US2] Add foreign-account tests to `crates/server/src/services/execute_proposal/binding_tests.rs`: a transaction whose fee faucet has the callback flag executes with the faucet read at `R`; a script invoking a public foreign account executes; a private foreign account yields `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` / `private`; pruned or unservable foreign state yields `/ unavailable`, all before proving (SC-032, FR-050)
- [ ] T079 [P] [US2] Add fee tests to `crates/server/src/services/execute_proposal/binding_tests.rs`: an account without enough native fee asset fails with `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`, not a binding mismatch; a reproduction whose TX_FEE note differs from the signed one (fee parameters changed between proposer and Guardian) is `GUARDIAN_EXECUTION_BINDING_MISMATCH` and the log line names both fee notes; a `ProtocolConfig` mismatch is `GUARDIAN_EXECUTION_BINDING_MISMATCH` before reproduction (SC-044, FR-057, FR-060)
- [ ] T080 [P] [US2] Add a chain-behind test to `crates/server/src/services/execute_proposal/binding_tests.rs`: a node whose tip is below the summary's bound block yields `GUARDIAN_EXECUTION_CHAIN_BEHIND` with the reservation released, and a later request after the node catches up succeeds (FR-056)
- [ ] T081 [P] [US2] Add a signature-provenance test in `crates/server/src/services/execute_proposal/binding_tests.rs`: signatures Guardian holds are used directly, while a proposal whose threshold is met only by offline-collected signatures not present server-side is refused as not-ready (FR-018)

### Implementation for User Story 2

- [ ] T082 [US2] Harden signature-subset selection in `crates/server/src/services/execute_proposal/mod.rs`: verify each stored entry against the signed commitment and the registered cosigner set, excluding invalid, duplicate, and non-cosigner entries rather than failing, record which were ignored and why in the log, and surface the count as `ignored_signatures` (FR-006). There is deliberately no signature-invalid error code
- [ ] T083 [US2] Compute the **effective per-procedure** threshold in `crates/server/src/services/execute_proposal/mod.rs` from account state, using a static, exhaustively handled mapping from proposal type to invoked procedure with the account default for custom types (FR-005, FR-013), and refuse below it synchronously as `GUARDIAN_PROPOSAL_NOT_READY` (FR-022)
- [ ] T084 [US2] Map execution failures to codes by **typed** error matching in `crates/server/src/services/execute_proposal/mod.rs`, never by error text: the auth procedure's approval-expired abort to `EXPIRATION_REACHED` / `approval`; a fee-payment abort for insufficient balance to `INSUFFICIENT_FEE`; a summary mismatch to `BINDING_MISMATCH` with a diagnostic log comparing the signed and reproduced TX_FEE output notes; typed `ForeignAccountUnavailable` to its code and reason. Decode MASM error codes the way the repo already does for contract errors, and add a test per mapping
- [ ] T085 [US2] Ensure every FR-022 refusal in `crates/server/src/services/execute_proposal/mod.rs` returns before any reservation is created or execution record written, that each maps to a distinct stable code, and that `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` names a proposal with no stored request without ever attempting to rebuild one from metadata (FR-010, FR-013, SC-003)
- [ ] T086 [US2] Enforce the FR-016 per-request size cap and the per-account aggregate cap at proposal creation in `crates/server/src/services/push_delta_proposal.rs`: count **decoded** request bytes, count only viable proposals, and run check-plus-insert atomically under the account lock, rejecting an oversized or malformed envelope at creation rather than at execution. Add a concurrent test where two creations against an account one slot below the cap yield exactly one acceptance
- [ ] T087 [US2] Record the maintainers' decision on the FR-016 per-proposer viable-count quota (default proposal: two per authenticated proposer) and how account-wide capacity is allocated so one proposer cannot consume another signer's share, in `speckit/features/254-guardian-prove-and-commit/spec.md` FR-016 and `contracts/execution-api.md`, including the creation-error code. `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` (default 20, `crates/server/src/services/push_delta_proposal.rs`) is the only cap on `main` today
- [ ] T088 [US2] Implement the T087 per-proposer quota atomically with insertion on both backends in `crates/server/src/services/push_delta_proposal.rs` and the storage layer, with identity from authentication, not metadata. Test concurrent creates, stale proposals freeing capacity, and a capped proposer not blocking another signer. Propagate the creation error and config through both base clients, both SDKs and `docs/CONFIGURATION.md`
- [ ] T089 [P] [US2] Add a test in `crates/server/src/services/push_delta_proposal.rs` verifying creation and signature collection never execute a request or reserve an account, that a proposal ready at creation still needs an explicit execution request, and that the Guardian acknowledgment cannot substitute for a missing cosigner signature, for Falcon and ECDSA accounts
- [ ] T090 [US2] Implement FR-017 stored-request cleanup in `crates/server/src/jobs/canonicalization/processor.rs`: when a proposal is deleted on promotion or discard, its stored transaction request goes with it, so the aggregate cap cannot be consumed by dead proposals

**Checkpoint**: delegating execution is now safe, not merely convenient.

---

## Phase 5: User Story 4, one submission even under concurrency and crashes (P1)

**Story goal**: At most one lease-authorized proving attempt and at most one on-chain submission,
with no account left indefinitely locked.

**Independent test**: Concurrent execution requests from multiple callers and replicas yield
exactly one submission; a replica killed mid-proof releases its reservation; a submission timeout
reports `submitted`, retains the reservation, and refuses retry until the chain is observed.

### Tests for User Story 4

- [ ] T091 [P] [US4] Add `crates/server/src/services/execute_proposal/concurrency_tests.rs` (Postgres, `#[ignore]`): two concurrent execute requests for one proposal create exactly one reservation, and the second observes the first execution (US4 scenario 1, SC-005)
- [ ] T092 [P] [US4] Add to `crates/server/src/services/execute_proposal/concurrency_tests.rs`: two replicas observing the same queued execution, exactly one proves and submits (US4 scenario 2)
- [ ] T093 [P] [US4] Add `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a crash injected immediately before the step-12 write fails-and-releases; a crash immediately after it reconciles and never retries or re-proves (SC-024, SC-030)
- [ ] T094 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: the fence is stolen between step 12 and step 14, nothing is sent, and the durable candidate is left to reconciliation (SC-031, FR-049)
- [ ] T095 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a lease expires mid-flight, ownership transfers, the new owner resolves the outcome, and the original worker resumes without submitting (SC-019, FR-038)
- [ ] T096 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a key-fetch failure, an attestation-validation failure, and a sealing failure are each injected at step 10 and settle `GUARDIAN_EXECUTION_SEALING_FAILED` with the reservation released and nothing sent; no path reaches step 12 without a sealed submission (SC-043, FR-059)
- [ ] T097 [P] [US4] Add the **self-deadlock regression test** to `crates/server/src/services/execute_proposal/concurrency_tests.rs`: Guardian admits its own candidate under its own reservation and the sequence completes (SC-028, FR-037, FR-044)
- [ ] T098 [P] [US4] Add to `crates/server/src/services/execute_proposal/concurrency_tests.rs`: while a reservation is active, a client `push_delta` for the same account is refused with `GUARDIAN_EXECUTION_CONFLICT` (US4 scenario 3, FR-027)
- [ ] T099 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a replica killed while proving has its lease expire, the execution reports `failed` / `GUARDIAN_EXECUTION_LEASE_EXPIRED`, the reservation is released, and the proposal remains executable (US4 scenario 4, FR-028, SC-007)
- [ ] T100 [P] [US4] Add to `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a submission timeout reports `submitted`, refuses retry, retains the reservation, and resolves only after chain observation; then chain observation unavailable across the recorded expiration height keeps the execution `submitted` and the reservation held, fires outage health and metrics, and resumes resolution after observation recovers (US4 scenario 5, FR-030, FR-040, SC-008)
- [ ] T101 [P] [US4] Add horizon tests to `crates/server/src/services/execute_proposal/tests.rs`: a built-in proposal (proven expiration `R + min(256, approval_expiration - R)`) passes the default horizon; a custom request with a 28,800-block approval window and no scripted delta is refused with `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON` before the boundary; a non-expiring transaction (both bounds absent, `u32::MAX`) cannot reach the boundary (SC-027, SC-033, FR-046)
- [ ] T102 [P] [US4] Add a definite-rejection test in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: an explicit node rejection discards the candidate, deletes the proposal, releases the reservation and reports `GUARDIAN_EXECUTION_SUBMISSION_REJECTED` with `proposal_exists: false`; an ambiguous transport failure in the same position reports `submitted` and retains the reservation (FR-030, SC-037)
- [ ] T103 [P] [US4] Add a `SwitchGuardian` fault-injection test in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a `SwitchGuardian` canonicalizing during proving fails the execution at the step-11 admissibility re-check with nothing submitted (SC-026, FR-048)
- [ ] T104 [P] [US4] Add terminal-persistence tests in `crates/server/src/services/execute_proposal/fault_injection_tests.rs`: a post-submission failure whose candidate and proposal were deleted still reports `failed` with a cause and `proposal_exists: false`; promotion and discard each persist the outcome inside their own transaction (SC-021, SC-025)

### Implementation for User Story 4

- [ ] T105 [US4] Implement lease renewal ownership: the execution worker heartbeats its own `execution:{account_id}` lease for the whole pre-boundary span, including proving and its retries, in `crates/server/src/services/execute_proposal/mod.rs`; after a fenced ownership transfer only the reconciliation owner heartbeats, in `crates/server/src/jobs/execution_reconcile/mod.rs`. Reconciliation MUST NOT renew a pre-boundary worker's lease. Test both: a proof outlasting one lease period keeps its reservation, and a killed pre-boundary worker's lease expires on schedule (FR-023, FR-028)
- [ ] T106 [US4] Add the reservation lease loop to `crates/server/src/jobs/execution_reconcile/mod.rs`: on expiry branch on whether the boundary was crossed, failing and releasing before it (`LEASE_EXPIRED`), transferring to reconciliation through `claim_execution_reservation` after it (FR-028)
- [ ] T107 [US4] Add the reservation refusal to `crates/server/src/services/push_delta.rs`: while a reservation is active for the account, refuse with `GUARDIAN_EXECUTION_CONFLICT` and `meta.blocking_proposal_id` from `AlreadyReserved.proposal_id` (FR-027, FR-036, SC-017)
- [ ] T108 [US4] Implement the **superseded** evidence path in `crates/server/src/jobs/execution_reconcile/mod.rs`: the account moved to a commitment that is neither `base_commitment` nor `expected_commitment` resolves through `resolve_execution` as `failed` / `GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` (FR-040)
- [ ] T109 [US4] Implement the **expired** evidence path in `crates/server/src/jobs/execution_reconcile/mod.rs`: the chain observed strictly past `expiration_block` with the account still at `base_commitment` resolves as `failed` / `GUARDIAN_EXECUTION_EXPIRED` (FR-040). On unavailable observation, retain the reservation, retry with capped backoff, and expose health, metrics and logs; never settle or release on wall-clock time
- [ ] T110 [US4] Use `SyncTransactions(block_range, account_ids)` through the existing `sync_transactions` in `crates/miden-rpc-client/src/lib.rs` as a faster observation input in `crates/server/src/jobs/execution_reconcile/mod.rs`: an included transaction whose id matches the evidence tells reconciliation to wait for promotion; it is an input only and never writes `committed` (FR-040, `rpc.proto:90,798-841`)
- [ ] T111 [US4] Implement restart recovery in `crates/server/src/jobs/execution_reconcile/mod.rs`: an execution whose `SubmissionEvidence` exists is never retried, only reconciled; one without it fails-and-releases as `GUARDIAN_EXECUTION_ABANDONED` (FR-031). Recovery reads the evidence, not a phase column
- [ ] T112 [US4] Implement stale-result rejection in `crates/server/src/services/execute_proposal/mod.rs`: a proving result produced by a worker whose fence is no longer current is never carried forward (FR-029)
- [ ] T113 [US4] Add a single-writer assertion for terminal outcomes in `crates/server/src/jobs/execution_reconcile/mod.rs`: reconciliation observing the account at `expected_commitment` waits for promotion and writes nothing, and a test asserts no code path other than the extended `promote_candidate` (T019) writes `committed` (FR-053, SC-036)
- [ ] T114 [US4] Implement `proposal_exists` on the envelope in `crates/server/src/services/execution_status.rs` as a fact read from storage, never retry advice, matching the truth table in `contracts/execution-api.md` (FR-042)

**Checkpoint**: the custody-safety argument holds under concurrency, crashes, and ambiguous
submissions.

---

## Phase 6: User Story 3, configuring the client and self-execution staying intact (P2)

**Story goal**: One client-level setting decides whether proposals are Guardian-executable,
default off, no existing method signature changes, self-execution untouched.

**Independent test**: Create the same transaction type through a default client and a
Guardian-executable client; confirm the default payload is byte-identical to today's and its
Guardian execution is refused; confirm the other succeeds; confirm local execution works for
both; confirm creation succeeds against a server with proving disabled.

### Tests for User Story 3

- [ ] T115 [P] [US3] Add a payload-shape test in `crates/miden-multisig-client/src/` asserting a default-client proposal carries no `transaction_request` and is shape-identical to a pre-feature proposal (US3 scenarios 1 and 5, SC-009)
- [ ] T116 [P] [US3] Add identity tests in `crates/miden-multisig-client/src/`: attaching the envelope does not change the proposal id, which derives from the summary alone (FR-012); and the same effects built under `GuardianExecutable` and `SelfExecuted` produce **different** ids, because the approval expiration and the 256-block delta are signed (SC-033)
- [ ] T117 [P] [US3] Add TypeScript equivalents of T115 and T116 in `packages/miden-multisig-client/tests/`
- [ ] T118 [P] [US3] Add tests in `crates/miden-multisig-client/src/` and `packages/miden-multisig-client/tests/` confirming list, review, sign, and export behave identically for proposals with and without the stored request (US3 scenario 6)
- [ ] T119 [P] [US3] Add empty-store reproduction tests for consume-notes in `crates/miden-multisig-client/src/transaction/consume.rs` and `packages/miden-multisig-client/tests/`: a fresh client with an empty store reproduces the proposer's summary from the request bytes alone, at a later tip than the proposer's, when the notes are pinned through `explicit_input_notes`

### Implementation for User Story 3

- [ ] T120 [US3] Add client-level `ProposalExecutionMode { SelfExecuted, GuardianExecutable }` and `MultisigClientBuilder::execution_mode` to `crates/miden-multisig-client/src/`, defaulting to `SelfExecuted`, with no changes to existing signatures, per `contracts/sdk-api.md`
- [ ] T121 [US3] Attach the `transaction_request` envelope when the mode is `GuardianExecutable` in `crates/miden-multisig-client/src/`: `format_version`, `protocol_line` `"0.17"`, `serializer_id` = the pinned `miden-client` version including prerelease (for example `0.17.0-rc.4`, not the Guardian crate version), SHA-256 `checksum` as `0x` lowercase hex, and base64 `bytes`. The client never asks the server what it supports (US3 scenario 4)
- [ ] T122 [US3] Add the shared constants `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA = 28_800` and `GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA = 256` to `crates/miden-multisig-client/src/transaction/` and apply them under `GuardianExecutable` (FR-051): the approval default through `ProposalOptions.approval_expiration_delta` when the caller passes none (`transaction/builder.rs`); the transaction delta for every built-in family through `TransactionRequestBuilder::expiration_delta` for no-script requests and `tx::update_expiration_block_delta` inside Guardian-owned scripts (the builder rejects `expiration_delta` with a custom script, `miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:305,682-687`). Custom producers keep their opaque request bytes unchanged; document on `MultisigClient::multisig_auth_args` that Guardian execution needs a non-zero approval delta and SHOULD have the 256-block delta scripted
- [ ] T123 [US3] Pin every consumed note in Guardian-executable consume-notes requests in `crates/miden-multisig-client/src/transaction/consume.rs` (FR-056): build with `TransactionRequestBuilder::explicit_input_notes` (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:178-190`) carrying each authenticated `InputNote` with its proof, instead of relying on store classification in `ensure_notes_authenticated`
- [ ] T124 [US3] Add `request_guardian_execution`, `execution_status` and `current_execution` to the Rust multisig SDK in `crates/miden-multisig-client/src/client/`, delegating to the base client (T127) and surfacing `error.meta` as the typed enums
- [ ] T125 [P] [US3] Mirror T120 to T124 in `packages/miden-multisig-client/src/`: `executionMode` on `MultisigClientConfig`, the envelope with `serializer_id` = the `@miden-sdk/miden-sdk` embedded client version, the same two constants applied by the typed request-building methods (`createP2idProposal`, `createConsumeNotesProposal`, signer-set and threshold methods, `createSwitchGuardianProposal`) with `createCustomProposal` attaching the producer's bytes unchanged, pinned consume-notes through the web SDK's explicit-input-notes equivalent, `requestGuardianExecution` / `executionStatus` / `currentExecution`, and the low-level `createProposal(nonce, txSummaryBase64, metadata)` refusing explicitly on a `guardian_executable` client
- [ ] T126 [US3] Take the bound block from the signed summary rather than `anchor.blockNum()` in the TypeScript request rebuild in `packages/miden-multisig-client/src/multisig.ts` (`proposalRequestBinding`), matching Rust's `summary.block_number()`, and add a test that a TS proposal whose anchor names a different block fails the same way on both SDKs (N3)
- [ ] T127 [P] [US3] Add the three execution calls to the Rust base client `crates/client/`, which has no Miden dependency (FR-034)
- [ ] T128 [P] [US3] Mirror T127 in the TypeScript base client `packages/guardian-client/src/`
- [ ] T129 [US3] Add every new error code to the TypeScript error-code vocabulary in `packages/guardian-client/src/` with the closed `meta.reason` / `meta.bound` value sets as typed unions, and the same in the Rust base client. `ANCHOR_EXPIRED`, `NO_FINITE_EXPIRATION` and `FOREIGN_INPUTS_UNSUPPORTED` MUST NOT appear. Rebuild `packages/guardian-client` and relink it into `packages/miden-multisig-client` before running that package's tests, since it typechecks against the published client otherwise
- [ ] T130 [P] [US3] Commit cross-language fixtures under `fixtures/miden-multisig-client/`: identical `format_version`, `protocol_line`, `serializer_id` and `checksum` from both SDKs for the same transaction; the same summary and proposal id for the same effects, salt and bound block under `GuardianExecutable`, including both bounds; a different-protocol-line fixture; a same-line unallowlisted-serializer fixture (rc.3 bytes); and an unsupported-format fixture, all wired into both SDK tests and the server codec tests (SC-029, SC-010)
- [ ] T131 [P] [US3] Add cross-SDK parity tests in `packages/miden-multisig-client/tests/execution-parity.test.ts` and the Rust SDK: both use 28,800 and 256, derive the same summary for the same inputs, emit the same `serializer_id` semantics, pin consume-notes identically, and expose the same state and error vocabularies (US3 scenario 7, FR-033, SC-013)
- [ ] T132 [P] [US3] Add an override-versus-default test in `crates/miden-multisig-client/src/` and `packages/miden-multisig-client/tests/`: a per-procedure threshold override is honored where present and the account default applies where absent (SC-004)
- [ ] T133 [US3] Add a no-regression suite for the untouched paths in `examples/demo` (Rust) and `examples/smoke-web` (TypeScript), covering a built-in and a custom proposal type: self-execution, export, offline signing, and local execution of an imported proposal (SC-012, FR-035)
- [ ] T134 [US3] Create the `examples/execution-smoke` harness: request Guardian execution and poll to `committed` using only `packages/guardian-client` plus a Rust counterpart over `crates/client`, constructing no Miden client and connecting to no node. This is the only artifact that can evidence the no-Miden guarantee (SC-001, FR-034)
- [ ] T135 [US3] Update `examples/demo` and `examples/smoke-web` for the full Guardian-execution lifecycle (propose as Guardian-executable, sign to threshold, request execution, observe on-chain commitment) for a built-in and a custom proposal type against devnet or a local 0.17 node, funding the account first through `scripts/devnet-register-account.sh` on devnet. `examples/smoke-web` MUST NOT be claimed for SC-001, since it constructs a Miden client (SC-015)

**Checkpoint**: integrators opt in explicitly; nothing changes for anyone who does not.

---

## Phase 7: User Story 5, operators control whether the capability exists (P3)

**Story goal**: An operator decides whether this deployment offers prove-and-commit and where
proving happens; a server without a prover refuses explicitly.

**Independent test**: Start with no prover (refused, nothing attempted); with the capability
disabled (same error class regardless of prover reachability); with a reachable prover
(succeeds).

### Tests for User Story 5

- [ ] T136 [P] [US5] Add `crates/server/src/services/execute_proposal/capability_tests.rs`: with no prover configured, execution is refused with `GUARDIAN_PROVING_UNAVAILABLE` and nothing is proven or submitted (US5 scenario 1, FR-021)
- [ ] T137 [P] [US5] Add to `crates/server/src/services/execute_proposal/capability_tests.rs`: with the capability disabled, the same error class is returned regardless of prover reachability, for 100% of requests (US5 scenario 2, SC-014)
- [ ] T138 [P] [US5] Add to `crates/server/src/services/execute_proposal/capability_tests.rs`: a configured but unreachable prover still accepts the request, and the execution reports `failed` / `GUARDIAN_EXECUTION_PROVING_FAILED` once retries stop (US5 scenario 3)
- [ ] T139 [P] [US5] Add a startup test in `crates/server/src/services/execute_proposal/capability_tests.rs`: execution enabled with canonicalization disabled refuses to start, naming the misconfiguration (SC-022, FR-043)
- [ ] T140 [P] [US5] Add a proving-disabled creation test in `crates/server/src/services/push_delta_proposal.rs`: with proving disabled, creating a Guardian-executable proposal still succeeds and stores the envelope; the mismatch surfaces only at execution (US3 scenario 4, FR-009)

### Implementation for User Story 5

- [ ] T141 [US5] Gate the execution endpoints on the capability in `crates/server/src/services/execute_proposal/mod.rs`: unset prover, disabled kill-switch, a build without the `proving` feature, or optimistic mode all return `GUARDIAN_PROVING_UNAVAILABLE` with no fallback (FR-021, FR-043)
- [ ] T142 [US5] Add `proving` to the published-image feature list: `Dockerfile` (`ARG GUARDIAN_SERVER_FEATURES=postgres`), the compose guides under `docs/guides/`, and `docs/SERVER_AWS_DEPLOY.md`. Without it every published-image deployment answers execute with `GUARDIAN_PROVING_UNAVAILABLE` with no log line naming a build-time cause (FR-021)

**Checkpoint**: safe to ship to a shared deployment.

---

## Phase 8: Polish and Cross-Cutting Concerns

- [ ] T143 [P] Regenerate the committed OpenAPI specs with `cargo run --features evm --bin gen-openapi -- docs` (AGENTS.md §4) and commit the result
- [ ] T144 [P] Document the three endpoints, the five-state vocabulary, every error code with its `meta` values, and the envelope in `spec/api.md`, and the execution sequence (fourteen steps, boundary at step 12) with its diagram in `spec/processes.md`
- [ ] T145 [P] Document the eight configuration variables and the serializer allowlist in `docs/CONFIGURATION.md`, including the horizon's minimum of 256 and the rule that a custom request without a scripted delta is refused under the default horizon
- [ ] T146 [P] Add to `docs/TROUBLESHOOTING.md`: the prover-timeout failure mode (the client library's 10 s default, `miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`, surfaces as an intermittent "failed to prove transaction" naming no timeout), `CHAIN_BEHIND`, `INSUFFICIENT_FEE` with devnet funding, `EXPIRATION_BEYOND_HORIZON` for custom producers, and a fee-drift `BINDING_MISMATCH`
- [ ] T147 [P] Add execution metrics in `crates/server/src/metrics/`: executions by terminal state and error code, chain-view assembly duration, proving duration and retries, sealing failures, reservation age, reconciliation outcomes by evidence path, and chain-observation outage duration. Internal phases are diagnosable from metrics, health and logs, never from the wire (FR-025, FR-040)
- [ ] T148 [P] Add transport-parity tests in `crates/server/src/api/execution_tests.rs` covering every case in the HTTP and gRPC parity table, including `error.meta`, verified per case (SC-016, SC-023)
- [ ] T149 [P] Update `docs/MULTISIG_SDK.md` with the client-level execution mode, the two expiration bounds and their constants, pinned consume-notes, and the custom-producer obligations; and `docs/MIDEN_COMPATIBILITY.md` with Guardian execution on the 0.18.x / Miden 0.17 line, the rc serializer allowlist, and the production gate on stable 0.17
- [ ] T150 [P] Annotate feature 008's FR-015 in `speckit/features/008-custom-proposal-producer/spec.md` with the narrowing this feature introduces: the serialized transaction request is now persisted, but only for proposals from a Guardian-executable client
- [ ] T151 Run the Stage 2 live submission on devnet (or a local 0.17 node): a funded, Guardian-registered 2-of-2 guarded multisig, a P2ID and a consume-notes proposal executed by Guardian more than 50 blocks after their bound blocks, with sealed submission, and record request sizes, seeding overhead (SC-011) and proving times against the historical 0.16 figures in `validation-matrix.md` and RFC 0001 Appendix A.4
- [ ] T152 Run the full validation matrix in [validation-matrix.md](./validation-matrix.md) and confirm every success criterion SC-001 to SC-044 has a passing test or a recorded, justified exception

---

## Dependencies

```text
Phase 1 (Setup)
   ↓
Phase 2A (storage + admission) ── GATE: T026 + T027
Phase 2B (0.17 execution seam) ── GATE: T041 + T042   (2A and 2B can run in parallel)
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

- **Phases 2A and 2B before any user story.** The admission primitive and the execution seam are
  the foundation; a bug in either is inherited by every phase.
- **Phase 5 must not be deferred.** Reconciliation is what makes the no-retry boundary survivable.
- **T048 and T049 before T054.** The internal ack path must exist before the sequence uses it.
- **T051 before T052, T053 before T054.** Structural checks precede chain assembly;
  reproduction precedes acknowledgment.
- **T057 and T058 before T059.** Sealing, admissibility and the horizon are all pre-boundary.
- **T059 is the boundary.** Every task after it treats submission as authorized and prepared,
  forbids another proof or send, and reconciles only.
- **T087 before T088.** The per-proposer quota needs a maintainer decision first.
- **Phase 6 after Phase 3.** Constitution Principle I: the server contract drives the clients.
- **T129 before T124 and T125 are tested.** The multisig SDKs typecheck against the base client.

## Parallel opportunities

| Phase | Parallel set |
|---|---|
| 1 | T002 to T005 |
| 2A | T012 to T014, T024, T028 to T031 |
| 2B | T033 to T036 (four independent RPC client calls), then T039 and T040 alongside T037 |
| 3 | T043 to T047 (tests, before implementation) |
| 4 | T071 to T081 (test tasks, independent cases), T089 |
| 5 | T091 to T104 (the whole fault-injection and concurrency test set) |
| 6 | T115 to T119, T125, T127, T128, T130 to T132 |
| 7 | T136 to T140 |
| 8 | T143 to T150 |

T010 and T011 are **not** parallel in practice: the Postgres implementation establishes the
semantics the filesystem one must match, so write Postgres first and treat T026 as the parity
check. T037 and T038 share the chain-view types, so write T037 first.

### Parallel example: Phase 2B

```text
Agent 1: T033 sync_chain_mmr port          (crates/miden-rpc-client/src/lib.rs, conversion.rs)
Agent 2: T035 get_account_at               (crates/miden-rpc-client/src/lib.rs, separate fn)
Agent 3: T036 get_transaction_encryption_key
Then:    T037 blockchain.rs -> T038 store.rs, with T039 foreign.rs and T040 sealing.rs in parallel
Gate:    T041 offline tests, T042 live tests
```

### Parallel example: User Story 2 tests

```text
T076 REQUEST_INVALID reasons | T077 expiration placements | T078 foreign accounts
T079 fee and ProtocolConfig  | T080 chain behind          | T081 signature provenance
```

## Implementation strategy

**MVP = Phases 1, 2A, 2B and 3.** That is heavier than a typical MVP because FR-023 requires the
durable reservation for the whole span and 0.17 requires the ported seam before anything
executes, so there is no smaller increment that executes even one proposal correctly. Stopping
after Phase 3 gives a working happy path with unsafe failure handling: a demo, not shippable.

**Smallest shippable increment = Phases 1 to 5.** US1, US2 and US4 are all P1 and together form
the correctness argument: the capability, its binding guarantee, and its single-submission
guarantee. Phases 6 and 7 add integrator ergonomics and operator control.

**Recommended sequence**: run 2A and 2B in parallel, with T026/T027 and T041/T042 green before
writing any service code. Then drive Phase 3 to a working `committed` against MockChain, follow
immediately with Phase 5's fault injection, and only then attempt live devnet runs (T042, T151).
Production release waits for stable Miden 0.17 and the re-pin.

## Task count

| Phase | Tasks | Of which tests |
|---|---|---|
| 1: Setup | 5 | 0 (defaults asserted inside T005) |
| 2A: Foundational, storage | 27 | 7 |
| 2B: Foundational, 0.17 seam | 10 | 2 |
| 3: US1 (P1) | 28 | 5 |
| 4: US2 (P1) | 20 | 12 |
| 5: US4 (P1) | 24 | 14 |
| 6: US3 (P2) | 21 | 8 |
| 7: US5 (P3) | 7 | 5 |
| 8: Polish | 10 | 1 |
| **Total** | **152** | **54** |

Relative to the 0.16 task set (129 tasks), the net growth is the 0.17 execution seam (Phase 2B:
RPC surface, tip chain view, foreign accounts, sealing), the step-2 structural checks and their
tests, the reached-expiration, fee, chain-behind and sealing failure tests, the two SDK
expiration constants, pinned consume-notes in both SDKs, the TypeScript bound-block fix, and the
Stage 2 live run. The 0.16 anchor admission, anchor header comparison, fee-conversion mirroring
and FPI refusal tasks were removed.
