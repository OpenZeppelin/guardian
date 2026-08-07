# Propagation and Validation Matrix: Guardian Prove and Commit

Per-layer propagation obligations (AGENTS.md §4, §8; Constitution I, II, V) and the
validation required at each. Every row must be satisfied or explicitly justified as
unaffected in PR notes.

## Gate 0 — architecture spike (**ratified, narrowed**)

**Status: the gate is closed for lifecycle implementation.** The spike was built and run: the
`DataStore` seam works over Guardian's own state, `PartialBlockchain` assembly from node RPC
was validated against public testnet, and the full authorized path executed and proved through
a remote prover — with no new dependencies.

The gate was **narrowed rather than fully passed**, and the residue below is deferred coverage,
not an open architectural question. Nothing outstanding can falsify the architecture: every
remaining case runs through the same `DataStore` and the same witness assembly. Snapshot-pinned
live-RPC note-block paths are now validated independently through `SyncNotes`; joining that
assembly to a live note-consuming execution remains deferred, as does submission.

Original gate wording, retained for the record: the architecture was to be considered not
final until a compile-tested spike executed, proved, and submitted all four proposal families
against a local node with a locally-run `miden-remote-prover`:

| Case | Why it must be in the spike | State |
|---|---|---|
| P2ID payment | Real send script from `AccountInterface::build_send_notes_script`; exercises vault witnesses and output-note recipients | **done** — `guardian_executes_the_p2id_send_family` |
| Configuration | Real `update_signers_and_threshold` from the multisig MASM library; script-arg + config-hash advice | **done** — `guardian_executes_the_configuration_family` |
| `consume_notes` | Input notes — the only family needing notes in the store | **prepared execution done**; snapshot-pinned live-RPC note-block assembly done independently; joined live flow pending |
| Custom (#266) | Opaque request, no recipe, account-default threshold | **deferred** — unrun; structurally identical to the above (an opaque script through the same seam) |
| Live submission | The only step that mutates chain state | **deferred** — needs a funded, Guardian-registered testnet account; not blocked by infrastructure |

Also covered: the full authorized path (`guardian_executes_signs_and_proves_end_to_end`) —
execute unsigned, sign as cosigner and as GUARDIAN, re-execute with advice, prove locally — and
the chain-MMR correctness gate at three successive heights.

**Round 1 (done): the original architecture was falsified.** `ClientDataStore` is
`pub(crate)`, so implementing miden-client's `Store` would have been pointless. Guardian must
implement `miden_tx::DataStore` directly — five methods plus `MastForestStore::get`. The
sqlite-versus-in-memory question is **moot**: there is no `Store`, so no embedded database and
no seeding I/O. See `research.md`.

**Round 2 (partly done): the `DataStore` seam works.** Implemented at
`crates/server/src/network/miden/execution/` behind a new `proving` feature and driven under
`MockChain`; two tests pass, including `consume_notes` with a real chain-committed note. Two
round-1 conclusions were corrected: `AccountSmtForest` cannot produce non-inclusion witnesses,
so the account's own `AssetVault::open` / `StorageMap::open` are used instead — which removes
the `miden-client` dependency entirely (`proving = ["miden-tx"]`, no `miden-processor` either) —
and `TransactionMastStore::load_account_code` is required, because the multisig and guardian
libraries are dynamically linked and are not in `account.code().mast()`.

**The MMR blocker recorded in round 2 was refuted on review and is withdrawn.**
`Mmr::get_delta` returns merge nodes plus peaks, not intervening blocks
(`miden-crypto-0.25.1/src/merkle/mmr/tests.rs:1241-1245`), so a cold-start `SyncChainMmr` is
logarithmic in chain length. Architecture A proceeds with **no persistent MMR cache** and **no
wait on Miden 0.16**, which adds no blocker-removing capability.

**`PartialBlockchain` assembly is written** —
`crates/server/src/network/miden/execution/blockchain.rs`, plus `sync_chain_mmr` on the thin
RPC client. Genesis-seeded `PartialMmr` → `SyncChainMmr(0)` → apply delta → assert
`hash_peaks()` equals the reference header's `chain_commitment`; note blocks tracked against
that single forest, deduplicated. The invariant that gate relies on is tested against a real
chain at three successive heights (`chain_mmr_peaks_hash_to_the_reference_block_commitment`).
The RPC path is exercised by that ignored live test against public testnet; ordinary CI keeps
the deterministic mock-chain coverage and does not require external connectivity.

**SUPERSEDED — round 2 has since closed.** The list below was written before the round-2 work
ran. Retained for the record only; do **not** read it as open scope. What it demanded has been
done: proving is wired and validated through a remote prover against public testnet;
`PartialBlockchain` assembly from RPC is implemented and gated on
`hash_peaks() == chain_commitment`, verified live at three successive heights; P2ID-send and
configuration are driven with their real tx scripts, not `TransactionArgs::default()`; the
SC-011 figures are measured and recorded in `spec.md`. Miden 0.16 was checked and changes none
of it.

> *Original text:* Round 2 (remaining) MUST still settle: proving and submission, which the
> spike does not touch; `PartialBlockchain` assembly from RPC — genesis-seeded `PartialMmr`, one
> `SyncChainMmr`, then assert `hash_peaks()` equals the reference header's `chain_commitment` —
> tested first for a **no-input-note** transaction against a real node, then for note
> consumption with every note-block proof anchored to the same forest and reference tip; P2ID-send
> and configuration transactions driven with their real tx scripts; the SC-011 figure; and whether
> the route holds at the Miden 0.16 line if #329 lands first. Until round 2 passes, treat the
> Execution Architecture section as provisional.

**Genuinely still deferred**: live **submission**; the live-RPC **note-block** path joined to
note-consuming execution in one flow; and the **custom family** (#266).
Foreign-account (FPI) inputs are a **normative exclusion** in v1 (FR-050), not deferred
coverage — the refusal is required behavior.

## Propagation

| Layer | Change | Required in same PR |
|---|---|---|
| `crates/server/proto/guardian.proto` | `ExecuteDeltaProposal`, `GetDeltaProposalExecution`, `GetCurrentExecution` + messages | yes |
| `crates/server/src/api/http.rs` | Three handlers with `#[utoipa::path]`, `ToSchema`/`IntoParams` derives | yes |
| `crates/server/src/api/grpc.rs` | Matching gRPC handlers, semantics equal to HTTP | yes |
| `crates/server/src/builder/handle.rs` | `POST /delta/proposal/execution`, `GET /delta/proposal/execution`, `GET /delta/execution/current` | yes |
| `docs/openapi*.json` | Regenerated via `cargo run --features evm --bin gen-openapi -- docs` | yes (CI fails on drift) |
| `crates/server/src/services/` | New execute-request + execution-status services | yes |
| `crates/server/src/services/push_delta.rs` | New reservation-conflict refusal (FR-027) | yes |
| `crates/server/src/services/push_delta_proposal.rs` | Envelope validation + FR-016 size limits | yes |
| `crates/server/src/jobs/` | Execution worker, modeled on `jobs/canonicalization/` | yes |
| `crates/server/src/coordination/`, `storage/` | Reservation reusing `LeaseFence`; single atomic admission primitive extending the `discard_candidate` pattern (FR-023, FR-037, FR-038) | yes |
| `crates/server/src/network/miden/` | Ephemeral store seeding + executor + prover delegation | yes |
| `crates/server/src/metadata/`, `storage/` | Reservation persistence, **filesystem and Postgres parity** | yes |
| `crates/server/src/error.rs` | New stable error codes | yes |
| `crates/client` | `execute_delta_proposal`, `get_delta_proposal_execution`, `get_current_execution` | yes |
| `packages/guardian-client` | Same three methods, `server-types.ts`, **error-code vocabulary** | yes |
| `crates/miden-multisig-client` | `ProposalExecutionMode`, request/status methods, envelope build | yes |
| `packages/miden-multisig-client` | Symmetric TS equivalents | yes |
| `fixtures/miden-multisig-client/` | Envelope cross-language + protocol-mismatch fixtures | yes |
| `packages/guardian-operator-client` | — | **no** (no `/dashboard/*` change; out of scope) |
| `packages/guardian-evm-client` | — | **no** (no `/evm/*` change; out of scope) |
| `examples/demo` | Rust end-to-end Guardian execution | yes |
| `examples/smoke-web` | TS end-to-end Guardian execution | yes |
| `examples/execution-smoke` (**new artifact**) | Base-client-only harness: request + poll with no Miden dependency, evidencing SC-001/FR-034 | yes |
| `docs/CONFIGURATION.md` | All eight new env vars | yes |
| `docs/MULTISIG_SDK.md` | New SDK surface + mode semantics | yes |
| `spec/api.md`, `spec/processes.md` | New endpoints + service description and diagram | yes |
| `docs/TROUBLESHOOTING.md` | New error codes and their symptoms | yes |
| `speckit/features/008-custom-proposal-producer/spec.md` | Annotate FR-015 with the narrowing | yes |

The error-code vocabulary row is called out because omitting it is exactly the defect
that produced #353 (`candidate_landed` present server-side, missing from the TS client).

## Validation

### Rust unit / service

| Target | Covers |
|---|---|
| `cargo test -p guardian-server` | Synchronous refusals (FR-022, SC-003); effective per-procedure threshold incl. override ≠ default (FR-005, SC-004); valid-subset selection incl. duplicate, invalid, and revoked-cosigner entries ignored (FR-006, SC-020) with insufficient valid sets refused as not-ready (SC-003); envelope checksum, format-version, protocol-line, and same-line serializer refusal before deserialization (FR-014/015, SC-010, SC-029); size limits (FR-016); state-transition legality and the five-value wire vocabulary with no internal state leaking (FR-024/025/026, SC-016); conflict error names the blocker (FR-036, SC-017) |
| `cargo test -p guardian-client` | Request/response mapping, error-code mapping, blocking-proposal id preserved on conflict (SC-017) |
| `cargo test -p miden-multisig-client` | Envelope construction; default (unconfigured) client produces a payload byte-shape unchanged from pre-feature (SC-009, FR-009); exhaustive state handling; no server-capability query and **no client-side size check** on the propose path (FR-009, H3) |
| `cargo test --workspace` | Regression sweep |

### Rust integration / e2e

| Target | Covers |
|---|---|
| `cargo test -p guardian-server --features integration` | Owner-authorized self-admission plus rejection of unrelated callers (FR-037); internal ack path while reserved (FR-044); atomic admission under concurrency (FR-037, SC-018); fence rejection of a stale worker (FR-038, SC-019); lease expiry → failed + released **only pre-submission**, ownership transfer post-submission (FR-028); `push_delta` refusal while reserved (FR-027); **filesystem and Postgres parity** for reservation state |
| `cargo test -p guardian-server --features e2e` | Full pipeline against a local node with a locally-run `miden-remote-prover`: seed → execute → prove → **admit candidate → submit** → canonical, matching the FR-045 order (SC-015); binding mismatch and state mismatch refusals (SC-002). **Not** SC-001 — that is base-client-only and belongs to the harness below |

### Spike coverage as it stands (offline, `--features e2e`)

Seven tests in `crates/server/src/network/miden/execution/tests.rs`:

| Test | Covers |
|---|---|
| `guardian_data_store_serves_a_full_transaction_execution` | Kernel reaches the auth boundary through Guardian's `DataStore` |
| `guardian_data_store_serves_note_consumption` | Prepared authenticated input-note execution |
| `guardian_executes_the_p2id_send_family` | Real send script; vault witnesses genuinely read |
| `guardian_executes_the_configuration_family` | Real `update_signers_and_threshold`; self-validating config-hash advice |
| `guardian_executes_signs_and_proves_end_to_end` | Full authorized path + proving; pins the `u32::MAX` expiration default |
| `guardian_proves_a_transaction_with_a_finite_expiration` | A send script that sets a finite expiration proves to `reference_block + delta`; validates the protocol mechanism behind FR-046/FR-051, not SDK-family coverage |
| `chain_mmr_peaks_hash_to_the_reference_block_commitment` | The `hash_peaks()` gate, at three successive heights |

### Live-network coverage (`#[ignore]`d, read-only unless noted)

```bash
cargo test -p guardian-server --features e2e --lib live_ -- --ignored --nocapture
```

| Test | Covers | Needs |
|---|---|---|
| `live_cold_start_chain_mmr_matches_the_reference_block` | Genesis-seeded cold start; peaks match the reference header at block 1,002,185 | RPC reads only |
| `live_sync_notes_paths_track_against_the_execution_reference_forest` | Explicit `block_to = reference - 1`, pagination, and 753 recent note-block paths tracked against the execution forest; reference 1,174,436 | RPC reads only; 1.63 s bounded run; separate full-range diagnostic took 59.9 s for broad tag `0` |
| `live_prove_a_guardian_assembled_witness` | Live chain data → execute → prove via the public testnet prover | RPC reads + `GUARDIAN_TX_PROVER_URL` |

Endpoint override: `GUARDIAN_TEST_RPC_ENDPOINT` (defaults to public testnet).

**Still uncovered:** joining RPC-assembled note data to a live note-consuming execution, and
submission. Submission needs a funded, Guardian-registered account rather than new
infrastructure.

**Operational note for the prover:** the client library's default timeout is 10 s and observed
proving times are 6–20 s, so leaving it unset fails *intermittently* with a message that names no
timeout. Tests and production must set it explicitly (FR-020).

### Fault injection (required, not optional)

These cover the findings that motivated FR-030/FR-031 and cannot be validated by
happy-path tests:

| Scenario | Asserts |
|---|---|
| Kill the reservation holder mid-proof | Lease expires; execution failed; reservation released; account usable (SC-007) |
| Submission times out / connection dropped | Reports `submitted`; reservation retained; retry refused; resolves only via chain observation; **never a second submission** (FR-030, SC-008) |
| Restart before the no-retry boundary | Execution reported `failed` and released (FR-031) |
| Restart after the no-retry boundary, including before network send | Resolved by reconciliation, not by store replay or re-send (FR-031, FR-047) |
| Candidate discarded after the no-retry boundary | Reports `failed` with `GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` (FR-040) |
| Prover unreachable | Proving failure; reservation released; account unlocked (US5 scenario 3) |
| Two replicas racing one queued execution | Exactly one proves and submits (FR-029, SC-005) |
| Guardian switched, observed by the final admissibility read | Execution failed, nothing submitted (FR-048, SC-026) |
| Guardian switched *after* the final read | Not preventable; the stale proof is rejected by the node and settles as a definite submission rejection (FR-048) |
| Concurrent reservation-create vs candidate-admit on one account | One atomic primitive; no **unauthorized** coexistence in either order. A candidate under its own authorizing reservation is the expected steady state while `submitted` (FR-037, SC-018) |
| Worker paused past lease expiry, ownership transferred, then resumed **before** the boundary commit | Fenced commit fails `StaleLease`; nothing written, nothing sent (FR-038, SC-019) |
| Worker goes stale **after** the boundary commit, then wakes | Pre-send fence re-check aborts it; writes nothing, sends nothing; reconciliation resolves the durable candidate within the horizon (FR-049, SC-031) |
| Worker dies after prover returns, before recording | Retry proves again (accepted); still exactly one submission (FR-029, SC-005) |
| Unknown submission, account never leaves base | Terminates only when chain passes the recorded expiration block (FR-040, SC-008) |
| Unknown submission, account observed superseded | Terminates `failed` on superseded evidence (FR-040) |
| Candidate + proposal deleted by canonicalization | Terminal outcome persisted before deletion, still readable, `proposal_exists: false` (FR-041, FR-042, SC-021) |
| One garbage signature among enough valid ones | Ignored and recorded; execution proceeds (FR-006, SC-020) |
| Server in optimistic delta-commit mode | Execution refused; misconfiguration reported at startup (FR-043, SC-022) |
| Proposal requiring foreign-account inputs (FPI) | Refused before any proving or submission, with a distinct reason (FR-050, SC-032) |
| Crash between FR-039 evidence write and the send | Evidence is durable; recovery reconciles only, never re-submits or re-proves (FR-047, SC-024) |
| Crash immediately *before* the evidence write | No submission occurred; recovery fails-and-releases (FR-047, SC-024) |
| Candidate promotion | Atomically persists `landed` and releases the reservation (FR-041, SC-025) |
| Candidate deletion | Atomically persists the terminal failure before the row disappears (FR-041, SC-025) |
| `SwitchGuardian` canonicalizes during proving | Pre-submission re-check fails the execution; nothing submitted (FR-048, SC-026) |
| Proven transaction with no finite expiration | Refused before the no-retry boundary with `GUARDIAN_EXECUTION_NO_FINITE_EXPIRATION` (FR-046, SC-027) |
| Built-in proposal family in Guardian mode, Rust and TS | Produces a finite expiration using the shared 256-block default; same effects and salt preserve the self-executed summary and proposal ID (FR-012, FR-051, SC-033) |
| Opaque custom request whose script sets finite expiration | Request bytes are attached unchanged and execution passes the finite-expiration gate (FR-051, SC-033) |
| Opaque custom request with no finite expiration | Request bytes are attached unchanged; execution is refused before the boundary with `GUARDIAN_EXECUTION_NO_FINITE_EXPIRATION` (FR-046, FR-051, SC-033) |
| Guardian's own candidate admission | Succeeds under the matching reservation owner + fence; an unrelated caller's candidate is still rejected (FR-037, SC-028) |
| Crash between the atomic commit and the network send | Candidate and evidence both durable; observer sees `submitted`; reconcile-only, never re-sent (FR-045 step 9, FR-047, SC-030) |
| Admissibility fails at FR-045 step 8 | Ordinary fail-and-release: no candidate, no evidence, reservation released — the pre-boundary path (FR-048, SC-026) |
| Definite submission rejection after the boundary | Candidate exists by construction and is discarded; reservation released (FR-032) |
| Guardian's own acknowledgment while reserved | Obtained via the internal path; public `push_delta` remains refused (FR-044, SC-028) |
| Guardian-executable client vs proving-disabled server | Creation succeeds and stores the attachment; refusal happens only at execute, as `GUARDIAN_PROVING_UNAVAILABLE`; no rejection and no silent discard at creation (FR-009) |
| Default client vs proving-enabled server | Creation stores no attachment; execute refused with `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` (FR-010) |

### TypeScript

| Target | Covers |
|---|---|
| `cd packages/guardian-client && npm test` | Envelope types, error-code vocabulary completeness, state exhaustiveness |
| `cd packages/miden-multisig-client && npm test` | Mode semantics, envelope build, parity with Rust fixtures |

### Examples (manual smoke, per AGENTS.md manual policy)

| Harness | Covers |
|---|---|
| `examples/demo` | Rust: propose as Guardian-executable → sign to threshold → request execution → observe landing, for a built-in **and** a custom proposal type (SC-015) |
| `examples/execution-smoke` (new) | SC-001 / FR-034: request execution and poll to `landed` using **only** `packages/guardian-client` (plus a Rust counterpart over `crates/client`) — no Miden client constructed, no node connectivity, no proving. The only artifact that can evidence the no-Miden guarantee |
| `examples/smoke-web` | TS end-to-end via the multisig SDK (SC-015). MUST NOT be claimed for SC-001/FR-034 — it constructs a Miden client, so it cannot demonstrate the no-Miden guarantee |

### Parity and performance

| Check | Covers |
|---|---|
| Cross-SDK envelope fixtures | Identical `format_version`, `protocol_line`, full `serializer_id` (including prerelease), and checksum from both SDKs; same-line/unallowlisted-serializer and unsupported-format fixtures are refused before deserialization, with identical behavior (SC-013, SC-029) |
| HTTP ↔ gRPC transport parity | Every refusal code, state value, and conflict/envelope field observably equivalent on both transports, per case (SC-023). Includes the 202-vs-200 distinction, which gRPC must carry as a response field |
| Execution witness setup measurement | Per-execution direct `miden_tx::DataStore`, `TransactionMastStore`, and in-memory SMT witness setup cost measured and recorded as a committed baseline; **telemetry, not a pass/fail gate** (SC-011). Execution remains ephemeral and memory-only; there is no SQLite-versus-`Store` decision |

## Skills

- `guardian-contract-change` — mandatory; this is a wire-contract change across every
  per-account client surface.
- `guardian-validation-matrix` — to select the minimal meaningful subset while iterating.
- `guardian-multisig-proposal-lifecycle` — proposal create/sign/execute changes.
- `smoke-test-rust-multisig-sdk`, `smoke-test-ts-multisig-sdk` — the two example smokes.
