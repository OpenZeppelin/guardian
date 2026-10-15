# Research: Guardian Modules

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md) | **Design source**: [RFC 0002](../../../docs/rfcs/0002-guardian-modules.md) revision 19

This plan is written before the prototype on `251-modules-prototype`. Decisions the RFC settled through nineteen revisions are recorded as decisions. Questions only running code can answer are recorded as **open items with a prototype gate**: each states what the prototype must show, the default the plan assumes until then, and what changes if the default fails. `tasks.md` is generated only after every gate is closed or explicitly accepted.

## Prototype gates (open)

### G1. Account view extraction effort

- **Question**: How much new decoding does `AccountView` (signers, default threshold, per-procedure thresholds, scheme) need, and does it fail cleanly on unsupported auth state?
- **Known**: The existing `account_auth_binding` (`crates/server/src/network/mod.rs:402`) returns `AuthBinding { signers, guardian }` with no threshold or scheme (`:247-250`). The Miden implementation returns an empty signer list as success (`network/miden/mod.rs:802-805`). Slots involved: `miden::standards::auth::multisig::approver_public_keys`, `threshold_config`, `procedure_thresholds`, `approver_schemes`. `configure_account` reads only metadata for existing accounts (`configure_account.rs:58`).
- **Default assumed**: one new `NetworkClient` method, `account_view(state_json) -> Result<AccountView, ViewError>`, decoding the four slots from one account decode, with `ViewError::Unsupported` and `ViewError::Malformed`; re-onboarding adds one `pull_state`.
- **Prototype must show**: the method on real single-key, multisig and guardian-multisig accounts; its error on an unsupported layout; the decode cost per call.
- **If the default fails**: drop `procedure_thresholds` from protocol version 1 and document threshold floors as default-threshold-only, or make the view lazy per module (`describe` declares whether it needs views).

### G2. `TxProjection` reuse of the dashboard projector

- **Question**: Can `TxProjection` be built from `decode_full` (`crates/server/src/delta_summary/projection.rs:88`), which already yields input notes, output notes, vault changes and storage changes?
- **Known gaps against the proto**: `nonce_delta`, `expiration_block`, `OutputNote.partial`, `recipient_digest`, `script_root`, `p2id_target` (the projector classifies tags and parties instead), and the storage create/update/remove operation per slot. Dashboard types serve a UI and change with it.
- **Default assumed**: a separate `module_projection` builder in the server that shares the low-level helpers (`project_vault_changes`, `project_storage_changes`, note decoding) and owns its own output types mapped one to one onto the proto, so dashboard changes cannot move the module contract.
- **Prototype must show**: the builder producing every proto field for the transaction kinds in the qualification suite (P2ID, P2IDE, consume notes, signer and threshold changes, procedure threshold update, switch guardian).
- **If the default fails**: fields that cannot be derived are removed from protocol version 1 before the freeze, and modules needing them decode `raw_tx_summary`.

### G3. Added latency

- **Question**: What does the pipeline add to `push_delta` and proposal creation?
- **Default assumed**: SC-001 (no change with no modules) and SC-006 (at most 10 ms p99 for one in-process module); default budget 1,000 ms and per-instance maximum 1,000 ms as in the RFC.
- **Prototype must show**: p50 and p99 of `push_delta` and `push_delta_proposal` locally with zero modules, one in-process `basic-policy`, and one remote module over loopback gRPC with TLS; the share spent building views and projections.
- **If the default fails**: build projections and views once per request and only when at least one module will be called (already required), cache the `before` view per tail commitment, and revise SC-006 and the default budget in the RFC.

### G4. Module author ergonomics

- **Question**: Is the `Module` trait and request shape pleasant to write against, in process and remotely?
- **Default assumed**: the trait in RFC section 2.1 with `async_trait` (object safety for `Arc<dyn Module>`), protocol types generated from the proto and re-exported by `guardian-module-sdk`, and a `serve` harness built on tonic.
- **Prototype must show**: `basic-policy` (amount cap, recipient and asset allowlists, signer allowlist with `signer_check`) written once and served both ways with identical decisions; one toy remote module written only against the SDK; the signer rule needing no per-hook branching.
- **If the default fails**: adjust the trait (for example a typed request wrapper with helpers such as `added_signers()`) before the freeze.

### G5. Pipeline placement on `main` versus PR #510

