# Research: Guardian Proves and Commits Transactions (#254)

Findings gathered while specifying issue #254. Every claim below was verified against
the tree at the time of writing; file references are `path:line`.

> **2026-09-15**: this log was written against Miden 0.15.x and the 0.16 alphas and is kept as
> the historical record. `main` now pins stable 0.16 (`miden-protocol`/`miden-tx` 0.16.1,
> `miden-client`/`miden-node-proto-build` 0.16.0, `miden-crypto` 0.29.4). The re-verified
> citations and the three design consequences that postdate this log, `ChainAnchor`-based
> reproduction, fee conversion advice, and the stale-anchor check, live in RFC 0001 revision 16
> and `spec.md` FR-056, FR-057 and FR-058. Where a claim below names a 0.15 file, the RFC
> carries the 0.16 equivalent, and where it describes assembling a chain view at the tip from
> `SyncChainMmr` and `SyncNotes`, that is the historical spike construction: FR-056 admits no
> anchorless proposal and forbids substituting the tip.
>
> **2026-09-30, superseded in part**: the 0.16 anchored-reproduction decisions named in the note
> above (execute at the proposal's `ChainAnchor`, never substitute the tip; derive fee conversion
> advice server-side; the `GUARDIAN_EXECUTION_ANCHOR_EXPIRED` stale-anchor stop) are withdrawn by
> the Miden 0.17 re-verification below and recorded as withdrawn in RFC 0001 A.3 (revision 17).
> The historical spike construction the note calls non-normative, a chain view at the tip from
> `SyncChainMmr`, is normative again as FR-061. The note is kept unedited as the 0.16 record.

## Miden 0.17 re-verification (2026-09-30)

Re-verified against the pins on `main` at `34920008`: `miden-protocol` / `miden-standards` /
`miden-tx` `0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`, `miden-node-proto-build` `0.17.0-rc.3`,
web SDK `0.17.0-rc.4`, including #485 (0.17 port) and #498 (tip execution). Crate paths are
relative to `~/.cargo/registry/src/index.crates.io-*/`. The full review, with the blocking (B1 to
B6) and non-blocking (N1 to N11) findings, is `rfc-0001-miden-017-review.md` at the repository
root; the decisions D1 to D5 there are normative for spec revision 11 and RFC revision 17.

Tags: **[READ]** was checked against the cited source on 2026-09-30. **[RAN]** was executed.
**[INFERRED]** follows from [READ] facts but was not executed. Nothing on the 0.17 line has been
[RAN] by this feature's work: the spike (`254-execution-spike`, `769e2a90`) is still on the 0.16
release candidates (protocol rc.9, client rc.4) and must be ported to 0.17 rc.7 before any of its
tests mean anything. The only 0.17 runs cited below are the SDK work recorded in
`docs/MIDEN_COMPATIBILITY.md` ("Open upstream items", last checked 2026-09-28), marked
*observed by the SDK work*.

Environments: live work targets **devnet** (node 0.17.0-rc.2). **Testnet runs Miden 0.16**, so
the testnet-equivalent environment on 0.17 is a local `miden-node` from the pinned line.
Production is gated on stable 0.17 plus the re-pin (procedure roots, fixtures, cross-SDK
determinism vectors).

### The summary binds a chosen bound block, not the reference block

- **[READ]** `TransactionSummary` carries `block_number` and `block_commitment`, "the block the
  summary binds", plus the transaction's `expiration_delta` and the user params
  (`miden-protocol-0.17.0-rc.7/src/transaction/tx_summary.rs:23-39`).
- **[READ]** The commitment preimage is 24 felts:
  `[version, metadata, user_param0, user_param1], [user_param2..5], ACCOUNT_DELTA_COMMITMENT,
  INPUT_NOTES_COMMITMENT, OUTPUT_NOTES_COMMITMENT, BLOCK_COMMITMENT` (layout
  `tx_summary.rs:137-143`, `to_elements` `:209-222`, `NUM_ELEMENTS` `:58-59`). There are six user
  params (`:365`). The metadata felt packs `expiration_delta` (16 bits) and `block_number`
  (32 bits) (`:276-282`).
- **[READ]** The auth procedure builds it with `create_tx_summary_with_block(bound_block, ...)`,
  whose bound block "must not exceed the transaction reference block number"
  (`miden-standards-0.17.0-rc.7/asm/standards/auth/mod.masm:48-49,67-99`). The kernel's
  `get_block_commitment` asserts `bound_block <= ref_block` and resolves an older block through
  the partial blockchain MMR (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:98-130`).
- **[READ]** The host rejects a summary whose bound block the transaction inputs do not
  authenticate: `TransactionSummaryUnknownBlockNumber` (`miden-tx-0.17.0-rc.7/src/host/mod.rs:504-516`).
- **[INFERRED]** Consequence: executing at the proposal's anchored block is no longer required
  for the binding. Any reference block `R >= bound_block` whose partial blockchain tracks the
  bound block reproduces the same `BLOCK_COMMITMENT`. The bound block's commitment is proven by
  an MMR path under `R`'s chain commitment, so the trust root reduces to "is `R`'s header
  canonical" (upstream question 2).

This also corrects "CORRECTED: expiration is not part of the signed transaction summary" below
for the 0.17 line: the transaction's expiration delta is now in the signed metadata (see the
guarded multisig section).

### Tip reproduction, and why the anchor fails on 0.17

- **[READ]** On `main` both SDKs verify, sign and execute at the tip (#498): Rust
  `execute_for_summary_at_tip` (`crates/miden-multisig-client/src/transaction/mod.rs:111-127`),
  TypeScript `executeForSummaryAtTip` (`packages/miden-multisig-client/src/transaction/summary.ts:148-169`).
  The Rust SDK refuses a request whose `block_numbers()` lacks the bound block
  (`BoundBlockNotDeclared`, `transaction/mod.rs:205-217`) and a local store behind the bound block
  (`ChainBehindBoundBlock`, `:152-173`).
- **[READ]** `chain_anchor` on `main` only names the bound block; nothing executes against it,
  and the server never reads it (`crates/miden-multisig-client/src/payload.rs:82-86`).
- *Observed by the SDK work* (`docs/MIDEN_COMPATIBILITY.md`, open upstream items): devnet keeps
  about 50 blocks of account history and answers `block N has been pruned` past that. Its fee
  faucet enables asset callbacks, so every fee payment loads the faucet as a foreign account at
  the reference block. Two guarded 2-of-2 proposals were verified, signed and executed at the
  tip more than 70 blocks after their bound block (#462).
- **[INFERRED]** An anchored execution (FR-056 revision 10) therefore fails at the foreign
  fee-faucet load for any execution requested more than about 50 blocks after proposal creation,
  which is the normal case for human-paced signature collection. The 0.16 decision "MUST NOT
  substitute the chain tip" is **superseded**: FR-056 becomes tip reproduction under the signed
  bound block, and FR-058 (`ANCHOR_EXPIRED`) loses its premise and becomes the
  expiration-reached stop (`GUARDIAN_EXECUTION_EXPIRATION_REACHED`, D3).

### `DataStore` on 0.17

- **[READ]** The seam survives: five methods plus `MastForestStore`. The only signature change is
  that `get_transaction_inputs` also returns a `ProtocolConfig`, which "must be the one the
  returned block header commits to" (`miden-tx-0.17.0-rc.7/src/executor/data_store.rs:19-96`,
  requirement at `:29-30`). FR-060.
- **[READ]** The reference block's header carries `PROTOCOL_CONFIG_COMMITMENT` and the
  `verification_base_fee` the kernel reads (prologue advice layout,
  `miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/prologue.masm:265-300`).
- **[READ]** `prepare_tx_inputs` builds `ref_blocks` from the input notes' blocks plus the
  reference block only (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:280-281`); the bound block is
  not among them. Lazy block-witness loading is a TODO: `TxBeforeBlockWitnessLoad` returns `None`
  (`host/tx_event.rs:491-494`). `ClientDataStore` closes the gap by adding the request's
  registered block numbers to the requested set
  (`miden-client-0.17.0-rc.4/src/store/data_store/mod.rs:115-121,335-342`).
