# Propagation and Validation Matrix: Guardian Prove and Commit

Per-layer propagation obligations (AGENTS.md §4, §8; Constitution I, II, V) and the
validation required at each. Every row must be satisfied or explicitly justified as
unaffected in PR notes.

Revised 2026-09-30 for Miden 0.17 (spec revision 11, RFC revision 17): protocol / standards / tx
`0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`, `miden-node-proto-build` `0.17.0-rc.3`, web SDK
`0.17.0-rc.4`. Evidence is in `research.md`, "Miden 0.17 re-verification". Tags: **[READ]**
checked against source, **[RAN]** executed, **[INFERRED]** derived. No 0.17 row below is [RAN] by
this feature's work yet.

**Environments on 0.17.** Live rows run against **devnet** (node 0.17.0-rc.2). Testnet runs
Miden 0.16, so testnet-equivalent rows run against a **local `miden-node`** from the pinned line
with a locally run `miden-remote-prover`. Production qualification is gated on stable 0.17 plus
the re-pin (roots, fixtures, vectors).

## Gate 0 — architecture spike (**ratified, narrowed**)

**2026-09-30:** the gate's findings were made on the 0.15 line and the spike's code runs on the
0.16 release candidates (protocol rc.9, client rc.4). On 0.17 it must be ported (to protocol
rc.7, client rc.4) before any of its tests count; its tip-reference `ChainView` and
`SyncChainMmr` construction is normative again as FR-061.

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
`crates/server/src/network/miden/execution/` (on the [`254-execution-spike`](https://github.com/OpenZeppelin/guardian/tree/254-execution-spike) branch, commit `769e2a90`; not on `main`) behind a new `proving` feature and driven under
`MockChain`; two tests pass, including `consume_notes` with a real chain-committed note. Two
round-1 conclusions were corrected: `AccountSmtForest` cannot produce non-inclusion witnesses,
so the account's own `AssetVault::open` / `StorageMap::open` are used instead — which removes
the `miden-client` dependency entirely (`proving = ["miden-tx"]`, no `miden-processor` either) —
and `TransactionMastStore::load_account_code` is required, because the multisig and guardian
libraries are dynamically linked and are not in `account.code().mast()`.

**The MMR blocker recorded in round 2 was refuted on review and is withdrawn.**
`Mmr::get_delta` returns merge nodes plus peaks, not intervening blocks
(`miden-crypto-0.29.4/src/merkle/mmr/tests.rs:1269-1273`; `0.25.1:1241-1245` when first cited), so a cold-start `SyncChainMmr` is
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
>
> **2026-09-15 addendum**: #329 landed and `main` pins stable 0.16. The spike ran on the 0.16 rc
> pins and predates two inputs the execution path now needs: the proposal's `ChainAnchor` as the
> source of the reference header and partial blockchain (FR-056), and the fee conversion advice
> (FR-057), plus the stale-anchor pre-proving check (FR-058). Its `SyncChainMmr`/`SyncNotes`
> live checks are historical evidence and cover nothing in v1; the anchored path, pinned-note
> reproduction, the fee decision, and the stale-anchor check need their own offline tests before
> the residue below counts as covered.
>
> **2026-09-30 addendum, superseded**: the 2026-09-15 addendum above is withdrawn for Miden 0.17.
> The summary binds a chosen bound block, so reproduction runs at the tip `R` (FR-056, FR-061),
> not at the anchor; the anchor fails on devnet after about 50 blocks (#462). Guardian never
> derives fee conversion advice (FR-057), and the stale-anchor check becomes the
> expiration-reached stop (FR-058). The spike's `SyncChainMmr` live checks are evidence for the
> normative FR-061 construction again, but only after the port; the new inputs
> (`ProtocolConfig`, bound-block tracking, foreign public accounts, sealing) need their own tests.

**Genuinely still deferred**: live **submission** (now sealed, FR-059); the live-RPC
**note-block** path joined to note-consuming execution in one flow; and the **custom family**
(#266). Foreign-account inputs are **no longer an exclusion** on 0.17: every devnet fee payment
loads the fee faucet as a foreign account, so foreign public accounts loaded at `R` are required
behavior (FR-050) and private or unservable foreign state is a distinct refusal.

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
| `crates/server/src/network/miden/` | Direct `miden_tx::DataStore` (returns the checked `ProtocolConfig`, tracks the bound block and note blocks at forest `R`, loads foreign public accounts at `R`), executor, prover delegation, input sealing (FR-050, FR-059, FR-060, FR-061) | yes |
| `crates/miden-rpc-client` | `sync_chain_mmr` (ported from the spike), `include_protocol_config` on `GetBlockHeaderByNumber` (hardcoded `None` on `main`), `GetAccount` at `block_num = R` with details, `GetTransactionEncryptionKey`, sealed `SubmitProvenTx` | yes |
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
| `cargo test -p guardian-server` (0.17 structural checks) | `GUARDIAN_EXECUTION_REQUEST_INVALID` raised asynchronously after envelope and protocol checks and before any chain read, one case per `meta.reason`: `bound_block_not_declared` (`block_numbers()` lacks the summary's `block_number`), `auth_args_missing` (empty `auth_arg`, or preimage absent from the advice map), `approval_expiration_missing` (user param 0 is zero), `input_notes_not_pinned` (consume-notes without `explicit_input_notes`); reservation released; bound block decoded from the signed summary only, never from the anchor (FR-051, FR-056, FR-057) |
| `cargo test -p guardian-server` | Synchronous refusals (FR-022, SC-003); effective per-procedure threshold incl. override ≠ default (FR-005, SC-004); valid-subset selection incl. duplicate, invalid, and revoked-cosigner entries ignored (FR-006, SC-020) with insufficient valid sets refused as not-ready (SC-003); envelope checksum, format-version, protocol-line, and same-line serializer refusal before deserialization (FR-014/015, SC-010, SC-029); size limits (FR-016); state-transition legality and the five-value wire vocabulary with no internal state leaking (FR-024/025/026, SC-016); conflict error names the blocker (FR-036, SC-017) |
| `cargo test -p guardian-client` | Request/response mapping, error-code mapping, blocking-proposal id preserved on conflict (SC-017) |
| `cargo test -p miden-multisig-client` | Envelope construction; default (unconfigured) client produces a payload byte-shape unchanged from pre-feature (SC-009, FR-009); exhaustive state handling; no server-capability query and **no client-side size check** on the propose path (FR-009, H3) |
| `cargo test --workspace` | Regression sweep |

### Rust integration / e2e

| Target | Covers |
|---|---|
| `cargo test -p guardian-server --features integration` | Owner-authorized self-admission plus rejection of unrelated callers (FR-037); internal ack path while reserved (FR-044); atomic admission under concurrency (FR-037, SC-018); fence rejection of a stale worker (FR-038, SC-019); lease expiry → failed + released **only pre-submission**, ownership transfer post-submission (FR-028); `push_delta` refusal while reserved (FR-027); **filesystem and Postgres parity** for reservation state |
| `cargo test -p guardian-server --features e2e` | Full pipeline against a local 0.17 node with a locally run `miden-remote-prover`: pick `R` → assemble chain view and `ProtocolConfig` → reproduce → execute → prove → **seal → admit candidate → submit** → canonical, matching the D4 FR-045 order (steps 3 to 14) (SC-015); binding mismatch and state mismatch refusals (SC-002). **Not** SC-001, which is base-client-only and belongs to the harness below |

### Spike coverage as it stands (offline, `--features e2e`)

Seven tests in `crates/server/src/network/miden/execution/tests.rs`, on the 0.16 rc.9 spike
branch. None counts for 0.17 until ported; the expiration tests below measured the 0.15/0.16
behavior (default `u32::MAX`, script delta exact) and must be re-run on 0.17, where a guarded
multisig transaction with an approval expiration always proves finite.

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

Endpoint override: `GUARDIAN_TEST_RPC_ENDPOINT` (defaults to public testnet). These results are
0.15-line evidence. On 0.17 the endpoint must point at devnet or a local 0.17 node, because
testnet runs 0.16.

Required 0.17 live coverage (none [RAN] yet by this feature; the SDK work on `main` observed
tip execution on devnet more than 70 blocks past the bound block, per
`docs/MIDEN_COMPATIBILITY.md` open upstream items):

| Scenario | Environment | Covers |
|---|---|---|
| Guardian execution requested more than 50 blocks after proposal creation | devnet | Tip reproduction at `R` under the signed bound block after devnet has pruned the bound block's account state (FR-056, FR-061) |
| Any fee-paying execution | devnet | Fee-faucet foreign load at `R` through `GetAccount(block_num = R)` (FR-050); `ProtocolConfig` fetched with `include_protocol_config` and checked (FR-060) |
| Sealed submission | devnet, then local node | Key fetch, attestation check and sealing before the boundary, then `SubmitProvenTx` accepted (FR-059) |
| Testnet-equivalent full pipeline | local 0.17 `miden-node` + local prover | Everything in the e2e row above, without depending on devnet's pruning window or version |

**Still uncovered:** joining RPC-assembled note data to a live note-consuming execution, and
submission. Submission needs a funded, Guardian-registered account rather than new
infrastructure.

**Operational note for the prover:** the client library's default timeout is still 10 s on 0.17
(`miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`, [READ]); the 0.16 proving times
are historical and must be re-measured on proof format 2. Observed
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
| Foreign public account loaded at `R`: fee-faucet asset callback on devnet (every fee payment), a callback-enabled asset P2ID, and FEE_SPONSORSHIP pricing of a network output note by FPI | Executes; `get_foreign_account_inputs` served from `GetAccount(account_id, block_num = R, details)` with the witnesses the executor asks for (FR-050, SC-032). Live on devnet |
| Private foreign account, or foreign state the node cannot serve (node `AccountNotPublic` / `BlockPruned`) | Refused pre-boundary with `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE`, `meta.reason` = `private` or `unavailable`; nothing proved or submitted; reservation released (FR-050, SC-032) |
| Crash between FR-039 evidence write and the send | Evidence is durable; recovery reconciles only, never re-submits or re-proves (FR-047, SC-024) |
| Crash immediately *before* the evidence write | No submission occurred; recovery fails-and-releases (FR-047, SC-024) |
| Candidate promotion | Atomically persists `committed` and releases the reservation (FR-041, SC-025) |
| Candidate deletion | Atomically persists the terminal failure before the row disappears (FR-041, SC-025) |
| `SwitchGuardian` canonicalizes during proving | Pre-submission re-check fails the execution; nothing submitted (FR-048, SC-026) |
| Proven expiration beyond the FR-046 horizon, measured as `proven expiration_block_num - R` for the attempt's own `R` (including the `u32::MAX` sentinel of a non-expiring transaction) | Refused before the no-retry boundary with `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON`; no waiting; the default horizon is at least 256 (FR-046, SC-027) |
| Approval expired, sync-tip check: observed tip at or past the approval expiration (user param 0) at D4 step 2 | Refused asynchronously pre-boundary with `GUARDIAN_EXECUTION_EXPIRATION_REACHED`, `meta.bound` = `approval`; no chain view assembled, nothing proved (FR-058) |
| Approval expired, VM abort: the tip passes the approval expiration between step 2 and reproduction, so `assert_approval_not_expired` aborts with `ERR_MULTISIG_APPROVAL_EXPIRED` | Mapped to `GUARDIAN_EXECUTION_EXPIRATION_REACHED`, `meta.bound` = `approval`, not to a generic reproduction failure; reservation released (FR-058) |
| Transient prover failures while the tip advances past the approval expiration or the executed transaction's expiration block | Retrying stops on the chain-height check before each proving retry, not only on the horizon; settles `failed` / `GUARDIAN_EXECUTION_EXPIRATION_REACHED` pre-boundary, `meta.bound` = `approval` or `transaction` (FR-055, FR-058) |
| Two-bound expiration, built-in family: a 256-block transaction delta reproduces at a different `R` than the proposer's | Summary matches (the delta is signed relative to the reference block); proven expiration is `R + min(256, approval_expiration - R)` (FR-051) |
| Two-bound expiration, approval clamp: approval expiration closer than 256 blocks to `R` | Proven expiration equals the approval expiration (`update_expiration_block_delta` keeps the minimum), not `R + 256` (FR-051) |
| Two-bound expiration, custom producer with no script delta | Proven expiration falls back to the approval bound clamped to 65,535 from `R`; refused with `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON` if beyond the horizon, executed otherwise (FR-046, FR-051) |
| `GUARDIAN_EXECUTION_CHAIN_BEHIND`: Guardian's node tip is below the summary's bound block | Refused pre-boundary as retryable; reservation released; a later request after the node catches up executes (FR-061) |
| `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`: account's native fee-asset balance cannot pay the fee (including an unfunded first transaction) | Reproduction abort mapped to this code, not a generic reproduction failure; pre-boundary; reservation released |
| Fee drift: `R`'s verification base fee differs from the bound block's, or the cycle count at `R` crosses a power of two, changing the TX_FEE note | Settles `GUARDIAN_EXECUTION_BINDING_MISMATCH` pre-boundary; the logged diagnostic compares the TX_FEE output notes so drift is distinguishable from tampering; wire code unchanged (FR-007, N1, open upstream question) |
| Consume-notes request (built-in or custom producer) without `explicit_input_notes` for a consumed note | Refused asynchronously before any chain read with `GUARDIAN_EXECUTION_REQUEST_INVALID`, `meta.reason` = `input_notes_not_pinned`; reservation released within milliseconds (FR-056) |
| FR-061 chain view: bound block **and** every authenticated note's creation block tracked in the `PartialBlockchain` at forest `R`, where `R` is the node's committed tip at attempt start | Executes; missing bound-block tracking would raise `TransactionSummaryUnknownBlockNumber`, which the test asserts cannot occur; `hash_peaks()` equals `R`'s `chain_commitment` (FR-061) |
| FR-061 tip moves between reads: the node tip advances between the `SyncChainMmr` peaks read and a `GetBlockHeaderByNumber(include_mmr_proof)` read, so returned paths are at a `chain_length` past `R` | Paths are adjusted to forest `R` and still verify; execution proceeds at the original `R`; no path at the wrong forest reaches the executor (FR-061) |
| FR-060: `ProtocolConfig` whose commitment does not match `R`'s `protocol_config_commitment()` | Refused before execution; nothing executed or proved (FR-060) |
| Request with an empty `auth_arg`, or whose advice map lacks the auth-arg preimage | Refused with `GUARDIAN_EXECUTION_REQUEST_INVALID`, `meta.reason` = `auth_args_missing`; Guardian never derives, attaches or repairs fee conversion info (FR-057) |
| Request that already carries the three-word multisig auth arg | Passed through unchanged at execution; Guardian does not overwrite the producer's fee commitment (FR-057) |
| FR-059 sealing pre-boundary: key fetch fails, attestations fail validation, or sealing fails (fault-inject each on `GetTransactionEncryptionKey`) | Settles `failed` / `GUARDIAN_EXECUTION_SEALING_FAILED` before the boundary commit; no candidate, no evidence, reservation released; never stranded until expiration (FR-059) |
| FR-059 sealing success | `TransactionInputs` kept from execution through sealing; the sealed blob is not persisted (nothing is re-sent); the submitted `ProvenTransactionSubmission` carries it (FR-059, FR-039) |
| Built-in proposal family in Guardian mode, Rust and TS | Carries the shared 256-block transaction delta and a non-zero approval expiration (default `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA = 28_800`, caller override 1..65,535); both SDKs derive the same summary and proposal ID for the same effects, salt and bound block, and that ID differs from self-executed mode because both the 256 delta and the approval expiration are signed (FR-012, FR-051, SC-033) |
| Opaque custom request built through `multisig_auth_args(salt, bound_block, delta)` with a non-zero approval expiration, with or without a script delta | Request bytes are attached unchanged; execution passes the expiration gate within the horizon (FR-051, SC-033) |
| Opaque custom request with approval expiration zero | Request bytes are attached unchanged; execution refused with `GUARDIAN_EXECUTION_REQUEST_INVALID`, `meta.reason` = `approval_expiration_missing` (FR-051, SC-033) |
| Guardian's own candidate admission | Succeeds under the matching reservation owner + fence; an unrelated caller's candidate is still rejected (FR-037, SC-028) |
| Crash between the atomic commit and the network send | Candidate and evidence both durable; observer sees `submitted`; reconcile-only, never re-sent (FR-045 step 12, FR-047, SC-030) |
| Admissibility fails at FR-045 step 11 | Ordinary fail-and-release: no candidate, no evidence, reservation released, the pre-boundary path (FR-048, SC-026) |
| Definite submission rejection after the boundary | Candidate exists by construction and is discarded; reservation released (FR-032) |
| Guardian's own acknowledgment while reserved | Obtained via the internal path; public `push_delta` remains refused (FR-044, SC-028) |
| Guardian-executable client vs proving-disabled server | Creation succeeds and stores the attachment; refusal happens only at execute, as `GUARDIAN_PROVING_UNAVAILABLE`; no rejection and no silent discard at creation (FR-009) |
| Default client vs proving-enabled server | Creation stores no attachment; execute refused with `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` (FR-010) |

### TypeScript

| Target | Covers |
|---|---|
| `cd packages/guardian-client && npm test` | Envelope types, error-code vocabulary completeness, state exhaustiveness |
| `cd packages/miden-multisig-client && npm test` | Mode semantics, envelope build, parity with Rust fixtures; shared 256-block delta and `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA` constants equal to Rust's |
| TS bound-block parity | The TypeScript SDK takes the bound block from the signed summary's `block_number`, as Rust does, not from `anchor.blockNum()` (`multisig.ts:250` on `main`); a TS proposal whose anchor names a different block fails the same way on both SDKs and on Guardian (N3) |

### Examples (manual smoke, per AGENTS.md manual policy)

| Harness | Covers |
|---|---|
| `examples/demo` | Rust: propose as Guardian-executable → sign to threshold → request execution → observe on-chain commitment, for a built-in **and** a custom proposal type (SC-015) |
| `examples/execution-smoke` (new) | SC-001 / FR-034: request execution and poll to `committed` using **only** `packages/guardian-client` (plus a Rust counterpart over `crates/client`) — no Miden client constructed, no node connectivity, no proving. The only artifact that can evidence the no-Miden guarantee |
| `examples/smoke-web` | TS end-to-end via the multisig SDK (SC-015). MUST NOT be claimed for SC-001/FR-034 — it constructs a Miden client, so it cannot demonstrate the no-Miden guarantee |

### Parity and performance

| Check | Covers |
|---|---|
| Cross-SDK envelope fixtures | Identical `format_version`, `protocol_line` (`"0.17"`), full `serializer_id` (including prerelease), and checksum from both SDKs; `serializer_id` names the miden-client / web SDK version that serialized the request, not the Guardian SDK version; same-line/unallowlisted-serializer and unsupported-format fixtures are refused before deserialization, with identical behavior, including an rc.3-serialized request against an rc.4 allowlist (rc.4 prepends `block_numbers`, so the bytes do not decode across) (FR-014, SC-013, SC-029) |
| HTTP ↔ gRPC transport parity | Every refusal code, state value, and conflict/envelope field observably equivalent on both transports, per case (SC-023). Includes the 202-vs-200 distinction, which gRPC must carry as a response field |
| Execution witness setup measurement | Per-execution direct `miden_tx::DataStore`, `TransactionMastStore`, and in-memory SMT witness setup cost measured and recorded as a committed baseline; **telemetry, not a pass/fail gate** (SC-011). Execution remains ephemeral and memory-only; there is no SQLite-versus-`Store` decision |

## Skills

- `guardian-contract-change` — mandatory; this is a wire-contract change across every
  per-account client surface.
- `guardian-validation-matrix` — to select the minimal meaningful subset while iterating.
- `guardian-multisig-proposal-lifecycle` — proposal create/sign/execute changes.
- `smoke-test-rust-multisig-sdk`, `smoke-test-ts-multisig-sdk` — the two example smokes.

## Upstream review acceptance checks

- Proposal creation and signature collection perform no transaction execution and create
  no execution reservation. A ready-at-creation proposal executes only after the explicit
  trigger; cover Rust and TypeScript consumers and Falcon/ECDSA signatures.
- A Guardian acknowledgment cannot fill a missing cosigner signature. Keep the upstream
  wallet quorum mapping open until agreed; do not infer general 2-of-3 equivalence.
- FR-016 count quotas use authenticated proposer identity. Test atomic concurrent creates,
  stale proposals freeing viable count quota, and capacity for another signer on both
  storage backends. Finalize count allocation/configuration before implementing these tests.
- Execution success is `committed` in HTTP, gRPC, SDKs and storage outcomes. Delta success
  stays `canonical`; existing `candidate_landed` error codes are unchanged.
- No v1 preparation, automatic execution, chaining or batching API is introduced.
