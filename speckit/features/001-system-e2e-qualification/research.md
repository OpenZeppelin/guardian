# Phase 0 Research: Miden system qualification

**Feature**: `001-system-e2e-qualification` · **Spec**: [spec.md](./spec.md) · **Date**: 2026-09-15

Five parallel investigations against the repository and, where stated, against
the live public networks. Findings that contradicted the spec were fed back
into it before this document was written; the corrections are listed in
[checklists/requirements.md](./checklists/requirements.md) passes 7 and 8.

---

## D1. Harness language and shape

**Decision**: Two drivers, one per SDK, sharing a declarative scenario manifest
and a common result schema. Rust driver as a new non-interactive binary.
TypeScript driver as a vitest suite reusing the package's existing WASM setup.

**Rationale**: The Rust SDK is already fully headless. Every flow the spec
requires has a public async method on `MultisigClient`
(`crates/miden-multisig-client/src/client/`), no terminal dependency anywhere
in the crate. No SDK change is needed to build the driver.

`examples/demo` cannot be scripted. `rustyline` prompts are threaded through
every action signature (`examples/demo/src/main.rs:184-205`,
`examples/demo/src/menu.rs:68-73`), startup configuration is prompted before a
client exists, and prompt sequences branch on indices printed by prior output.
The repository's own skill documents it as a manual, three-terminal surface.

For TypeScript, reusing `packages/miden-multisig-client/vitest.config.ts` and
`tests/setup-wasm.ts` is the shortest supported path, because that plumbing
already solves the module-resolution problem below.

**Alternatives considered**: A single polyglot driver shelling out to both
(rejected: loses typed results and doubles the serialization surface). Driving
`examples/smoke-web` through Playwright for the TypeScript side (rejected for
the broad matrix: far slower, and it is a React harness exposing a JSON API on
`window`, not a library entry point). Extending `examples/demo` with a
non-interactive mode (rejected: it would grow a second control path through a
1655-line interactive module).

**Reuse**: `benchmarks/prod-server` is the structural precedent worth copying,
being the only non-interactive, profile-driven, artifact-producing harness in
the repo. The demo's retry and reinitialize-on-store-error logic
(`examples/demo/src/actions/sync_account.rs:41-137`) should be ported rather
than reinvented.

---

## D2. TypeScript runtime

**Decision**: Server-side runtime for the broad scenario matrix; retain the
existing browser determinism check unchanged for bundling coverage.

**Rationale**: Verified by execution, not inspection. With the SDK's own test
plumbing the agent created a real multisig account against testnet from
headless Node and synced to a live block. Nothing in
`packages/miden-multisig-client/src` touches `window`, `localStorage` or
`sessionStorage`; the single `document` use is already guarded and has a
byte-level twin; `navigator` is explicitly guarded for non-browser contexts.

Three conditions apply, all already solved in-repo by
`packages/miden-multisig-client/vitest.config.ts` and `tests/setup-wasm.ts`:
the bare specifier must resolve to the WASM entry rather than the package's
`node` condition, an IndexedDB shim must be installed before the SDK is
imported, and the WASM module must be initialized from bytes.

**Consequence fed into the spec**: the package's published `node` entry point
omits exports the SDK requires, so a consumer importing it in stock Node fails
at module-link time. The harness can alias around it, but FR-023f now requires
recording that as a finding against the artifact rather than absorbing it.

**Alternatives considered**: Browser-only via Playwright for everything
(rejected: cost, and it would not have been more faithful, since the same WASM
runs either way). The package's native `node` entry (rejected: incomplete
export surface, which is the defect above).

**Retained**: the Playwright determinism spec is the only check that the
browser bundling pipeline still produces byte-identical accounts. A
server-side suite does not subsume it. FR-023g.

---

## D3. Deterministic profile hermeticity

**Decision**: Hermetic within the run's own boundary, with a local stand-in for
the chain RPC endpoint. Not hermetic in the sense of omitting it.

**Rationale**: `ServerBuilder::build()` performs a real connection to
`GUARDIAN_MIDEN_RPC_ENDPOINT` at startup and retries for roughly 35 seconds
before failing. A profile that leaves the endpoint unset does not boot. The
stand-in only needs to complete the transport handshake: every request path
this profile exercises (`configure`, `push_delta_proposal`, `get_state`,
proposal listing and signing) is local-only, confirmed by the server's own
`lazy_for_test` construction whose documentation states these paths never issue
an RPC.

Canonicalization and abandon resolution do reach the node and therefore belong
to the live profile. FR-014b.