- **[INFERRED]** Guardian's `DataStore` must do the same: the `PartialBlockchain` it returns must
  track `request.block_numbers()` (which on a valid request includes the bound block) in addition
  to the note blocks, or the host raises `TransactionSummaryUnknownBlockNumber`. FR-061.
- **[READ]** `execute_transaction(account_id, block_ref, notes, tx_args)` takes the reference
  block explicitly (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:189-194`), so Guardian chooses `R`.
- **[READ]** Foreign accounts are loaded lazily through
  `DataStore::get_foreign_account_inputs(id, ref_block)` (`executor/exec_host.rs:178-186`).

### Guarded multisig auth flow

- **[READ]** The guarded multisig component's auth procedure resolves the auth args, asserts the
  approval has not expired, pays the fee, builds and signs the summary, verifies the guardian
  signature, then applies the approval expiration
  (`miden-standards-0.17.0-rc.7/asm/components/auth/guarded_multisig/guarded_multisig.masm:49-113`).
  The library procedures it calls are in `asm/standards/auth/multisig.masm:827-1053`.
- **[READ]** `resolve_auth_args` requires the auth-arg preimage in the advice map
  (`ERR_AUTH_ARGS_PREIMAGE_MISSING`) and pipes **three** words: `[bound_block,
  approval_expiration, 0, 0]`, `SALT`, `CONVERSION_INFO` (`multisig.masm:827-854`). The approval
  expiration is user param 0 of the summary (`multisig.masm:956-965,995-1053`).
- **[READ]** `assert_approval_not_expired` aborts the VM with `ERR_MULTISIG_APPROVAL_EXPIRED` when
  `ref_block >= approval_expiration` and otherwise returns the delta clamped to
  `MAX_EXPIRATION_BLOCK_DELTA` (65,535) (`multisig.masm:884-914`).
- **[READ]** `apply_approval_expiration` calls `tx::update_expiration_block_delta` and must run
  after every event that reports the summary to the host (`multisig.masm:931-942`).
  `update_expiration_block_delta` accepts 1..65,535, adds it to the reference block, and only
  stores the result if it is lower than the current value, so it keeps the minimum
  (`tx.masm:143-169`).
- **[READ]** The expiration delta is relative: `get_expiration_delta` returns
  `expiration_block - ref_block`, or 0 when unset (`tx.masm:177-189`). At summary time the host
  rejects a summary whose metadata delta differs from the kernel's
  (`TransactionSummaryExpirationDeltaMismatch`, `miden-tx-0.17.0-rc.7/src/host/tx_event.rs:787-795`).
- **[INFERRED]** Two bounds result (D1). A relative transaction delta (256 for built-in families)
  is signed as a delta, so it reproduces at any `R`; the approval expiration is absolute and
  signed. The approval clamp runs after the summary, so the proven expiration is
  `R + min(256, approval_expiration - R)`, and a custom producer with no script delta falls back
  to the approval bound clamped to 65,535 from `R`. The proven expiration therefore moves with
  `R`, so the FR-046 horizon is measured as `proven expiration_block_num - R` for the attempt's
  own `R`.
- **[READ]** Neither SDK sets a transaction `expiration_delta` on `main`; the only `256` is the
  Rust client's stale-sync bound `max_block_number_delta(256)`
  (`crates/miden-multisig-client/src/builder.rs:354`). `TransactionRequestBuilder::expiration_delta`
  is still rejected together with a custom script
  (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:305,682-687`). The shared
  256-block constant and `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA = 28_800` are D1
  decisions, not yet implemented.

### Fees

- **[READ]** `compute_fee` is `(ilog2(clk + extra_cycles) + 1) × verification_base_fee` plus an
  output-notes fee that currently returns 0; the base fee is read from kernel memory populated
  from the reference block header (`tx.masm:309-339`, `prologue.masm:265-300`).
- **[READ]** `fee::pay_fee` first creates a FEE_SPONSORSHIP note for every network output note,
  pricing each through its target account's fee policy via FPI, "which requires that target to be
  provisioned as a foreign account" (`miden-standards-0.17.0-rc.7/asm/standards/fee/mod.masm:349-352`).
  A non-zero fee requires committed conversion info pinned to the native asset at 1/1
  (`ERR_FEE_CONVERSION_INFO_MISSING`, `ERR_FEE_CONVERSION_INFO_NOT_NATIVE`, `fee/mod.masm:379-444`).
  The brief cites `:378-440`; the procedure spans `:379-444`.
- **[READ]** Moving an asset whose faucet ID has the callback flag starts a foreign context
  against the faucet on every movement, even when it registers no callback
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/callbacks.masm:102-123`).
- **[INFERRED]** Foreign public accounts are on the normal path on devnet (fee faucet, callback
  assets, sponsorship pricing). The v1 exclusion (FR-050 revision 10) would refuse every devnet
  transaction and is **superseded**: FR-050 supports foreign public accounts loaded lazily at `R`
  through `GetAccount(account_id, block_num = R, details)`
  (`miden-node-proto-build-0.17.0-rc.3/proto/rpc.proto:33,345-412`); private or unservable
  foreign state fails pre-boundary with `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` (D2).
  The node's `GetAccount` error codes distinguish `AccountNotPublic` (3) and `BlockPruned` (5)
  (`miden-client-0.17.0-rc.4/src/rpc/errors/node/account.rs:28-45`).
- **[INFERRED]** Fee drift (N1, **open**): the TX_FEE amount enters the signed output-notes
  commitment, and Guardian's reproduction at `R` computes it from `R`'s base fee and its own
  cycle count. A base-fee change, or a cycle count crossing a power of two (an MMR walk for
  `bound < R` that the proposer's `bound == R` run did not do), changes the summary. The SDK
  cosigners carry the same risk and the SDK work observed no drift on devnet 70+ blocks past the
  bound block. Guardian classifies it as `GUARDIAN_EXECUTION_BINDING_MISMATCH` with a logged
  TX_FEE comparison (D3); it is a candidate upstream question 9.

### Client fee path: 2-word versus 3-word preimage

- **[READ]** miden-client commits a **two-word** `[SALT, CONVERSION_INFO]` preimage when it
  attaches fee conversion info (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:289-300`,
  `miden-standards-0.17.0-rc.7/src/account/auth/fee.rs:110-122`). The guarded multisig pipes three
  words, so that path aborts in `resolve_auth_args`.
- **[READ]** The client leaves a request with a non-empty auth arg untouched
  (`miden-client-0.17.0-rc.4/src/transaction/mod.rs:1535-1540`). Both SDKs set the three-word
  auth arg and preimage themselves (`docs/MIDEN_COMPATIBILITY.md`, "miden-client fee path").
- **[INFERRED]** FR-057 revision 10 (Guardian derives and attaches fee conversion info) is
  **superseded**: Guardian never derives, attaches or repairs it, refuses a request with an empty
  auth arg or a missing preimage (`GUARDIAN_EXECUTION_REQUEST_INVALID`, `auth_args_missing`), and
  passes the auth arg through unchanged.

### Sealed submission

- **[READ]** `SubmitProvenTx` takes `ProvenTransactionSubmission{transaction,
  SealedTransactionInputs{key_id, ciphertext}}`
  (`miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:8-28`); the key comes from
  `GetTransactionEncryptionKey` with validator attestations (`rpc.proto:74`,
  `submission.proto:51-97`).
- **[READ]** `seal_transaction_inputs(rng, key, tx_id, &TransactionInputs)`
  (`miden-client-0.17.0-rc.4/src/rpc/encryption.rs:445-460`); the client verifies the attested
  key against the genesis commitment and the tip's validator set before sealing
  (`transaction/mod.rs:779-845`).
- **[INFERRED]** The worker must keep the `TransactionInputs` from execution to sealing, and key
  fetch, attestation check and sealing must complete before the no-retry boundary (FR-059, D4 step
  10), so a failure fails and releases (`GUARDIAN_EXECUTION_SEALING_FAILED`) rather than
  stranding the account. Attestation validation is a sibling of question 2.

### RPC surface

- **[READ]** `GetBlockHeaderByNumber` takes `include_mmr_proof` and `include_protocol_config`;
  its MMR path is "based on the current chain length", returned with `chain_length`
  (`rpc.proto:268-295`). `main`'s RPC client hardcodes `include_protocol_config: None`
  (`crates/miden-rpc-client/src/lib.rs:302`) and has no `sync_chain_mmr` (the spike's wrapper is
  not on `main`).
