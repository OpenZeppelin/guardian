# Miden compatibility

Which Miden protocol line each Guardian release targets, what changed between
lines, and what each upgrade does to stored data.

> Guardian's own version and Miden's are **not** aligned. Guardian 0.16.x runs on
> Miden 0.15; Miden 0.16 arrives in Guardian 0.17.x; Miden 0.17 arrives in
> Guardian 0.18.x. Read the matrix rather than matching the numbers.

This page is the single source of truth for those facts. Procedures live
elsewhere and link here:

| For | Read |
|---|---|
| Operator upgrade steps | [`PRODUCTION.md`](./PRODUCTION.md) |
| Diagnosing a version-mismatch symptom | [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) |
| SDK contract pinning and release policy | [`MULTISIG_SDK.md`](./MULTISIG_SDK.md#contract-version-pinning) |

## Support matrix

| Guardian | Miden protocol | `miden-protocol` / `miden-standards` | `miden-client` (Rust) | `@miden-sdk/miden-sdk` (npm) |
|---|---|---|---|---|
| 0.18.0 | 0.17 | `=0.17.0` | `=0.17.0` | `0.17.0` (exact) |
| 0.17.0 | 0.16 | `=0.16.1` | `=0.16.0` | `0.16.0` (exact) |
| 0.16.x | 0.15 | `0.15.3` | `0.15.0` | `^0.15.8` |
| 0.15.x | 0.15 | `0.15.x` | `0.15.0` | `^0.15.0` |
| 0.14.x | 0.14 | n/a | `0.14.x` | `^0.14.0` |
| 0.13.x | 0.13 | n/a | `0.13.0` | `^0.13.0` |
| 0.12.x | 0.12 | n/a | `0.12.5` | `^0.12.5` |

0.18.0 builds on the stable Miden 0.17 release. `@miden-sdk/miden-sdk` 0.17.0 embeds
`miden-client` 0.17.0 and `miden-protocol` / `miden-standards` 0.17.0, so every Rust pin
is 0.17.0. The 0.18.0 release candidates tracked the Miden 0.17 release candidates, were
published to npm under the `rc` dist-tag, and are not supported.

0.17.0 builds on the stable Miden 0.16 release. `@miden-sdk/miden-sdk` 0.16.0 embeds
`miden-client` 0.16.0 and `miden-protocol` / `miden-standards` 0.16.1, which is why the
Rust pins are 0.16.1 for the protocol crates and 0.16.0 for the client crates.

Pins are exact on both lines, and the Rust and npm pins must move together: nothing
at build time verifies that the npm SDK's embedded `miden-standards` matches the Rust
pin, so the CI parity gates are what catch drift. See
[`MULTISIG_SDK.md`](./MULTISIG_SDK.md#contract-version-pinning).

**Upgrading from 0.17.0 (Miden 0.16) to 0.18.x (Miden 0.17)** is a protocol-line change.
Stored Miden account data is reset by the embedded migration listed below, accounts
must be recreated, and both the Rust SQLite store and the browser IndexedDB store
must be recreated: a store created under 0.16 does not open under 0.17. 0.18.0 also
changes SDK and server contracts:

- **It needs a node on 0.17.0.** A node rejects a client whose version carries a
  different pre-release label, so a stable client and an `rc` node (or the reverse)
  cannot talk (`accept header validation failed`). See "Public networks" under
  [Open upstream items](#open-upstream-items).
- **Deploy the server first.** Both multisig SDKs ask GUARDIAN for the canonical nonce
  (`GET /state/nonce`, gRPC `GetCanonicalNonce`) before fetching the state: Rust `sync()`
  and `sync_from_guardian()`, TypeScript `syncState()`. A failed pre-check is a sync
  error, not a fallback to the full fetch, so these SDKs fail every sync against a server
  that does not serve the endpoint. The server migration `2026-09-30-000001_state_nonce`
  adds a nullable `nonce` column next to `commitment` in `states`, which the endpoint
  reads instead of decoding the stored account. Filesystem-backed deployments need no
  step.
- **`Multisig.syncState()` returns `SyncStateResult` instead of `AccountState`**:
  `{ source: 'guardian', state }` when it fetched and reconciled GUARDIAN's state, or
  `{ source: 'local', localNonce, guardianNonce }` when GUARDIAN had nothing newer.
  Callers that used the returned state read `state` after checking `source`, or call
  `fetchState()` when they need GUARDIAN's copy either way.
- **The TypeScript multisig SDK runs only on the supplied `MidenClient`.** It no longer
  opens a second `WasmWebClient` on that client's store; every store read and write,
  chain sync and transaction execution goes through the given client.
  `createMultisigAccount`, the `build*TransactionRequest` builders and
  `executeForSummary` / `executeForSummaryAt` / `executeForSummaryAtTip` take only a
  `MidenClient`. `midenRpcEndpoint` is removed from those helpers and from
  `SignatureOptions`, and the `MidenClientSignatureOptions` and
  `MidenClientMultisigRequestOptions` types are removed; `MultisigClient` still requires
  `midenRpcEndpoint`, now only for the SDK's direct node reads. SDK calls queue on the
  application's client, show up in its `observer` and can reach its keystore callbacks,
  so `executeForSummary*` on a transaction the client can fully authorize rejects with
  `TRANSACTION_ALREADY_AUTHORIZED`.
- **`getConsumableNotes()` applies the web SDK's consumable-now rule**
  (`notes.listAvailable`), so notes the account can never consume are no longer
  returned, matching the Rust SDK's `list_consumable_notes`.
- **Browser clients need `useWorker: false`** until the web SDK fixes worker mode; see
  "Web SDK worker mode" under [Open upstream items](#open-upstream-items).
- **Proposals execute at the chain tip** (see
  [`MULTISIG_SDK.md`](./MULTISIG_SDK.md#tip-execution-and-the-bound-block)).
- **Note transport requires inclusion proofs.** A private note can be relayed only after
  the transaction that created it is committed: Rust `send_private_note_with_proof`,
  TypeScript `notes.sendPrivate({ inclusionProof })` or `notes.sendPrivateOutput`.
- **A delta that carries account code no longer means a new account**, because a code
  upgrade (`native_account::upgrade`) carries code too. GUARDIAN and both SDKs treat a
  code-carrying delta as an account creation only when the account has not executed a
  transaction yet (nonce zero), and apply it as a code upgrade otherwise.
- **The Rust SDK no longer supplies its own random generator.** `ClientBuilder::rng` is
  available only under the `testing` feature, and the client always uses an OS-seeded
  `ChaCha20Rng`.
- **Custom-proposal producers must serialize `TransactionRequest`s with a client on the
  same pin as the SDK**, because `propose_custom_transaction` / `createCustomProposal`
  and `prepare_custom_execution` / `prepareCustomExecution` take serialized request
  bytes, and their encoding changed across the 0.17 line.

**Upgrading from 0.16.x (Miden 0.15) to 0.17.0 (Miden 0.16)** is a protocol-line change:
the guarded-multisig auth component now pays the transaction fee and transaction summaries
bind the reference block, so nothing signed or stored on 0.15 verifies on 0.16. Stored Miden account data is
reset by the embedded migration listed below, accounts must be recreated, and the Rust
SDK's local `miden-client` SQLite store must be recreated (the browser IndexedDB store
migrates in place). `miden-client` 0.16.0 also raises the MSRV to 1.98.1.

**0.17.0-rc.1 to rc.3 were pre-releases on the Miden 0.16 release candidates**, published
to npm under the `rc` dist-tag. They are not supported. Every rc pinned a different
`auth_tx` procedure root than 0.17.0 (0.16.1 factored the fee payment into
`miden::standards::auth::multisig::pay_bounded_fee`), so an account created on an rc is
rejected with `UnsupportedContractVersion`, and a proposal still pending from an rc cannot
be reproduced: `TransactionRequest` serialization changed and the auth arg commitment moved.
Execute or cancel every pending proposal on the rc version, have GUARDIAN drop any that
cannot be executed, then recreate the account on 0.17.0. Recreating the account does not
clear proposals served for the old one.

**Moving past 0.18.0: TypeScript proposals are labelled with the account's next
nonce** keeps the protocol pins, stored data and server contracts, but changes a
TypeScript SDK default and two admission rules of a server that queues chained
candidates (`GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT` above 1, issue #17):

- **`create*Proposal` defaults `nonce` to the store account's nonce plus one**, the
  nonce the executed transaction will have, as the Rust SDK has since #72. Through
  0.18.0 the default was `Date.now()`. `options.nonce` still overrides it, and
  an account nonce at or above `Number.MAX_SAFE_INTEGER` makes the default throw
  rather than round. The delta and proposal nonce is GUARDIAN's storage key
  (`UNIQUE(account_id, nonce)`), the order of `/delta/since`, the history and the
  candidate queue, and the lookup key of `getDelta` and `abandonCandidate`. Rows this
  SDK wrote before the change keep their timestamp keys, nothing is migrated: on such
  an account a timestamp sorts after every account nonce, so `/delta/since` (ascending)
  lists those rows last and the history (newest-first) lists them first, and neither
  order is chain order across the switch; `/delta/since?nonce=N` from a nonce-keyed
  cursor returns the timestamp-keyed rows every time.
- **Mixed-version cosigners.** A cosigner still on 0.18.0 or earlier labels with a
  timestamp. A server at the default depth accepts that proposal as before; a queueing
  server refuses it with `409 conflict_pending_delta` while a candidate is queued (next
  item), and refuses a delta labelled with anything but the nonce it leaves the account
  at while a candidate is queued, so a timestamp label cannot take the queue's tail. With
  nothing queued, a timestamp-labelled delta is still accepted, and the queue then holds
  only that candidate until it promotes, because no real nonce exceeds its label. Upgrade
  the proposing devices first; signing and executing a proposal another device created is
  unchanged, and a proposal GUARDIAN already holds under a timestamp key stays executable
  from the device that holds the state it is pinned to.
- **A queueing server records a proposal behind a queued candidate only at that
  candidate's nonce plus one**, and **admits nothing behind a candidate that changes the
  account's signer set or guardian key** until it promotes, both `409
  conflict_pending_delta`. At the default depth of one nothing changes: a queued
  candidate already refuses every submission. The queue helps the device that pushed
  the newest candidate; `/state` serves the canonical state, so every other cosigner
  is refused until it drains, and serving the queue tail to cosigners is follow-up
  work.

A Guardian server or SDK built on one protocol line rejects a node from another.
Run a node matching the **Miden protocol** column.

## Data resets

Guardian has three times been unable to migrate stored account data across a Miden
line. Each reset is an embedded migration that runs automatically at server
startup, each is irreversible, and each scopes the purge to Miden rows using
`account_metadata.network_config->>'kind'` so EVM accounts survive.

| Migration | Introduced in | Deletes | Preserves |
|---|---|---|---|
| `2026-09-22-000001_miden_017_irreversible_reset` | Guardian 0.18.x | Miden rows in `delta_proposals`, `deltas`, `states`, `account_metadata`; `account_auth_state` by cascade | EVM rows, `admin_actions`, `auth_sessions`, `auth_challenges`, `storage_encryption_marker`, `worker_leases`, dashboard stats snapshot, keystore |
| `2026-08-24-000001_miden_016_irreversible_reset` | Guardian 0.17.x | Miden rows in `delta_proposals`, `deltas`, `states`, `account_metadata`; `account_auth_state` by cascade | EVM rows, `admin_actions`, `auth_sessions`, `auth_challenges`, `storage_encryption_marker`, `worker_leases`, keystore |
| `2026-06-14-000001_v015_account_id_cutover` | Guardian 0.15.x | pre-0.15 (v0 account ID) Miden rows in the same four tables | EVM rows, `admin_actions` |

All three are Postgres-only. Filesystem-backed deployments reset by starting from
empty storage and metadata directories, preserving the keystore directory.

A deployment upgrading across more than one line runs every pending reset in the
same startup; the newer reset subsumes the older ones.

## Guardian 0.18.x on Miden 0.17

Nothing stored under Miden 0.16 survives:

- **Serialized accounts, headers, and asset ids carry a version.** `Account`
  decoding rejects the 0.16 encoding (`account version is 241 but only version 1
  is supported`).
- **Account code procedures are ordered with the authentication procedure
  first**, so the code commitment of an account built from the same components
  changes.
- **Delta and storage-patch commitments are versioned**, and their domain
  separators moved into the hasher capacity word, so a stored delta no longer
  recomputes to the commitment it was signed under.
- **The transaction summary is versioned.** It binds a caller-chosen block and
  carries six user params instead of seven: the approval-expiration block (zero
  when the approval never expires), a zero, then the four salt felts. Stored
  summaries cannot be deserialized or re-verified.
- **The multisig auth argument is the commitment to a three-word preimage**:
  `[bound_block, approval_expiration, 0, 0]`, the salt, and the native 1/1 fee
  conversion info. Both SDKs set that preimage on the request. Rust builds
  `MultisigAuthArgs`; TypeScript starts from `feeAwareTransactionRequestBuilder`.
  A 0.16 request that only declared `fee_conversion_salt` is one word short and
  aborts in the auth procedure.
- **The guarded-multisig auth procedure pays the transaction fee.** It creates the `TX_FEE` note from the
  account's vault before the transaction summary is built, so the fee note is
  covered by the approver and GUARDIAN signatures. Every transaction therefore
  needs a balance in the chain's native fee asset, including the first one
  that deploys the account. A node rejects a transaction without a canonical
  `TX_FEE` note.
- **The fee asset left the block header.** It lives in the chain's protocol
  configuration, which the client receives from the node with each sync and
  stores per header. The auth args name the fee faucet of the configuration at
  the client's sync height, the one an execution at the tip loads, read from
  that store.
- **P2ID note storage is four felts** (target account, then a two-felt salt that
  defaults to zero) and **P2IDE storage is six** (reclaimer, target, reclaim
  height, timelock height). The script roots moved with the layouts.
- **Execution proofs come from VM 0.35** (Plonky3 0.8). A 0.16 proof is rejected.

### Guardian execution

Guardian 0.18.x adds server-side execution of Guardian-executable proposals on this line. A
stored request declaring a protocol line other than `0.17` is refused before it is decoded.
Within the line, `TransactionRequest` serialization carries no version tag and the 0.17
release candidates changed it, so the SDK that created a proposal and the server should run the
same `miden-client`: this build pins the stable 0.17.0 client in the server and the Rust SDK and
web SDK 0.17.0 in the TypeScript SDK. A request from a different client either fails to decode
(`GUARDIAN_EXECUTION_REQUEST_CODEC`) or reproduces a transaction other than the signed one
(`GUARDIAN_EXECUTION_BINDING_MISMATCH`); either way nothing is submitted. The live
scenarios (`live-guardian-execute-*`) passed by hand on devnet (node 0.17.0) on 2026-10-06 with
these stable pins, on both SDKs; the qualification matrix requires them on testnet and excludes
them from devnet runs, whose step budget they exceed.

Source-level changes in the Rust SDK (`miden-multisig-client`) for integrators upgrading:

- `ProposalPayload` has a new public field, `transaction_request`, so a struct literal must set
  it (`None` for a self-executed proposal) or start from `ProposalPayload::new` and
  `with_transaction_request`.
- `MultisigError` has two new variants, `GuardianExecutionRefused { code, message, retryable,
  retry_after, blocking_proposal_id }` and `GuardianExecutionWaitTimedOut { proposal_id,
  deadline, last_observed }` (from `wait_for_guardian_execution`), so an exhaustive `match` on
  the error needs an arm for each.
- `guardian_shared::execution::ExecutionFailureCode` includes
  `AcknowledgementFailed` (`GUARDIAN_EXECUTION_ACKNOWLEDGEMENT_FAILED`): Guardian could not sign
  or record its acknowledgement, a failure on Guardian's side that leaves the proposal
  retryable.

Source-level changes in the TypeScript packages:

- `GuardianErrorCode` (`@openzeppelin/guardian-client`) has new members, so an exhaustive
  `switch` over it stops compiling until it handles them: `account_request_capacity_exceeded`,
  `execution_busy`, `execution_conflict`, `execution_not_found`,
  `proposal_executes_locally`, `proposal_missing_transaction_request`, `proposal_not_ready`,
  `proposal_request_too_large` and `proving_unavailable`.
- `StatusResponse.execution` is a new required field (`{ enabled: true }` or
  `{ enabled: false, reason }`), so code that builds a `StatusResponse` (a mock, for example)
  must set it.
- `Multisig.createProposal` (`@openzeppelin/miden-multisig-client`) throws on a client created
  with `executionMode: 'guardian_executable'`, because it takes a summary without the request
  Guardian would execute. Use a typed `create*Proposal` method or `createCustomProposal` there.
- The multisig execution methods (`requestGuardianExecution`, `executionStatus`,
  `currentExecution`) throw `GuardianExecutionRefusedError`, whose `code` is the wire code (for
  example `GUARDIAN_EXECUTION_CONFLICT`), instead of the base client's `GuardianHttpError`, which
  stays available as `cause`. `waitForGuardianExecution` throws
  `GuardianExecutionWaitTimeoutError` when its deadline passes.

Server behavior that changes for every client:

- Pushing a proposal that already exists answers with the proposal as stored, not an echo of
  the push, and is refused as an invalid delta when its `transaction_request` differs from the
  stored one: the stored request is the one Guardian would execute.

#### Upgrading and rolling back

- **Migration.** `2026-10-01-000001_execution_reservations` runs at startup. It is additive,
  not a data reset: it creates `execution_reservations`, `execution_submissions` and
  `execution_outcomes`, and adds `delta_proposals.request_bytes` (default `0`). Stored accounts,
  deltas and proposals are kept.
- **Enable in two deploys.** A replica from an earlier release still canonicalizes during a
  rolling update, but it promotes a Guardian-executed candidate without releasing its execution
  reservation and skips the `ProtectedByExecution` gate that stops a candidate under a live
  execution from being discarded. Either can leave the account reserved with nothing able to
  release it, so no further execution starts on that account. Once every replica runs this
  release, reconciliation settles a reservation an older replica promoted, but nothing undoes a
  candidate an older replica discarded. Finish rolling out this release with
  `GUARDIAN_TX_PROVER_URL` unset, then set it in a second deploy once no old replica is
  running. Proposals stored by old replicas during the overlap have `request_bytes = 0`
  whatever request they carry, so they do not count toward `GUARDIAN_MAX_ACCOUNT_REQUEST_BYTES`.
- **Rolling back.** Set `GUARDIAN_PROVING_ENABLED=false` first and wait until no execution is
  active: `SELECT count(*) FROM execution_reservations WHERE released_at IS NULL` returns `0`,
  or `guardian_execution_oldest_reservation_age_seconds` reads `0`. Reconciliation keeps
  settling in-flight executions while execution is switched off. Only then roll back, for the same
  reason as above: an older replica cannot release a reservation.

### Open upstream items

The facts below change independently of this repository. This list is the one
place that tracks them; other documents point here rather than restating them.
Last checked 2026-10-06.

- **Public networks.** Devnet runs node 0.17.0, which this build's pins reach:
  live qualification, including the `live-guardian-execute-*` scenarios, passed
  there on both SDKs on 2026-10-06. The manual devnet run described next is a
  separate, earlier run on the Miden 0.17 release candidates and has not been
  repeated on 0.17.0. There, this build's protocol
  configuration for devnet's fee asset hashed to the commitment in devnet's
  block headers, so the transaction kernels matched. A guarded 2-of-2 multisig
  ran two proposals there end to end, a first consume-notes
  transaction and a P2ID send. Each was verified, signed and executed at the tip
  more than 70 blocks after the block it binds, once devnet already answered
  `block N has been pruned` for that block's account state. Devnet keeps about
  50 blocks of account history, and its fee faucet's account ID enables asset
  callbacks, so every fee payment loads the faucet as a foreign account.
  Devnet has no faucet front end: an account is funded by calling the node's
  `RegisterAccount` RPC, which pays it a small public P2ID note in the fee asset
  (`scripts/devnet-register-account.sh`). `miden-client`'s `register_account`
  does not send the request on devnet, because devnet enforces no allowlist and
  reports every account as already allowed. Testnet runs Miden 0.16, so
  on testnet the examples need a local `miden-node` from the pinned line until
  it upgrades.
- **miden-client fee path.** miden-client (still in 0.17.0) commits the
  two-word 0.16 auth arg when a request declares `fee_conversion_salt`, so both
  SDKs set the three-word auth arg themselves (rationale in the multisig
  client's `transaction/auth_args.rs`). When the client builds
  `MultisigAuthArgs` itself: the helper can delegate to it; nothing stored or
  signed changes.
- **Web SDK worker mode.** A browser `MidenClient` created with the default
  `useWorker: true` is two WASM instances, each with its own in-memory copy of
  the account's storage trees: `accounts.insert` runs on the page, while
  transactions execute and apply in a Web Worker. The multisig SDK writes the
  state it loads or syncs from GUARDIAN with `accounts.insert`, so the worker
  cannot apply a device's first transaction after `MultisigClient.load`, and it
  applies the first one after a `syncState()` import on stale trees, saving a
  storage root that leaves the import out. The transactions still reach the
  chain; only the device's store breaks. Tracked as
  [0xMiden/web-sdk#441](https://github.com/0xMiden/web-sdk/issues/441), which
  was closed on 2026-09-30 without a web SDK fix. Web SDK 0.17.0 changes how
  clients sharing a database refresh account witnesses (web-sdk#453), but worker
  mode has not been re-verified on it. Until it is, browser clients pass
  `useWorker: false`, as `examples/web` and `examples/smoke-web` do; symptoms
  and recovery are in
  [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md#account-data-wasnt-found-or-incomplete-storage-map-in-a-browser).
  When the web SDK fixes it: remove that guidance (`git grep useWorker`).
- **Pins.** The workspace pins the stable 0.17.0 protocol and client crates and
  web SDK 0.17.0 (see the matrix). The protocol pin follows the client and web
  SDK releases, not the protocol tags, because both SDKs must embed the same
  kernel.

Data effect: full reset, see above. Client stores are recreated, not migrated.
Operator steps: [`PRODUCTION.md`](./PRODUCTION.md#upgrading-to-miden-017).

### Before bumping the Miden pin

A deployed Miden account is immutable and its procedure roots fix at creation, so
moving the contract pin strands every account created under the previous one,
including each network's qualification treasury. Nothing catches this
automatically: the qualification suite deliberately carries no long-lived
account, because GUARDIAN holds the only full copy of a private account and the
suite's stack is torn down with every run (see "Current limits" in
[QUALIFICATION.md](./QUALIFICATION.md)).

So a pin bump owes these by hand, in this order:

1. Qualify an account created **before** the bump against a server built
   **after** it, and confirm it fails loudly rather than silently misbehaving.
   A run whose accounts are all created after the bump proves nothing about it.
2. Recreate each network's treasury with `treasury-new`, fund it, and record the
   new key. The old treasury is stranded like any other account.
3. Note the bump in the support matrix above, with whether existing accounts
   survive.

Step 1 is the one most easily skipped, because every other signal stays green.

## Guardian 0.17.x on Miden 0.16

Nothing stored under Miden 0.15 survives, because the account's on-chain surface
moved in several independent ways:

- **Procedure roots changed**, so stored proposals no longer address the
  procedures they were signed against, and root-keyed storage reads
  (`procedure_thresholds`) miss.
- **ECDSA-k256 public-key commitments changed** in `miden-crypto` 0.28 to hash
  native affine-coordinate limbs (`qx || qy` as little-endian `u32` limbs)
  instead of the compressed SEC1 bytes, so stored approver commitments no longer
  match their keys. Compressed SEC1 *serialization* is unchanged, which is why
  this fails as a commitment mismatch rather than a decode error.
- **The signature advice ABI changed** in `miden-vm` 0.29 to
  `QX[8] || QY[8] || SIG_R[8] || SIG_S[8]`, and the recovery byte is no longer
  part of it, so stored signatures cannot be replayed into a transaction.
- **Storage slot names moved** from `openzeppelin::*` to `miden::standards::*`,
  so stored state cannot be read back by name.
- **The transaction summary layout changed** and now binds a chain anchor, so
  stored summaries cannot be recomputed or re-verified. Proposals carry a
  serialized `ChainAnchor` (wire field `chain_anchor`) and verification and
  execution pin to it.
- **The custody account is now the upstream `miden-standards`
  `AuthGuardedMultisig` component** rather than Guardian's local MASM, and
  `guardianEnabled` is gone: the guardian is always present.
- **Transaction fees became the auth component's responsibility**, and
  `AuthGuardedMultisig` now pays them. Its auth procedure calls
  `miden::standards::fee::pay_fee` *before* building the transaction summary, so
  the fee note and the vault withdrawal funding it fall inside what the cosigners
  sign rather than being appended afterwards.

  That makes the auth arg carry double duty. `fee::load_conversion_info` reads it
  as the commitment `hash(CONVERSION_INFO || SALT)` and looks the preimage up in
  the advice map; the same word then serves as the transaction summary salt. A
  bare salt still satisfies the salt role but not the fee role: the lookup
  misses, conversion info comes back empty, and `pay_fee` aborts with
  `ERR_FEE_CONVERSION_INFO_MISSING` — though only once the computed fee is
  non-zero, so a zero-`verification_base_fee` chain never notices.

  **Every typed `create*Proposal` path in both SDKs therefore commits native
  conversion info**, at rate 1/1 under the chain's own fee faucet. This is the
  invariant change: a proposal's auth arg is no longer its salt.

  Cross-SDK reconstruction survives because the committed value is *derived*, not
  chosen. The faucet is read from the block the proposal is anchored at — the
  anchor travels with the proposal and is checked against the summary's block
  commitment before use — and the rate is fixed at 1/1. Both SDKs declare the
  stored proposal salt on their transaction request builders:
  `fee_conversion_salt(salt)` in Rust and `withFeeConversionSalt(salt)` in
  TypeScript. The pinned Miden clients then derive and commit the same native
  conversion info from the reference header used for execution.

  Two consequences worth knowing:

  - The pinned Miden clients classify `AuthGuardedMultisig` as
    `CallerChosenSalt`. A request declares a salt, and the client commits the
    chain-native conversion info under that salt. Components that do not read
    fee conversion info still fail with
    `TransactionRequestError::FeeConversionInfoUnsupported`.
  - `pay_fee` spends the faucet and rate the committed conversion info names, so
    what a guarded account must hold follows from what it commits. The built-in
    typed proposal paths always commit the chain-native asset at rate 1/1, so on
    a fee-charging chain an account driving them needs that native asset in its
    vault or `pay_fee` aborts before the summary exists — and guardian-assisted
    recovery cannot route around it, since it takes the same path. A custom
    request that commits a different fee asset must instead fund *that* asset:
    holding only it is enough to execute through fee payment, provided the
    request needs no other assets. Whether the resulting transaction is then
    *included* is a separate question — the batch builder decides what fee
    asset and rate it accepts.

  The exported builders always declare the fee conversion salt. A caller
  assembling a raw custom request can omit it, but the resulting request works
  only on a zero-fee chain. Typed proposal reconstruction always declares the
  stored `salt_hex`.

Data effect: full reset, see above. Operator steps:
[`PRODUCTION.md`](./PRODUCTION.md#upgrading-to-miden-016).

## Guardian 0.15.x and 0.16.x on Miden 0.15

Miden 0.15 invalidated account ID version 0: encoded version `0` is rejected, and
every serialized `AccountDelta` or `TransactionSummary` embedding a v0 ID fails to
deserialize. A v0 ID is a proof-of-work-derived commitment with no v1 equivalent,
so there is no in-place migration. Addresses also moved to bech32m.

Guardian 0.16.x stayed on Miden 0.15 and required no reset; the changes in that
release were Guardian-side only.

Data effect: the 0.15 cutover above, on the first 0.15 deploy.

## Adding a line

When Guardian adopts a new Miden line:

1. Add a matrix row with the exact pins.
2. Add a per-line section stating what broke and what it does to stored data.
3. If data cannot be migrated, add the migration to the reset table and write the
   operator steps in [`PRODUCTION.md`](./PRODUCTION.md).
4. Leave the procedural and symptom docs pointing here rather than restating the
   version facts, so there is one place to update.