**Alternatives considered**: Pointing at a public network for the deterministic
profile (rejected: it would stop being deterministic, and would put a PR gate
behind an external dependency). Making the connection lazy in the server
(rejected: a product change to suit a test, outside this feature's scope).

---

## D4. Server configuration for the provisioned stack

**Decision**: Pin the acknowledgement identity to a file-backed provider, raise
rate limits, inject the commit at image build, leave the stage variable unset.

**Rationale, per item**:

- *Acknowledgement identity*: the development default regenerates the keypair
  every boot. Account registration binds the server's acknowledgement
  commitment, so a fixture prepared against one boot is rejected by the next,
  and the restart assertion would fail for a configuration reason. FR-013a.
  The repository ships fixtures and an `ack-keygen` binary inside the image.
- *Rate limits*: the limiter keys by client address, and every caller from the
  host presents as one address, so the development allowance of 10/sec and
  60/min throttles the driver. The existing compose file already raises these.
  FR-008a.
- *Commit identity*: the build context excludes version-control metadata, so
  an image built without the build argument reports `unknown` and the identity
  assertion passes vacuously. FR-007b.
- *Stage*: production mode forces a cloud secret provider for acknowledgement
  keys and JSON logging. Leave it unset for the profile.

**Isolation**: per-run compose project naming namespaces volumes, networks and
container names with no file changes. Host port mappings are literals today and
must be parameterized, or published ephemerally and read back. FR-008.

---

## D5. Error assertion target

**Decision**: Assert `account_not_found`, raised inside a service, keyed on the
error code only.

**Rationale**: The envelope is `{code, message, meta}` with the code as the
stable contract and the message wording explicitly documented as unstable.
`account_not_found` is produced by the metadata lookup before timestamp
validation and before signature verification, so an unregistered account with a
throwaway signature yields the full envelope deterministically on both
transports.

Errors rejected at the transport boundary before a handler runs do not carry
the envelope on gRPC, so cross-transport parity must not be asserted on those.
FR-012b.

The existing in-process integration tests already lock the per-handler envelope
shape. The new value is proving it survives a real socket and a real client.
FR-012c.

---

## D6. Funding and treasury

**Decision** (requester, 2026-09-16): the treasury is a single-signature basic
wallet, built from `AuthSingleSig` plus `BasicWallet` and driven with the Miden
client directly. Both options were buildable with existing dependencies; the
pattern for this one already appears in `crates/shared`.

**Rationale**: diagnostic independence. A treasury of the same guarded kind the
suite tests shares a failure mode with the subject. A regression in the proposal
lifecycle would break funding first, and every live scenario would report a
setup failure rather than the defect. The treasury must not depend on the code
under test.

**Rationale**: There is no plain-wallet abstraction in this repository. A
treasury is therefore either an account of the same guarded kind the suite
tests, in which case every funding transfer is a full propose, sign and execute
round trip, or an account created and driven outside the SDKs this feature
qualifies. Both are viable; they differ in setup cost versus per-transfer cost.

**Enabling fact**: a new account can bootstrap from an inbound transfer without
a pre-funded vault, because the consuming transaction pays its fee from the
note it consumes. This is what makes per-run ephemeral accounts viable on a
fee-charging chain at all.

**Key custody**: the Rust builder accepts a caller-supplied secret key, and
`recover_by_key` discovers the accounts a key authorizes, so a run reconstructs
a working client from the secret alone without persisting an account id.

**Durability constraint fed into the spec**: a treasury account is invalidated
both by a chain data reset and by an SDK contract pin bump, because deployed
accounts are immutable and their procedure roots fix at creation. Re-creating
and re-funding is expected recurring maintenance. FR-033d, FR-033e.

**Concurrency constraint**: replay protection is per signer, so two runs
signing with the treasury key at once are rejected as replays. Correct server
behaviour, not a product failure. FR-033b, FR-033c.

---

## D7. Target networks

**Decision** (requester, 2026-09-15): both public networks are planned as full
targets, including the published pairing. The transport refusal below is
treated as a temporary devnet outage rather than a structural limitation.

**Evidence gathered**:

- The TypeScript SDK cannot reach devnet at all today. A direct probe returned
  HTTP 415 with `application/grpc` from `rpc.devnet.miden.io` for a gRPC-web
  request, against 200 from `rpc.testnet.miden.io` for the identical request.
  The Rust client speaks plain gRPC and is unaffected.
- Devnet serves historical account state for roughly 50 blocks, about 2.5
  minutes. Proposal re-execution at the anchor block loads every foreign
  account the transaction touches, and every fee-paying transaction touches the
  fee faucet, so past that window a proposal cannot be executed by anyone.
  Tracked upstream as issue #462.

**How the two differ, and why only one changes the matrix**: the transport
refusal is an outage and is absorbed at run time by FR-026f, so devnet stays
declared available and recovers with no manifest edit. The retention window is
documented upstream behaviour, tracked as issue #462, and persists after the
gateway recovers. It therefore bounds which scenarios can be *required* on
devnet: flows whose step budget exceeds the window are marked not-required for
that network rather than declared unavailable. Multi-step flows (offline export
and import, cross-SDK handoff, wider signature collection) are the ones
affected. FR-018b, FR-018c, FR-018d.

---

## D8. CI integration

**Decision**: New workflows copy the newest pinned action generation. Post-
publication qualification is triggered by a job added to the publishing
workflow, not inferred from a completed run. Treasury credentials live in
GitHub Environments separate from the existing deployment environments.

**Rationale**: Two generations of pinned action SHAs are in flight; the three
most recently touched workflows carry the newer set. The publishing workflow
does not expose the image digest as a job or workflow output, but the
deployment workflow already proves digest re-resolution from the registry plus
attestation verification, so the pattern exists.

`devnet` and `testnet` environments already exist, carrying deployment
variables and OIDC role references. Reusing them for treasury secrets would put
qualification dispatches in reach of the deployment identity. Separate
environments keep the two apart.

**Caution recorded**: the attestation check pins the signer workflow by
filename, so renaming the publishing workflow breaks deployment verification.

**Alternatives considered**: `workflow_run` chaining (rejected: it executes in
default-branch context with awkward secret semantics, and the repository uses
no `workflow_run` anywhere). Re-resolving the digest with no publishing-side
change (viable, and the fallback if adding a job proves contentious, but it
does not satisfy FR-025c's requirement that the publishing pipeline initiate).