- **[READ]** `SyncChainMmr` takes a `FinalityLevel {COMMITTED, PROVEN}` and returns the target
  header, `repeated primitives.Signature block_signatures`, and the target's `ProtocolConfig` when
  the caller starts at genesis (`rpc.proto:670-714`). The header response has no signatures.
- **[READ]** miden-client adjusts a returned path to a given forest with
  `adjust_merkle_path_for_forest`, which is `pub(crate)`
  (`miden-client-0.17.0-rc.4/src/sync/block_header.rs:219`).
- **[READ]** There is still no transaction-status RPC, but `SyncTransactions(block_range,
  account_ids)` returns `TransactionRecord{block_num, header, ...}` (`rpc.proto:90,798-841`) and
  `BlockSubscription` / `ProofSubscription` streams exist (`rpc.proto:122,128`). `main`'s RPC
  client already has `sync_transactions` (`crates/miden-rpc-client/src/lib.rs:518`).
- **[INFERRED]** `SyncTransactions` is a usable inclusion observation for FR-040's committed path;
  the "no transaction-status lookup" section below is restated accordingly. Which finality level
  `R` should be anchored to, and whether Guardian's `committed` means COMMITTED or PROVEN, are
  open (question 2, N4).

### The spike's tip `ChainView` is normative again (FR-061)

- **[INFERRED]** FR-061: `R` is the node's committed tip at attempt start. The `PartialBlockchain`
  at forest `R` is assembled from genesis-seeded `SyncChainMmr` peaks (the construction the spike
  validated live at 0.6 s cold start, see "Validated against public Miden testnet", 0.15/0.16
  evidence) plus `GetBlockHeaderByNumber(n, include_mmr_proof)` for the bound block and every
  authenticated note block, with each path adjusted from the returned `chain_length` to forest
  `R`. This addresses the defect for which the earlier `GetBlockHeaderByNumber` note-path test was
  withdrawn (paths opened at the current chain length, not the execution forest). If the node tip
  moves between the peaks read and a header read, the adjustment is what keeps the paths valid.
- **[INFERRED]** A node below the bound block is a retryable pre-boundary condition
  (`GUARDIAN_EXECUTION_CHAIN_BEHIND`), mirroring the SDK's `ChainBehindBoundBlock`.
- The spike code must be ported from 0.16 rc.9 to 0.17 rc.7, forward onto a branch from `main`
  (`blockchain.rs`, `store.rs`, the RPC `sync_chain_mmr`), rather than resumed on the stale branch.

### `ClientDataStore` and `Store` are still unusable

- **[READ]** `ClientDataStore` is still in a `pub(crate)` module
  (`miden-client-0.17.0-rc.4/src/store/mod.rs:65-71`); the only public path is the `testing`
  re-export (`lib.rs:345-358`). `Store` has 61 methods, 45 of them required
  (`store/mod.rs:191-839`). The round-2 decision (Guardian implements `miden_tx::DataStore`
  directly) stands. `chain_anchor_at_tip(tracked_blocks)` is private
  (`transaction/mod.rs:398-427`), a candidate Q5(b) ask shared with the web SDK.

### Request serialization is unversioned across rc.3 and rc.4

- **[READ]** `TransactionRequest` serialization carries no version tag. rc.4 writes
  `block_numbers` as the **first** field (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:447-477`);
  rc.3 starts with `input_notes` (`miden-client-0.17.0-rc.3/src/transaction/request/mod.rs:439-443`).
- **[INFERRED]** rc.3 and rc.4 bytes do not decode across each other. This is the concrete
  evidence for FR-014's serializer-identity allowlist, and `serializer_id` must name the
  miden-client / web SDK version that serialized the request (the TypeScript bytes come from the
  WASM's embedded client), not the Guardian SDK package version.

### Other re-citations

- **[READ]** The remote prover default timeout is still 10 s
  (`miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`). The 0.16 proving timings and the
  ~26 KB request size are historical: proofs are format 2 (VM 0.33) and the post-#498 request
  also carries `block_numbers` and the auth-arg preimage. Re-measure on 0.17.
- **[READ]** `explicit_input_notes` still exists
  (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:178-190`). The consume-notes
  pinning requirement stands, now checked at execution as `GUARDIAN_EXECUTION_REQUEST_INVALID`
  (`input_notes_not_pinned`).

## CORRECTED: note-path assembly is conditional, and `SyncNotes` supplies the pinned forest

The account and reference-chain snapshot is required for every transaction. Historical block
paths are required only for **authenticated** input notes. A serialized `TransactionRequest`
cannot determine that distinction: it contains note bodies, while Miden treats a note as
authenticated only when the prepared `InputNote` carries a `NoteInclusionProof`.

`build_chain_view` now accepts the prepared `InputNotes` and derives the branch directly:

- no input notes, or only unauthenticated input notes: perform the cold `SyncChainMmr` assembly
  and make **no** `SyncNotes` call;
- authenticated input notes: derive their block numbers and tags from their inclusion data,
  request snapshot-pinned paths, retain only the required blocks, and stop as soon as all are
  found.

The earlier `GetBlockHeaderByNumber` note-path experiment was not a valid snapshot test. On a
stable live tip its proof reported `chain_length = reference + 1`, while transaction execution
against reference block `N` needs the `N`-leaf forest committed by that reference header. The
path happened to track for the sampled blocks, but that did not establish the required forest.
That test and its claim are withdrawn.

`SyncNotes` exposes the missing pin: response paths are opened at forest `block_to + 1`. Thus an
execution at reference block `N` requests `block_to = N - 1`. The new raw RPC wrapper preserves
that explicit bound and rejects inverted ranges. The live test
`live_sync_notes_paths_track_against_the_execution_reference_forest` tracked 753 recent returned
paths against reference block 1,174,436 and built the expected `PartialBlockchain`.

This is a correctness primitive, not an unconditional performance optimization. `SyncNotes` is
tag/range based rather than exact-note based. A diagnostic genesis-to-tip query for the single
broad tag `0` returned 91,465 matching blocks and took 59.9 s. Production narrows the lower bound
to the earliest authenticated input-note block and stops once all required blocks appear, but an
exact-note or exact-block proof at an explicit target forest would be a materially better
stateless-executor API. This is now upstream Question 3.

## CORRECTED: expiration is not part of the signed transaction summary

> **2026-09-30, Miden 0.17**: no longer true on 0.17. The summary metadata packs the transaction's
> `expiration_delta` (relative to the reference block) and the host checks it against the kernel,
> and the multisig approval expiration is signed as user param 0. See "Miden 0.17
> re-verification", guarded multisig auth flow. The 0.15 analysis below is kept as the record.

The pinned Miden 0.15.3 `TransactionSummary` contains account delta, input notes, output notes,
and salt (`miden-protocol-0.15.3/src/transaction/tx_summary.rs:20-24`), and its commitment contains
exactly those four values (`:77-86`). Expiration is absent. Adding or tightening expiration
therefore does **not** by itself change the signed summary or proposal identity.

This corrects the older conclusion retained in "Measured: the default transaction never
expires" below. The finite-expiration requirement remains load-bearing for reconciliation, but
the reason Guardian v1 does not rewrite expiration is architectural: preserve exact request bytes
and their checksum, and keep transaction construction in the SDK/custom producer boundary. It is
not an FR-007 binding limitation.

The insertion mechanism is not uniform on Miden 0.15. `TransactionRequest` exposes no public
expiration mutator, and `TransactionRequestBuilder::build` rejects `expiration_delta` together
with `custom_script` (`miden-client-0.15.0/src/transaction/request/builder.rs:570-595`). Built-in
Guardian proposal families can add the expiration while constructing their scripts; an opaque
custom-producer request cannot be generically retrofitted and must encode finite expiration in
the producer-owned script. FR-051 and the SDK contract now record that distinction.

## Historical baseline before the Gate 0 spike

Before this spike, Guardian's Miden surface was **read-only**. The completed spike added the
optional `proving` feature, which enables `miden-tx` and
`miden-remote-prover-client`; `e2e` additionally enables `miden-client`,
`miden-testing`, and the contract test helpers. Default server builds still perform on-chain
interaction only through raw gRPC reads (`crates/miden-rpc-client/src/lib.rs`,
`crates/server/src/network/miden/mod.rs`).