- **Question**: Does the pipeline drop into the ack function cleanly on `main` and after PR #510 merges?
- **Known**: on `main` the only production ack is `push_delta` before `state.ack.ack_delta` (`services/push_delta.rs:182`); PR #510 routes it and the execution worker through `ack_delta_internal::acknowledge_delta`. `ExecutionJob` has no requester (`execute_proposal/worker.rs:27-40` at `29c722b4`).
- **Default assumed**: the prototype targets `main`; the M2a implementation starts after #510 merges and places the pipeline in `acknowledge_delta`.
- **Prototype must show**: the pipeline at `push_delta.rs:182` and in `push_delta_proposal` before `submit_delta_proposal`, with no change to other flows.
- **If the default fails**: an explicit ordering decision between #510 and M2a goes into the plan.

## Decisions (settled by the RFC)

### D1. A separate published SDK crate

- **Decision**: new workspace crate `crates/guardian-module-sdk` holding the `Module` and `ModuleFactory` traits, protocol types generated from `guardian.module.v1`, the `serve` harness, the `StatefulModule` watermark helper (M2b) and the conformance suite. The server depends on it.
- **Rationale**: module authors must not depend on `guardian-server` or its Miden pins (RFC 2.5); one proto is the source of truth for every transport.
- **Alternatives**: types inside `guardian-server` (forces authors onto the server's dependency tree); hand-written types per transport (drift).

### D2. Proto build

- **Decision**: compile `guardian_module_v1.proto` with `tonic-prost-build`, as the server already does for `guardian.proto` (`crates/server/Cargo.toml` build dependencies), inside the SDK crate.
- **Rationale**: same toolchain and versions as the existing gRPC surface.

### D3. Configuration and runtime state

- **Decision**: the catalog is a TOML file at `GUARDIAN_MODULES_CONFIG`, parsed and validated in `ServerBuilder::build()`. Runtime state that every replica must see (per-account assignments, instance enable flags) lives in the metadata store on both backends; Postgres gets a migration. Delivery and resync state (M2b) live only on Postgres with the outbox (D8).
- **Rationale**: RFC 2.2 (no hot reload, runtime changes through shared state). Filesystem keeps parity for gate hooks.
- **Alternatives**: hot reload, runtime registration, cloud configuration services (RFC section 8).

### D4. Errors

- **Decision**: two `GuardianError` variants mapped to `GUARDIAN_MODULE_DENIED` (403 / `PERMISSION_DENIED`, `meta {module, reason_code}`) and `GUARDIAN_MODULE_UNAVAILABLE` (503 / `UNAVAILABLE`, `meta {module}`) through the existing `{code, message, meta}` envelope, on HTTP and gRPC, then typed in `crates/client`, `packages/guardian-client` and both multisig SDKs.
- **Rationale**: constitution principles II and IV; the human-readable error envelope already exists.

### D5. Operator API and audit

- **Decision**: module endpoints under `/dashboard/*` with the existing session authentication; `policies:write` (already reserved in `dashboard/permissions.rs`) for writes, a new `modules:disable` for disable only, `dashboard:read` for reads; each write recorded through the existing audit facility with operator identity and reason. `packages/guardian-operator-client` updated in the same change.
- **Rationale**: RFC 2.2; AGENTS.md section 8 for dashboard changes.

### D6. Failure handling is per replica and in memory

- **Decision**: the circuit breaker and health probe are per replica in memory; the evaluation budget is a per-request deadline passed down the pipeline.
- **Rationale**: RFC 4.3 and 4.4; nothing about breaker state needs to survive a restart.

### D7. Proposal storage: first creator wins

- **Decision**: the filesystem backend's `submit_delta_proposal` becomes create-if-absent, matching Postgres `on_conflict(..).do_nothing()` (`storage/postgres.rs:1400-1404`). Its `update_delta_proposal` currently re-submits (`storage/filesystem.rs:767-768`), so it gets its own replace-if-present implementation; otherwise every cosigner signature would become a silent no-op. Proposal lookup for policy returns `Result<Option<ProposalContext>>`.
- **Tests**: duplicate creation keeps the first proposer on both backends; create, sign, reload on the filesystem backend shows the signature.
- **Rationale**: constitution invariant on backend parity; RFC 3.2.

### D8. Notify delivery (M2b)

- **Decision**: `module_events` outbox rows written in the same storage write that commits the delta (`promote_candidate` and the optimistic commit), a notifier on the existing coordination leadership lease (`coordination/leader.rs`) with fenced cursor writes, per (instance, account) cursors, retention and `resync_required`.
- **Rationale**: RFC 3.3 and 3.4. Postgres only; the filesystem backend refuses a catalog that subscribes to `after_transaction_committed`, an accepted backend-specific limitation (see plan, Constitution Check).
- **Activation**: once any instance subscribes, module data (projection, `signed_at`, `submitter`, `proposer`) is stored with every signed delta for every account, so later assignment needs no backfill. Candidates queued without it (signed before M2b or by a replica on an older catalog) put subscribed modules for that account behind an activation fence: evaluation counts as an error and `on_error` applies until those candidates leave the queue. Backfilling was rejected because `submitter` cannot be recovered and a reconstructed accounting time could move a transaction between windows.

### D9. Execution requester (after PR #510)

- **Decision**: the authenticated requester is stored on the durable execution reservation when an attempt is created, kept on repeated requests for an active attempt, and carried into `acknowledge_delta` and the stored delta.
- **Rationale**: spec FR-033; restart safety.

### D10. Delivery order

- **Decision**: M2a, M2b and M2c ship in order, each behind no feature flag beyond configuration: a server with no `GUARDIAN_MODULES_CONFIG` runs no pipeline.
- **Rationale**: spec FR-006 and SC-001; protocol version 1 published complete at M2a (RFC section 7).

## Prototype findings (2026-10-13)

Built on `251-guardian-modules-rfc` (local only, not pushed): `crates/guardian-module-sdk` (traits, protocol types, `serve` harness, `basic-policy`, conformance tests), `crates/server/src/modules/` (catalog, registry, `grpc` kind, request assembly, pipeline), the three gate hooks in `push_delta`, `push_delta_proposal` and `configure_account`, module views in `crates/server/src/network/miden/module_views.rs`, typed errors in `crates/client` and `crates/miden-multisig-client`. Server suite: 1,167 passed, 1 timing test failed under load and passes alone (known flake).

**Not run**: the quickstart walk-through (T033) and the end-to-end `push_delta` A/B (second half of T036). Both need a running Miden node; none was available locally. They move to M2a validation.

### G1 closed, with RFC changes

- Extraction reads `threshold_config` as `[threshold, num_approvers, 0, 0]`, the approver map at indices `0..num_approvers` (a missing key is an error, never a shorter list), and every entry of `procedure_thresholds`. An account without the multisig component is an error. Tests: fixture account, a built 2-of-3 account with a procedure threshold, a missing approver key, a non-multisig account.
- **Scheme is not decoded from state.** Schemes are stored per approver (`approver_schemes`), so mixed-scheme accounts exist. The prototype takes `auth_scheme` from the account's verified auth configuration. RFC revision 20 should say so, or replace the field with a per-signer scheme list.
- **Single-key accounts have no multisig component**, so every gate request for them fails extraction and `on_error` applies. Revision 20 must decide: build the view from the registered single signer (no thresholds), or document that signer and threshold rules do not apply to single-key accounts.
- Cost (release): 1.1 ms per view, almost all of it `Account::from_json`. Two views per gate request, so about 2.2 ms; proposal creation also runs one extra `apply_delta` to obtain `after`. Reusing the accounts the push path already decoded would remove most of it; not required for SC-006.
- `procedure_thresholds` stays in protocol version 1.

### G2 closed

- Every `TxProjection` field derives from `TransactionSummary`: `nonce_delta` from the account delta, `expiration_block = block_number + expiration_delta`, vault changes per added and removed asset (not netted), output notes with `partial`, `recipient_digest`, `script_root` and `p2id_target`, storage changes with their create, update or remove operation from the patch.
- The builder is written directly against Miden types; only the P2ID and P2IDE target lookup duplicates the dashboard projector. The dashboard helpers are private and emit UI types, which confirms the separate builder. Cost: about 2 µs per projection (release).
- **`raw_tx_summary`**: the prototype sends the JSON payload bytes. Revision 20 should fix the encoding (Miden binary serialization is the stable choice).
- **`RegistrationSubject.initial_state`** was not built; registration rules in the prototype use only the account views.
- Coverage: the three delta fixtures decode, wrapped or bare. Per-kind fixtures (P2IDE, consume notes, switch guardian, procedure threshold update) are not in the repository; generating them is an M2a task.

### G3 closed at component level

- Remote module over loopback gRPC: p50 43 µs, p99 102 µs per evaluation. In-process `basic-policy`: 0.2 µs.
- Estimated added latency for a gated push: about 2.3 ms plus the module round trips, within SC-006 (10 ms). The default budget of 1,000 ms is unchanged.
- SC-001 holds by construction: with no catalog the pipeline is one `Option` check, and with a catalog but no module for the account and hook no request is built. The full server suite passes unchanged.
- Open for M2a: the end-to-end p50 and p99 A/B against a local node.

### G4 closed, with SDK changes

- The trait in RFC section 2.1 works as written. `notify` gets a default no-op like `notify_gap`, so a stateless module implements three methods.
- The SDK needs small helpers the RFC does not list: `Decision::allow`/`deny`, `ParamsVerdict::ok`/`invalid`, `is_valid_reason_code`, and a `ModuleInstanceConfig` type for factories. Recommended for M2a: accessors such as `Asset::fungible_amount()`; the generated `oneof` is verbose.
- The account views removed per-hook branching: the signer rule is one function used at all three hooks.
- `serve(..).instance(id, token)` takes the token value, not a file path as the RFC example shows; the example binary reads the file.
- The RFC requires TLS for remote modules but defines no catalog fields for CA or client certificates; the prototype maps `grpc://` to plaintext. Revision 20 needs the TLS fields.

### G5 closed

- On `main` the approval hook sits in `push_delta` after the proposal lookup and before `ack_delta`; no other flow changed. Every input it needs (tail state, applied state, authenticated signer) also exists in `acknowledge_delta` on PR #510, so the move is mechanical; the execution requester (D9) is the only new data.
- The registration hook runs before the replay timestamp is consumed, so a denied reconfiguration consumes nothing. Revision 20 should state it.
- A failed `apply_delta` at proposal creation is an invalid delta (client fault), not a module failure.

### Other findings

- Filesystem proposal creation uses a hard link, which fails when the file exists, for an atomic create-if-absent; update is replace-if-present. Postgres update semantics on a missing proposal still need checking.
- The multisig SDK used to flatten every Guardian error into a string. It now has `ModuleDenied` and `ModuleUnavailable`; the TypeScript SDK needs the same for parity (constitution principle II).
- Token files in the catalog resolve relative to the catalog file.

### Found in the live run (2026-10-15)

- **The fee is an output note.** Miden 0.17 `pay_fee` creates a public `TX_FEE` note funded from the vault (`miden-standards` `account/auth/singlesig.rs:45`). The prototype counted it as a payment, so a cap on the fee asset refused every transaction (a consume-notes proposal on devnet was refused at creation), and a recipient allowlist would refuse every transaction too. Fixed: `OutputNote.tx_fee` marks the fee note, and `basic-policy` excludes it from the cap, the recipient allowlist and the asset allowlist; the asset allowlist now covers assets sent in payment notes and received in input notes, since vault changes cannot tell a fee from a payment.
- **Review of the prototype (2026-10-15).** Fixed: bearer tokens held as `SecretString` (redacted, zeroized) in the catalog and the SDK factory config; a disabled instance whose kind is unknown or whose endpoint is malformed no longer blocks startup; the expiration height uses checked addition and an overflow is a module failure, not a panic; the asset allowlist covers consumed notes. Open for M2a: request assembly is not yet inside the reconstruction bound (revision 20 item 8).
- **The multisig SDK flattened refusals in its call sites.** Proposal push, delta push and account configuration wrapped Guardian errors into strings before the typed conversion ran, so the demo showed the generic message without the module or reason code. Those ten sites now keep `ModuleDenied` and `ModuleUnavailable`.

### Changes for RFC revision 20

All applied in RFC 0002 revision 20 (2026-10-15). The prototype still differs from it on four points, all M2a work: single-key accounts fail view extraction instead of getting a view from the registered signer (item 2); `raw_tx_summary` carries the JSON payload, not the binary serialization (item 3); there is no `tls` catalog field and `grpc://` is plaintext (item 4); request assembly runs on the request task (item 8).


1. `AccountView.auth_scheme` source (or per-signer schemes).
2. Single-key accounts and account views.
3. `raw_tx_summary` encoding.
4. TLS fields for remote kinds.
5. `notify` default no-op; SDK helper list; `serve` token argument.
6. Registration hook runs before the replay timestamp is consumed.
7. `OutputNote.tx_fee`, and the fee rule stated in terms of it.
8. Request assembly runs inside the bounded reconstruction task. The prototype decodes the account views and projection inline on the request task: an error already becomes a module failure under `on_error`, but a slow or heavy decode is not bounded the way `apply_delta` is (`network::reconstructor()`). M2a moves the view and projection building into the same CPU-bounded task, and the RFC states that request assembly shares the reconstruction bound (section 4.3).