The client owns execute → prove → submit:
`crates/miden-multisig-client/src/client/helpers.rs:343-373`.

Guardian's only role at execution time is the acknowledgment signature. The client calls
`push_delta`, receives `ack_sig`, and injects it into the transaction's advice map
alongside the cosigner signatures
(`crates/miden-multisig-client/src/client/helpers.rs:131-205`).

## A `TransactionSummary` cannot be proven

The issue's phrasing — "user submits a signed `TransactionSummary`, Guardian proves it" —
is not directly implementable. A summary is a *commitment* over (account delta, input
notes, output notes, salt), not a program. Proving requires executing a
`TransactionRequest` against real account state.

What Guardian stores today is a **semantic recipe**, not a transaction:
`ProposalMetadataPayload` (`crates/miden-multisig-client/src/payload.rs:13-65`) carries
`proposal_type`, `recipient_id`, `faucet_id`, `amount`, `salt`, `note_type`, and embedded
notes. The client rebuilds the `TransactionRequest` from that recipe at execute time.

### Rebuilding server-side was rejected

Two reasons, one of which is absolute:

1. **Risk (built-in types).** Reconstruction is possible but must produce byte-identical
   MAST for the binding check to pass. This is the M-08 / #229 bug class —
   non-deterministic request rebuild from signed metadata — which this repository has hit
   twice. A divergence surfaces as an opaque commitment mismatch, with no way for a user
   to tell whether their proposal or Guardian's rebuild is at fault. It would also
   duplicate every builder in `crates/miden-multisig-client/src/transaction/` into server
   code that is not the source of truth.
2. **Impossibility (custom types).** `with_custom_metadata`
   (`crates/miden-multisig-client/src/payload.rs:246-252`) sets `proposal_type` and
   nothing else — every other field is `Default::default()`. A custom proposal's stored
   metadata is a free-form label, `required_signatures`, `signers`, and the summary.
   There is no recipe, by design: the doc comment on `propose_custom_transaction`
   (`crates/miden-multisig-client/src/client/proposals.rs:299-300`) states that "the
   integration keeps its own recipe to execute later via `prepare_custom_execution`".

Consequence: because custom proposals can *never* be rebuilt, a bytes-carrying mechanism
is required regardless. Rebuilding does not replace it — it adds a second, riskier path
on top of it.

Custom proposal types are fully first-class at ingress; the former server-side allowlist
is gone and any non-empty trimmed `proposal_type` is accepted
(`crates/server/src/services/mod.rs:235-249`; test `normalize_payload_accepts_custom_proposal_type`
uses `"b2agg"`).

## The binding check already exists in the codebase

`prepare_custom_execution`
(`crates/miden-multisig-client/src/client/proposals.rs:388-460`) accepts serialized
`TransactionRequest` bytes, re-executes them, and refuses to release the acknowledgment
unless the derived summary commitment equals the signed one. The server-side design
relocates this check; it does not invent one.

## The bytes already exist at creation time for every proposal type

This is the finding that makes the opt-in cheap:

- **Built-in types**: `ProposalBuilder::build` constructs the `TransactionRequest` before
  `execute_for_summary` derives the summary.
- **Custom types**: `propose_custom_transaction(&mut self, transaction_request_bytes: &[u8], proposal_type: &str)`
  receives the bytes as its first argument
  (`crates/miden-multisig-client/src/client/proposals.rs:301`).

So declaring Guardian-execution intent requires no new request argument; attachment is an
SDK-internal write-path change. Built-in methods can also add the finite-expiration policy
internally. A custom producer already owns the opaque transaction recipe and must encode finite
expiration there on Miden 0.15.

## Measured payload cost

Measured directly by instrumenting `build_p2id_transaction_request` and running
`cargo test -p miden-multisig-client --lib build_p2id_transaction_request_respects_note_type`
(instrumentation reverted afterward):

| Transaction | `TransactionRequest::to_bytes()` |
|---|---|
| P2ID payment, private note | 26,078 bytes |
| P2ID payment, public note | 26,087 bytes |

≈ 35 KB once base64-encoded in JSON. Reference points: default request-body limit is
1 MB (`crates/server/src/middleware/body_limit.rs:8`), and pending proposals per account
are already capped via `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT`.

Every multisig builder uses `custom_script(...)` — including P2ID payments
(`crates/miden-multisig-client/src/transaction/payment.rs:52-53`) — so the serialized
request always embeds a `TransactionScript`, which is `Arc<MastForest>` + entrypoint.
Configuration transactions link the multisig library **dynamically**
(`crates/miden-multisig-client/src/transaction/configuration/config.rs:79-81`), so the
library MAST is referenced rather than embedded; their requests are expected to be
comparable or smaller than the payment figure above. **Not yet measured** — worth
confirming during implementation.

Precedent for payload growth already exists: `consume_notes_metadata_version` v2 embeds
fully serialized notes in the proposal payload
(`crates/miden-multisig-client/src/payload.rs:49-55`).

## Proposal identity is unaffected by a new payload field

The server derives proposal identity from the summary alone:
`delta_proposal_id(&account_id, nonce, tx_summary)`
(`crates/server/src/services/push_delta_proposal.rs:124-151`). Adding a
`transaction_request` field to the payload therefore does not change any proposal ID.

## Gaps the server must close

- **No threshold awareness.** The server has no notion of an account's signing threshold
  in production code. `Auth` carries only `cosigner_commitments`
  (`crates/server/src/metadata/auth/mod.rs:22-27`). `MidenAccountInspector` already knows
  the `openzeppelin::multisig::threshold_config` slot name
  (`crates/server/src/network/miden/account_inspector.rs:6`) but nothing reads it — a new
  accessor is required.
- **No signature verification.** `push_delta_proposal` stores cosigner signatures without
  cryptographically verifying them against the summary commitment
  (`crates/server/src/services/push_delta_proposal.rs:167-180`); the client verifies at
  execute time. Server-side execution must verify, or it burns proving resources on
  unusable input.

## Concurrency primitives already exist — reuse, don't invent

Review flagged that the reservation needed fencing and an atomic admission primitive. Both
already exist in this codebase and MUST be extended rather than duplicated:

- **`LeaseFence { lease_name, holder_id, fence_token: i64 }`**
  (`crates/server/src/storage/mod.rs:149-153`), already threaded through every custody write
  by canonicalization (`jobs/canonicalization/processor.rs:135`).
- **Status-conditional fenced writes**: `discard_candidate(..., fence)` commits the delete,
  the fence validation, and the conditional flag clear as **one** write, reporting
  `CanonicalWrite::{Applied, StaleLease, NotCandidate}`
  (`crates/server/src/storage/mod.rs:155-164`, `processor.rs:1085-1149`). That is exactly
  the compare-and-set-inside-a-fenced-write shape the reservation's admission needs.

## Canonicalization destroys the candidate *and* the proposal

`remove_candidate` (`jobs/canonicalization/processor.rs:1085-1149`) deletes an
unrecoverable candidate and then deletes its matching proposal, with the rationale that
"leaving it would strand it as `pending` forever and let clients re-submit a stale intent."

Two consequences for this feature, both of which changed the spec:

- A post-submission terminal state **cannot** be derived from the candidate on read — the
  candidate is gone. The outcome must be persisted before deletion (FR-041).
- The proposal is gone too, so promising "the proposal remains executable" after a
  post-submission failure would be false. Retry requires a *new* proposal (FR-042).

## No transaction-status lookup; expiration is the terminal bound

> **2026-09-30, Miden 0.17**: there is still no status-by-id RPC, but `SyncTransactions`
> returns per-account `TransactionRecord{block_num, header}` and `main`'s RPC client already wraps
> it, which is a usable inclusion observation for FR-040's committed path. See "Miden 0.17
> re-verification", RPC surface. `ProvenTransaction::expiration_block_num()` remains the
> authoritative expiration (FR-039).

`MidenRpcClient` (`crates/miden-rpc-client/src/lib.rs`) exposes `get_status`,
`get_block_header`, `submit_transaction`, `sync_state`, `get_notes_by_id`,
`get_account_commitment`, and `get_account_details` — **no transaction-status or inclusion
lookup**. So an unknown submission cannot be resolved by asking the node about the
transaction.

An account still at its base commitment is *not* evidence of a drop; it may still land.
The transaction's **expiration block** supplies the finite chain-height bound for resolution
once trustworthy observation is available, but the authoritative source matters:

- `TransactionRequest::expiration_delta` is **`Option<u16>`**, and the doc is explicit: "If
  `None`, the transaction will not expire" (`miden-client-0.15.0/src/transaction/request/mod.rs:112-114`).
  Executed code may also impose an expiration the request never asked for. So the request's
  delta is **not** a sound basis.
- `ProvenTransaction::expiration_block_num() -> BlockNumber`
  (`miden-protocol-0.15.3/src/transaction/proven_tx.rs`, via
  `TransactionOutputs::expiration_block_num`) is the authoritative value, is a concrete block
  number rather than an `Option`, and is available **after proving and before submitting** —
  exactly where FR-039 needs it.

Because a non-expiring transaction surfaces as a very distant block rather than an absent
value, FR-046 refuses submission when the expiration falls outside a configured
reconciliation horizon. Without that refusal, FR-040's termination guarantee degrades to
best-effort and a reservation could be held indefinitely.

Not verified: whether the Miden node offers a transaction-status RPC upstream that our thin
client simply does not wrap. If it does, it is a faster path to the landed/superseded
outcomes; the expiration bound stays the backstop either way.

## Signatures are stored unverified, with no way to remove one

`sign_delta_proposal` (`crates/server/src/services/sign_delta_proposal.rs:24-110`)
authenticates the *caller* and derives the signer commitment from their pubkey, but stores
the supplied `signature` verbatim — it is never checked against the summary commitment.
There is no endpoint to remove or replace a stored signature, and **no route deletes a Miden
proposal**: `delete_delta_proposal` is called only internally (`delta_commit.rs`,
`evm/service.rs`, `canonicalization/processor.rs`). Despite issue #337's title, the PR that
landed (#350) was the viable-proposal-cap fix.

So treating any invalid signature as fatal at execution would let one cosigner permanently
brick Guardian execution for a proposal, with no recovery path. Hence FR-006 selects the
valid subset and ignores the rest.

## Optimistic mode has no landing signal

`CanonicalizationConfig` is optional — `None` means optimistic delta-commit mode, where
deltas are accepted without on-chain verification (`services/delta_commit.rs`
`DeltaCommitStrategy::Optimistic`, `builder/mod.rs`, `services/dashboard_info.rs`). Since
FR-040 resolves outcomes purely by chain observation, that mode cannot support Guardian
execution — hence FR-043 refuses it outright rather than defining a weaker second lifecycle.

## Proving: remote-only

`miden-remote-prover-client` is a real crate at the 0.15 line (0.15.0 and 0.15.1 present
in the local registry). `RemoteTransactionProver::new(endpoint)` takes a
`{protocol}://{hostname}:{port}` string, exposes `.with_timeout(Duration)`, and
`prove(&TransactionInputs) -> Result<ProvenTransaction, TransactionProverError>`
(`miden-remote-prover-client-0.15.1/src/remote_prover/tx_prover.rs:38-108`).

**The default timeout is 10 seconds** (`tx_prover.rs:45`), which is far below realistic
proving times. It must be configured explicitly.

miden-client also defines a `TransactionProver` trait with both `LocalTransactionProver`
and `RemoteTransactionProver` impls
(`miden-client-0.15.0/src/transaction/prover.rs:8-35`). The design deliberately does not
use the trait: Guardian always constructs a `RemoteTransactionProver`, and an operator
wanting local proving runs `miden-remote-prover` as a sidecar and points the URL at it.
Local proving becomes a deployment topology rather than a second code path to test.

## Gate 0 spike, round 1: the recorded architecture is falsified

**`ClientDataStore` is `pub(crate)` and cannot be used from `guardian-server`.**
`miden-client-0.15.0/src/store/mod.rs:62` declares `pub(crate) mod data_store;`, there is no
`pub use` of it anywhere in the crate, and `store/mod.rs` exports only `Store`, the filter
enums, and the record types. The doc comment says so outright: it "isn't public because it's
an implementation detail to instantiate the executor."

This invalidates the decision recorded below, which was to drive
`miden-tx::TransactionExecutor` through `ClientDataStore` over a `Store` implementation. Two
consequences:

- **Implementing `Store` is pointless.** Its 46 required methods only matter to something that
  consumes a `Store`, and the only such thing is crate-private. The "46 methods vs 7" tradeoff
  that drove the original decision was therefore comparing against an unavailable option.
- **Guardian must implement `miden_tx::DataStore` directly** — the route the original spike
  rejected as "a large amount of security-sensitive code written from scratch."

### Superseded round-1 direct route

> **Superseded by round 2 below.** This was the first direct-`DataStore` recipe, retained to
> show how the spike converged. `AccountSmtForest` cannot produce required non-inclusion
> witnesses; the implemented route uses `AssetVault::open` / `StorageMap::open`,
> `TransactionMastStore`, and no direct `miden-client` or `miden-processor` dependency.

`miden-tx-0.15.3/src/executor/data_store.rs` requires **five** methods, plus
`MastForestStore::get` (one method, `miden-processor-0.25.7/src/host/mast_forest_store.rs:52`):

| Method | How Guardian answers it |
|---|---|
| `get_transaction_inputs` → `(PartialAccount, BlockHeader, PartialBlockchain)` | `PartialAccount::from(&Account)` exists (`miden-protocol-0.15.3/src/account/partial.rs:162`); Guardian holds the full account. Header + peaks from RPC |
| `get_vault_asset_witnesses` | `AccountSmtForest::get_asset_and_witness` |
| `get_storage_map_witness` | `AccountSmtForest::get_storage_map_item_witness` |
| `get_foreign_account_inputs` | RPC, only for FPI transactions |
| `get_note_script` | Guardian holds the notes (consume_notes v2 metadata embeds them) |
| `MastForestStore::get` | Delegate to `MemMastForestStore` |

The witness assembly that made this route look expensive is **public**:
`AccountSmtForest` is exported at `miden-client-0.15.0/src/store/mod.rs:68` with
`new`, `insert_account_state`, `insert_asset_nodes`, `insert_storage_map_nodes`,
`get_asset_and_witness`, and `get_storage_map_item_witness`. The sqlite store's own witness
code is a thin wrapper over it (`miden-client-sqlite-store-0.15.0/src/account/accounts.rs:245-300`),
so Guardian can do the same. Holding the *full* account state is what makes this easy — the
sqlite store does extra work precisely because it does not.

### Superseded round-1 design consequences

- **No `Store`, so the sqlite-versus-in-memory question dissolves.** There is no temp
  directory, no sqlite dependency, and no seeding I/O. SC-011's seeding cost shrinks to
  building an SMT forest from an account Guardian already has in memory, plus one RPC read.
- **`miden-processor` becomes a direct dependency** of `guardian-server`, pinned to the version
  miden-tx uses (0.25.x), because `LoadedMastForest` and `MemMastForestStore` are not
  re-exported by `miden-protocol` or `miden-tx`.
- **The remaining real risk is `PartialBlockchain` assembly** — MMR peaks plus authentication
  paths for the reference block. This is the one piece `ClientDataStore` would have provided,
  and it is where the "reference block == chain tip" simplification earns its keep.
- The ephemeral, per-execution, seeded-at-tip shape **survives** and gets simpler: there is no
  store object to seed, only a forest to build and a header to fetch.

## Miden already solves "prove without a store" — by shipping the witness

Checked whether the ecosystem has a reusable pattern before writing an MMR cache. It does,
and it reframes this feature.

`RemoteTransactionProver::prove(&TransactionInputs)`
(`miden-remote-prover-client-0.15.1/src/remote_prover/tx_prover.rs:104-140`) serializes the
witness and ships it over gRPC. The remote prover is **stateless**: it needs no `DataStore`,
no MMR, and no account state, because `TransactionInputs` is self-contained and
`Serializable`/`Deserializable`
(`miden-protocol-0.15.3/src/transaction/inputs/mod.rs:54-64, 498, 511`):

```text
PartialAccount, BlockHeader, PartialBlockchain, InputNotes,
TransactionArgs, AdviceInputs, foreign_account_code, ...
```

That is precisely the bundle GUARDIAN's `DataStore` exists to construct — **peaks included**,
inside the `PartialBlockchain`. Miden's answer to "execute/prove somewhere that has no store"
is not to rebuild the inputs remotely; it is to **transport them**.

### Two architectures, and they are different features

| | **Guardian executes** (what #254 specifies) | **Guardian proves a client-built witness** (the Miden pattern) |
|---|---|---|
| Client sends | serialized `TransactionRequest` | serialized `TransactionInputs` + signatures |
| GUARDIAN needs | `DataStore` (done, works) **+ MMR peaks** | **nothing new** |
| MMR problem | blocking — no stateless path | **absent** — peaks arrive in the witness |
| Caller needs Miden capability | **no** (FR-034 holds) | **yes** — must execute locally |
| Cost to build | high | low, on existing primitives |

The second row is the whole story: the witness-transport route needs no new server capability
at all, while the execute-server-side route is blocked on the MMR gap documented below.

### Consequence for sequencing

The witness-transport route is essentially **#180 (delegated proving for operators)**, and it
is nearly free. It delivers the substance of the practical complaint — proving is the slow,
expensive step, and submission is the liveness burden — while leaving the caller a full Miden
client.

**Superseded in part.** This section argued for landing #180 before #254 because #254 was
blocked on the MMR. That blocker was not real, so the sequencing argument loses its force:
architecture A can proceed now. Architecture B remains genuinely useful for **#180**, but it
does **not** satisfy #254's requirement that the caller need no Miden capability, so it is not
a substitute. Keep the custom `DataStore`.

The `DataStore` work from round 2 is not wasted either way: it is exactly what #254 needs, and
it is proven to work.

Unmeasured: the serialized size of `TransactionInputs`. It contains the ~26 KB transaction
script plus account and chain data, so it is the same order of magnitude as the request but
larger. Worth measuring before committing to the wire shape.

## Validated against public Miden testnet

> **2026-09-30**: these runs were on the 0.15 line (and the spike later on the 0.16 release
> candidates). Testnet still runs Miden 0.16, so none of them cover 0.17. On 0.17 the live
> environment is devnet (node 0.17.0-rc.2), and the testnet-equivalent is a local `miden-node`
> from the pinned line. The `SyncChainMmr` cold-start construction validated here is normative
> again (FR-061) once ported.

Chain-MMR acquisition turned out to be **entirely read-only** — `get_block_header` and
`sync_chain_mmr` are queries — so validating it needed no account, no funds, and no
infrastructure of our own. `NetworkType::MidenTestnet` already pointed at
`https://rpc.testnet.miden.io`.

Three `#[ignore]`d tests in `live_tests.rs`, run with:

```bash
cargo test -p guardian-server --features e2e --lib live_ -- --ignored --nocapture
```

| Test | Result |
|---|---|
| `live_cold_start_chain_mmr_matches_the_reference_block` | Genesis-seeded `PartialMmr` + `SyncChainMmr(0)` + apply → `hash_peaks()` **matches** the reference header's `chain_commitment`, at block 1,002,185 |
| `live_sync_notes_paths_track_against_the_execution_reference_forest` | `SyncNotes(block_to = reference - 1)` tracked 753 recent paths against the execution forest at reference 1,174,436; separate full-range diagnostic for tag `0` returned 91,465 blocks in 59.9 s |
| `live_prove_a_guardian_assembled_witness` | Witness assembled from **live** chain data, executed, and proved by `https://tx-prover.testnet.miden.io` |

What this settles:

- **The node does map `current_client_block_height: 0` to a delta from a one-leaf forest.**
  Previously our least-verified load-bearing claim.
- **The logarithmic-payload property is confirmed empirically.** Cold start completed in ~0.6 s
  against a chain of over a million blocks; a payload proportional to chain length could not.
- **`SyncNotes` paths track correctly against the execution forest when requested through
  `reference - 1`.** The earlier arbitrary-block `GetBlockHeaderByNumber` test did not prove this
  and has been withdrawn; see the newest correction above.
- **Submission and the joined live note-consumption flow remain untested.** Submission needs a
  funded, Guardian-registered account rather than infrastructure.

### Measurements

| Quantity | Value |
|---|---|
| Testnet node version | **0.15.0** — same line as our dependencies, so **no version skew on testnet** (devnet has been on 0.16 prereleases; testnet has not) |
| Serialized witness (`TransactionInputs`) | **~270 KB** (269,587–270,131 across runs) — about **10×** the 26 KB serialized `TransactionRequest` |
| Remote proving | 6.2 s / 13.8 s / 20.1 s across three consecutive runs of the same transaction |
| Local proving | ~60 s |
| Per-execution store setup | negligible; memory-only, no database, no temp directory |

The witness figure corrects an earlier guess of "same order of magnitude, but larger" — it is an
order of magnitude larger. That is the real per-transaction cost of architecture B / #180, where
the client ships the witness instead of the request.

### The default prover timeout is a live failure mode

`RemoteTransactionProver::new()` leaves a **10-second default timeout**
(`miden-remote-prover-client-0.15.0/src/remote_prover/tx_prover.rs:45`). The first live attempt
failed with nothing but `"failed to prove transaction"` — the gRPC status sits in the error's
source chain and the message names no timeout. Setting an explicit 300 s timeout made three
consecutive runs pass.

Since observed proving times (6–20 s) **straddle** the default, leaving it produces an
*intermittent* failure pointing nowhere useful. FR-020 already required configuring this
explicitly; it is now a confirmed failure mode rather than a precaution, and belongs in operator
troubleshooting documentation.

Secondary lesson for our own code: wrapping a prover error with `{e}` discards the cause. The
live test now walks the full `std::error::Error` source chain.

### Expiration: both halves measured

- **Default is unbounded.** A transaction with no explicit `expiration_delta` proves to
  `expiration_block_num = u32::MAX` (`guardian_executes_signs_and_proves_end_to_end`).
- **A client-set delta is finite and exact.**
  `build_send_notes_script(notes, Some(100))` proves to `reference_block + 100`
  (`guardian_proves_a_transaction_with_a_finite_expiration`).

So the finite-expiration requirement is load-bearing rather than defensive, and the mechanism the
client must use to satisfy it is confirmed to work.

## All four proposal families execute through GUARDIAN's DataStore

Both remaining families now run with their **real** scripts rather than
`TransactionArgs::default()`. Six tests pass; clippy clean; 772 existing server tests unaffected.

| Family | Script source | Notes |
|---|---|---|
| P2ID send | `AccountInterface::build_send_notes_script` — the same upstream primitive the SDK's `build_p2id_transaction_request` uses | Vault had to be funded first; sending an unheld asset aborts on a kernel vault-balance assertion, which is itself evidence the vault witnesses are genuinely read |
| Configuration | Real `update_signers_and_threshold` from `get_multisig_library()`, with script-arg and config-hash advice | The advice payload is reconstructed rather than imported, and is **self-validating**: the MASM recomputes the config hash and aborts on mismatch, so a wrong payload fails the test instead of passing quietly |
| `consume_notes` | Real chain-committed P2ID note | Prepared execution and snapshot-pinned live-RPC note-block assembly are independently covered; joined live flow pending |
| Custom (#266) | not covered | Structurally identical — an opaque script through the same seam |

The server cannot depend on the multisig SDK, so scripts are built from the upstream primitives
the SDK itself uses rather than by importing its builders. That is a deliberate constraint, and
it means these tests would not catch SDK-side builder drift — only that the seam accepts real
scripts of each shape.

## Measured: the default transaction never expires

`guardian_executes_signs_and_proves_end_to_end` drives the full authorized path through
GUARDIAN's `DataStore` — execute unsigned, sign as cosigner *and* as GUARDIAN, re-execute with
both signatures, prove locally — and prints the proven transaction's expiration:

```text
expiration_block_num = 4294967295   (u32::MAX)   reference_block = 0
```

A transaction built without an explicit `expiration_delta` is **non-expiring**. This confirms
FR-039 (the authoritative expiration is on the proven transaction, available before submission)
and materially changes the standing of FR-046: the finite-expiration refusal is **load-bearing,
not defensive**, because non-expiring is the *default* rather than an edge case. Without it
every execution would sit unbounded in the unknown-submission state and FR-040's `expired`
evidence path would never fire.

**Earlier conclusion (withdrawn):** this section originally said Guardian could not add an
expiration because doing so would change the summary and break FR-007 binding. That was false:
expiration is not part of `TransactionSummary`. The current v1 decision is instead that built-in
SDK builders apply the shared finite policy, custom producers encode it in their opaque scripts,
and Guardian enforces the executed/proven result without rewriting stored request bytes. That is
FR-051 plus FR-046. The test still asserts the non-expiring sentinel deliberately, so an upstream
default change forces the policy to be revisited.

Incidental datum: local proving took ~60s for a minimal configuration transaction, against a
per-execution store setup that is memory-only. For SC-011's purposes, proving dominates setup
by orders of magnitude.

## CORRECTED: PartialBlockchain from RPC is viable — the MMR blocker was not real

Review refuted the blocker below, and the refutation checks out. **Retained for the record;
the conclusion is wrong.**

`Mmr::get_delta` returns **merge authentication nodes plus new peaks — not the intervening
blocks**. `miden-crypto-0.25.1/src/merkle/mmr/tests.rs:1241-1245` asserts it directly:

```rust
assert_eq!(&mmr.get_delta(Forest::empty(), mmr.forest()).unwrap().data,
           acc.peaks(), "all peaks");
```

and from forest 1 the payload is `"one sibling, two peaks"` (`tests.rs:1236-1240`). The
payload tracks the **peak count — logarithmic in chain length**, not the chain length. My claim
that a cold-start delta is "proportional to chain length" was simply false, and it was the sole
basis for calling this a blocker.

**Consequence: architecture A proceeds with no persistent MMR cache and no upstream
dependency.** Cold start per execution is one compact `SyncChainMmr` call.

One correction to the recipe: `current_client_block_height: 0` means *"I already have block
0"*, matching the field's own documentation. So seed the `PartialMmr` with the **genesis block
commitment** and then apply the delta — do **not** apply it to a completely empty MMR. Verify
by checking `hash_peaks()` equals the returned reference header's `chain_commitment`.
**Historical note — later confirmed live.** At this point the node-side mapping from
`current_client_block_height` to `get_delta`'s `from_forest` had only been read from the field
contract. The later `chain_mmr_peaks_hash_to_the_reference_block_commitment` live test confirmed
the genesis-seeded recipe at three successive public-testnet heights.

**The upstream ask is downgraded from blocker to nicety.** A direct peaks field or
`GetChainMmrPeaks` would still save the genesis seeding step and one round trip, but nothing
depends on it. Filing it remains optional.

### Miden 0.16 does not change any of this

Checked against PR #340's dependency set — all **prereleases**: `miden-protocol`,
`miden-standards`, `miden-tx` at `0.16.0-alpha.4`; node proto `0.16.0-alpha.2`; Rust client
`0.16.0-alpha.1`.

- No peaks field and no peaks RPC added.
- `ClientDataStore` still behind a crate-private module.
- `AccountSmtForest` still rejects absent vault assets; `AssetVault::open` remains the fix.
- MMR delta behavior effectively unchanged from the 0.15 line.

So 0.16 is porting work with no blocker-removing capability. There is no reason to wait for it.

**2026-09-30, Miden 0.17**: still no peaks field or peaks RPC, so the genesis-seeded
`SyncChainMmr` cold start remains the peaks source; the response now also carries the target
header's validator signatures, a `FinalityLevel`, and its `ProtocolConfig` from genesis.
`ClientDataStore` is still crate-private. See "Miden 0.17 re-verification".

## SUPERSEDED: PartialBlockchain from RPC: there is no stateless path

Investigated before implementing. The conclusion is a genuine constraint, not a coding
problem.

`PartialBlockchain::new(PartialMmr, headers)` needs a `PartialMmr`, which needs
`MmrPeaks`, which needs the actual peak digests — `MmrPeaks::new(forest, peaks: Vec<Word>)`
(`miden-crypto-0.25.1/src/merkle/mmr/peaks.rs:62`). Peaks are not derivable from the block
header's `chain_commitment`; the commitment is their hash.

What the node RPC actually offers:

| RPC | Gives | Enough for peaks? |
|---|---|---|
| `GetBlockHeaderByNumber` | `block_header`, `mmr_path`, `chain_length` | **No.** Per-block inclusion path only |
| `SyncChainMmr` | `MmrDelta` — "data needed to update the partial MMR from `current_client_block_height + 1` to the sync target" | **Only incrementally** |

`SyncChainMmr` is *by construction* a delta against a height the caller already holds. There
is **no RPC that returns MMR peaks at a block from scratch**. So a stateless executor has
exactly two options:

1. **Re-sync from genesis every execution** — `SyncChainMmr { current_client_block_height: 0 }`
   into an empty `PartialMmr` via `PartialMmr::apply` (`partial.rs:516`). Correct, but the
   delta is proportional to chain length, paid per execution. Tolerable on a young devnet,
   untenable on a mature chain.
2. **Keep a `PartialMmr` across executions**, advanced by `SyncChainMmr` deltas.

This is why the client "just works" locally: miden-client accumulated these MMR nodes
incrementally during sync (`apply_state_sync` → `insert_partial_blockchain_nodes`), so by
execution time the answers are already in its database. The cost was paid earlier, not avoided.

### What this changes

The ephemeral design survives for **account** state — that part is genuinely derivable from
what GUARDIAN already holds. But **chain MMR state is history, not current state**, and cannot
be reconstructed from the account. The spec's claim that "GUARDIAN holds no synced chain state
between executions" is therefore **false as written** and must be corrected.

Option 2 is the right answer, and it is materially weaker than the per-account synced store
rejected in round 1:

- it is **shared across all accounts** — chain state, not custody state, so no per-account
  coherence problem;
- it is **fully derivable from scratch** at any time, so it is a cache, not a source of truth;
- **losing it is not a correctness event**, only a slow path.

So it does not reintroduce the multi-replica coherence or restart-replay problems that
motivated the ephemeral design — but it is state, and the spec must say so.

### Upstream ask (recommended)

This is a protocol-API gap worth raising with the Miden team, and it is well-scoped:

> Expose MMR peaks for a given block over RPC — either as an additional field on
> `BlockHeaderByNumberResponse`, or a `GetChainMmrPeaks(block_num)` method.

Rationale: any **stateless** executor needs to build a `PartialBlockchain` without carrying a
synced MMR. That is not Guardian-specific — batch provers, relayers, delegated-proving
services, and server-side co-signers all hit it. The current API assumes a long-lived client
that syncs. With peaks exposed, the whole chain-MMR cache disappears and the ephemeral design
holds exactly as originally specified.

Worth filing regardless of whether we wait for it: it converts a permanent cache into a single
RPC call. A second, lower-priority ask is making `ClientDataStore` public (it cost round 1),
though we no longer need it and ended up with fewer dependencies without it.

## Gate 0 spike, round 2: the DataStore seam works

Implemented at `crates/server/src/network/miden/execution/` (on the [`254-execution-spike`](https://github.com/OpenZeppelin/guardian/tree/254-execution-spike) branch, commit `769e2a90`; not on `main`) behind a new `proving` feature,
driven under `miden-testing`'s `MockChain`. Two tests pass:

| Test | What it proves |
|---|---|
| `guardian_data_store_serves_a_full_transaction_execution` | The kernel executes to completion against GUARDIAN's own `DataStore` and stops only at signature verification |
| `guardian_data_store_serves_note_consumption` | **Prepared** authenticated input-note execution: the custom `DataStore` can execute note consumption when the note and its inclusion data are supplied. `MockChain` provides both `latest_partial_blockchain()` and the authenticated `InputNote`, so this does **not** prove Guardian can assemble a note block's MMR path from live RPC — that remains pending |

Reaching `TransactionExecutorError::Unauthorized` is the assertion that matters: the VM
accepted every answer the store gave — partial account, reference block, partial blockchain,
vault witnesses, storage-map witnesses, account MAST, note scripts — and halted only because
the transaction is unsigned.

`cargo clippy` is clean, the default (non-`proving`) build is unchanged, and all 772 existing
server lib tests still pass.

### Two corrections to round 1

**`AccountSmtForest` is the wrong tool, and `miden-client` is not needed at all.**
`get_asset_and_witness` returns `UntrackedKey` when the value is empty
(`smt_forest.rs:50-67`), so it cannot produce **proofs of non-inclusion** — and the executor
asks for exactly those whenever a vault key is absent. The first run failed on it:
`FetchAssetWitnessFailed(... "merkle store error")` against an empty vault. Its inner `forest`
field is private, so `open` is not reachable through it.

The account's own SMTs solve it, and they live in `miden-protocol`:

- `AssetVault::open(vault_key) -> AssetWitness` (`asset/vault/mod.rs:146`) yields a witness
  whether or not the key is present.
- `StorageMap::open(&key) -> StorageMapWitness` (`account/storage/map/mod.rs:144`), located by
  matching `map.root()` against the requested root.

So `proving = ["miden-tx"]` — **no `miden-client` dependency, and no `miden-processor`
dependency either.** Round 1's claim that `miden-processor` would be needed for
`LoadedMastForest` was wrong on version grounds: the lockfile pins miden-processor **0.23.3**,
whose `MastForestStore::get` returns `Option<Arc<MastForest>>`. `LoadedMastForest` only appears
in 0.25.x, which is present in the local registry but not in use. `miden_protocol` re-exports
`MastForest` (lib.rs:28) and `vm::FutureMaybeSend`, so nothing new is required.

**Registering `account.code().mast()` is not sufficient.** The second failure was
`ProcedureNotFound`: the multisig and guardian libraries are *dynamically* linked, so their
procedures resolve through the MAST store rather than being embedded in the account code.
`miden_tx::TransactionMastStore` is public and its `load_account_code(&AccountCode)` registers
the account's procedures plus the kernel and component libraries it reaches by `ExternalNode`.
Using it fixed the run. This is the same thing miden-testing's own harness does
(`tx_context/builder.rs:323-331`).

### Still not verified — SUPERSEDED, see "Validated against public Miden testnet" below

**This section is stale and retained only for the record.** It was written at the end of
round 2 and every item in it has since been closed. Do not read it as open scope.

- ~~**Proving and submission are not wired.**~~ Proving is wired and validated: a witness built
  from live testnet chain data was proved through `RemoteTransactionProver`
  (`live_prove_a_guardian_assembled_witness`). **Submission remains genuinely deferred** — it
  needs a funded, Guardian-registered account.
- ~~**`PartialBlockchain` is supplied by `MockChain`, not built from RPC.**~~ Assembly from RPC
  is implemented in `blockchain.rs` and validated live, gated on
  `hash_peaks() == chain_commitment`. Cold start measured at ~0.6 s on a 1,002,185-block chain.
- ~~**Only two of the four families are covered** / real tx scripts not used.~~ P2ID-send and
  configuration are driven with their real scripts. `consume_notes` executes from a prepared
  store but its live-RPC note-block path is still unexercised, and the **custom** family (#266)
  is still unrun — both tracked as deferred coverage in `validation-matrix.md`.
- **Foreign account inputs (FPI) are refused outright**, so any proposal reaching into another
  account is unsupported. Acceptable for v1 but must be stated. **Superseded 2026-09-30**: on
  Miden 0.17 every devnet fee payment loads the fee faucet as a foreign account, so FR-050 now
  supports foreign public accounts loaded at `R` (see "Miden 0.17 re-verification", fees).
- **The SC-011 seeding figure is not measured**, though the shape is now known to be cheap: no
  database, no temp directory, no I/O — one `TransactionMastStore` load plus witnesses opened
  on demand from in-memory SMTs.
- **The Miden 0.16 line is unchecked.**

## Superseded: original architecture spike

Executing a transaction server-side requires a `miden-tx` `DataStore`
(`miden-tx-0.15.3/src/executor/data_store.rs:18`): `get_transaction_inputs` (returning
`PartialAccount`, `BlockHeader`, `PartialBlockchain`), `get_foreign_account_inputs`,
`get_vault_asset_witnesses`, `get_storage_map_witness`, note-script lookup, plus the
`MastForestStore` supertrait. A serialized `TransactionRequest` supplies none of this —
it defines *what* to run, not the authenticated state to run it against.

### Findings

- **`ClientDataStore` is public and reusable.**
  `miden-client-0.15.0/src/store/data_store/mod.rs:44` — "Wrapper structure that
  implements `DataStore` over any `Store`". Constructed via
  `new(Arc<dyn Store>, Arc<dyn NodeRpcClient>)` (`:54`), implements `DataStore` (`:205`)
  and `MastForestStore` (`:432`), and lazy-loads foreign account data from RPC on cache
  miss.
- **It takes `Arc`, not `&mut`.** `Store` is declared `pub trait Store: Send + Sync`
  (`miden-client-0.15.0/src/store/mod.rs:119`). The earlier concern about `&mut self`-heavy
  APIs applies to miden-client's `Client` façade, which this route bypasses entirely —
  `TransactionExecutor` + `ClientDataStore` is sufficient and shareable. This removes the
  conflict with the de-mutexed `NetworkClient` direction.
- **Hand-writing `Store` is not viable: 52 methods** (counted over
  `store/mod.rs:119`+) spanning transactions, notes, note scripts, block headers, MMR
  nodes, accounts, addresses, settings, note tags, and sync state. Implementing
  `DataStore` directly (~7 methods) would be less work than implementing `Store`.
- **Execution needs less than a full sync.** `ClientDataStore::get_transaction_inputs`
  (`store/data_store/mod.rs:205-275`) reads exactly: `get_current_blockchain_peaks`,
  `get_minimal_partial_account` (or `get_account` when nonce == 0),
  `get_block_header_by_num(ref_block)`, and `get_block_headers` for the remaining
  reference blocks. Guardian already holds the account state; the rest is RPC-fetchable at
  execution time.
- **Seeding at the tip sidesteps an upstream bug.** That function carries a `TODO`
  (`store/data_store/mod.rs:260-262`): the client stores only MMR peaks at the current
  sync height, so if `block_ref != current_sync_height` the returned `PartialBlockchain`
  is invalid. A store seeded at the tip makes `ref_block == sync_height`, so this route
  avoids the defect rather than inheriting it.
- **The remote prover comes along for free.** miden-client's default `std` feature already
  enables `miden-remote-prover-client/tx-prover` (`Cargo.toml:68-78`), and `std` also
  pulls `tempfile` — useful for a per-execution scratch store.

### Decision

**Ephemeral, per-execution data store seeded at the chain tip**, driving
`miden-tx::TransactionExecutor` through `ClientDataStore`, bypassing miden-client's
`Client` façade. Seed from Guardian's own stored account state plus the tip block header
and blockchain peaks fetched at execution time; discard at terminal state.

Consequences, all of which simplify the lifecycle requirements:

- No sync loop, no chain-following, no long-lived per-account store.
- No cross-execution store state, so no multi-replica store-coherence protocol. The only
  durable coordination is the execution reservation.
- Restart recovery needs no store replay; an interrupted execution is resolved by rule.
- Reference block always equals sync height.

Cost: a real `Store` implementation is still required to back `ClientDataStore`. Using
`miden-client-sqlite-store` in a temp directory per execution is the low-code option;
seeding latency must be measured (spec SC-011) but is expected to be small relative to
proving.

### Architecture is provisional until the spike passes

Review of this document flagged that the decision above rests on reading upstream code, not
on running it. The architecture MUST NOT be treated as final until a compile-tested spike
executes, proves, and submits **P2ID, configuration, custom, and `consume_notes`**
transactions end to end. `consume_notes` is the one that matters most: it is the only family
requiring input notes in the store, and that path is entirely untraced. See Gate 0 in
`validation-matrix.md`.

### Still unverified

- Whether `miden-client-sqlite-store` can be seeded and torn down per execution cheaply
  enough, and whether a lighter in-memory `Store` is worth writing instead. Measure before
  committing to sqlite.
- Whether `miden-remote-prover-client` and this execution route both hold at the 0.16
  line, if #329 lands first.
- Input-note supply for `consume_notes` proposals: the v2 metadata embeds serialized notes
  in the payload, which should satisfy the store's note requirements, but this path has
  not been traced end to end.
