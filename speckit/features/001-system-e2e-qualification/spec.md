# Feature Specification: Miden system qualification (black-box system E2E and multisig)

**Feature Branch**: `001-system-e2e-qualification`
**Created**: 2026-09-15
**Status**: Draft
**Tracking issue**: [OpenZeppelin/guardian#432](https://github.com/OpenZeppelin/guardian/issues/432)
**Input**: User description: "Improve the verification story with robust real tests that run nightly or pre-release. A funded account seeded through CI secrets can fund per-run test accounts. Cover the most used Miden actions first: configuring accounts with different signature schemes, creating different multisig shapes, creating and executing proposals. Dashboard and operator APIs come later. Start the server and its database inside the workflow, building the image or the server from the branch under test."

## Overview

Guardian is a custody product. Today's automated verification proves that each
layer compiles and behaves correctly in isolation: server modules, in-process
E2E, Postgres-backed implementations, and each client package. Nothing
automated proves that the assembled system works: the shipped server image,
talking to a real database, over real HTTP and gRPC sockets, driving real Miden
transactions through both multisig SDKs.

That gap is currently covered by a human running `examples/demo` and the
browser smoke harnesses by hand, and recording in the PR what was exercised.
That is slow, inconsistent between reviewers, and skipped under time pressure.
Several production-visible defects reached `main` because a flow that was never
re-run by hand had silently broken (stale compiled artifacts, cross-client
convention drift, and post-upgrade regressions that only reproduce against a
live chain).

This feature introduces a **qualification suite**: a set of named end-to-end
scenarios that run against an assembled Guardian stack, with two execution
profiles.

- A **deterministic profile** with no external chain dependency, fast and
  reliable enough to gate pull requests.
- A **live profile** that drives real Miden transactions, funded from a
  CI-held treasury account, run nightly and before every release.

Both multisig SDKs are covered. The TypeScript SDK leads, because it is
published and has consumers outside this repository: a regression there is
visible to other people before it is visible here.

The suite must be runnable locally with one command so that any failure it
reports can be reproduced and debugged without waiting on CI.

## Clarifications

### Session 2026-09-15 (@zeljkoX)

**Target environment**: The live profile targets both public Miden networks,
devnet and testnet. One workflow definition drives both, parameterised by a
target-environment input, with one environment per run invocation. The
scheduled run covers both. Devnet and testnet results are recorded separately
and never merged into a single verdict, because the two networks routinely run
different protocol lines during an upgrade window.

**Treasury key custody**: Treasury credentials are held as protected
environment secrets, exposed only to the jobs that need them, without required
reviewers so scheduled runs stay unattended. The live profile never runs on a
fork-originated event, so fork exposure is structural rather than
policy-dependent. A managed secret store was considered and rejected as
disproportionate: the funds at risk are devnet and testnet balances with no
market value.

**Release gating**: A failed live run denotes failure and is recorded against
the ref, but does not block publication. Test-side and environment-side
problems are common enough that an automatic block would be a false gate. The
release decision stays with a human, who must record it.

### Session 2026-09-15 (spec review)

**Image source is a first-class dimension.** An earlier draft treated "build
the server from the commit under test" and "run the image operators actually
pull" as the same run selected by a different ref. They are not. The publishing
pipeline pushes only on a published release or a manual dispatch, and the
`latest` tag moves only on a full release, never on a pre-release. So the
published image can be weeks behind the default branch, and a build from the
default branch can be green while the published artifact is stale or broken.
Every run now declares its image source, and identity is asserted differently
for each.

**SDK checkout follows the image.** Driving a published image with SDKs from
the default branch produces false failures after any contract change. Two
pairings are defined: a *branch pairing* (image built from a ref, SDKs from the
same ref) and a *release pairing* (published image digest, SDKs from the
matching tag).

**Nightly uses the branch pairing, not the published one.** The requester's
goal is catching regressions introduced by branch work, and because the
published `latest` only moves on full releases, a nightly against it would
mostly measure network drift rather than code changes. The published image is
qualified on publication, before a release, and on a slower recurring cadence.

**Green has two meanings and both are specified.** The deterministic profile is
a required gate whose failure blocks a merge. The live profile is not a gate:
it reports red on a product failure so that a failure is visible and notified,
and it does not report red when a network is environment-blocked, because
otherwise every Miden upgrade window would leave the schedule red for days and
people would stop reading it. Per-network results are never combined into one
verdict.

### Session 2026-09-15 (second spec review)

**Coverage matrix replaces an open-ended lifecycle.** The requirements
previously let a single generic proposal lifecycle satisfy coverage. A named
minimum set of user-facing actions is now required (FR-019a), with a declared
coverage matrix deciding when a run may claim qualification (FR-003c to
FR-003e) and an explicit statement of what a green result does not cover
(FR-003f).

**Mixed-scheme accounts were required but are not buildable.** An earlier draft
required a multisig shape mixing signature schemes among its signers. Verified
against both SDKs: the on-chain storage layout holds a scheme per signer, but
both account builders assign one configured scheme to every signer, so neither
can construct such an account. It is now a recorded capability gap (FR-017a)
rather than required coverage.

**Proposals do not reach a terminal status.** An earlier draft asserted
completion by observing a proposal's terminal status through Guardian.
Verified against the documented behaviour: proposals are removed once
canonicalized, so that status is never observed and the assertion would hang.
Completion is now the conjunction of chain confirmation, canonical delta
history, commitment agreement, and absence from pending proposals (FR-018,
FR-018a).

**Cross-SDK handoff is now required.** Running each SDK independently proves
neither interoperates with the other. FR-023e requires handoffs in both
directions on a shared account.

**Permission to publish does not amend the result** (FR-041c).

### Session 2026-09-15 (trigger allocation, requester)

**The scheduled run carries the full scenario set**, not a core subset. The
earlier draft rotated expensive scenarios out of the nightly to protect
capacity. The requester's call is that nightly proves everything and the cost
is accepted. SC-013 sets a 3-hour-per-network window with concurrency across
scenarios that do not share an account, and SC-013a forbids the obvious
failure mode: silently trimming the set to keep the window while still
reporting it as the full run.

**Live scenarios become available on pull requests, opt-in and off by
default** (FR-025j). A reviewer who wants chain-level evidence for a change can
request the core subset; no pull request pays for it otherwise. This replaces
the earlier flat prohibition on live-on-pull-request. Two guards come with it,
because a pull request can edit the workflow that reads the treasury secret:
the opt-in path resolves its workflow definition from the default branch and
treats the pull request only as code under test, never runs for a fork
(FR-025k), and records who asked for it so treasury spend is attributable
(FR-025l).

**Pre-release and post-publication carry the full live set against the
published digest** (FR-025e), unchanged in intent but no longer the only
trigger that runs everything.

The deterministic profile is unaffected: it remains a required pull-request
check under FR-015 and FR-015a.

**Published SDK packages are in scope** (requester's decision, 2026-09-15).
A third pairing joins the two above: published image digest plus both SDKs
installed from their registries (FR-025a, FR-025f to FR-025i). It catches
packaging defects that an in-repo SDK structurally cannot surface, such as a
wrong file manifest, missing build output, or an unsatisfiable dependency
range. It carries two costs written into the requirements: the install must
happen where this repository's path-based package linking cannot substitute
local code for published code, and the image and SDK versions must be checked
for skew, because they are released by separate pipelines. The nightly schedule
stays on the branch pairing; the published pairing runs post-publication,
pre-release, and at least weekly.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Nightly Miden multisig canary on the TypeScript SDK (Priority: P1)

Every night, and on demand, the most used Guardian flows run end to end
against a real Miden network through the TypeScript multisig SDK, in the
runtime its consumers actually use: a multisig account is configured and
registered with Guardian, a proposal is created, threshold signatures are
collected, the proposal executes on chain, and the resulting account state is
verified against both Guardian and the chain. Each supported signature scheme
and several multisig shapes are exercised. When a flow breaks, the team learns
within a day and sees which named scenario failed.

**Why this priority**: The TypeScript SDK is published and has external
consumers, so a regression in it reaches people outside this repository before
anyone here notices. It is also the surface with the longest history of silent
breakage, because its shipped artifacts are built separately from its sources
and its transport behaviour is partly outside this project's control. Nothing
automated covers it today.

**Independent Test**: Run the live profile with only the TypeScript scenarios
selected, against a configured network with a funded treasury, and confirm
every named scenario reports a pass or an explicit skip, with no manual account
setup and without the Rust scenarios running.

**Acceptance Scenarios**:

1. **Given** a configured Miden test environment and a funded treasury,
   **When** the nightly run executes, **Then** a multisig account is created and
   registered with Guardian for each supported signature scheme, and each
   registration is confirmed by reading the account back through Guardian.
2. **Given** a registered multisig account with threshold `t` of `n`,
   **When** a proposal is created and exactly `t` signatures are collected,
   **Then** the proposal becomes executable, executes on chain, and the account
   state commitment reported by Guardian converges with the chain.
3. **Given** a registered multisig account with threshold `t` of `n`,
   **When** fewer than `t` signatures are collected, **Then** execution is
   refused and the proposal remains pending.
4. **Given** a scenario that fails, **When** the run finishes, **Then** the
   report names the failing scenario, the multisig shape and scheme it used,
   and the step at which it failed.
5. **Given** an intentionally introduced regression in the proposal execution
   path, **When** the nightly run executes, **Then** at least one scenario
   fails rather than passing or erroring ambiguously.
6. **Given** the two public networks are running different protocol lines,
   **When** the scheduled run covers both, **Then** each network's result is
   reported separately and one network's outcome never masks the other's.
7. **Given** the SDK's shipped artifacts are stale relative to its sources,
   **When** a run starts, **Then** it fails with a stale-artifact outcome
   rather than exercising code that does not match the commit under test.

---

### User Story 2 - The same canary runs on the Rust SDK (Priority: P1)

The identical named scenarios run through the Rust multisig SDK against the
same networks, so the reference implementation is covered to the same depth as
the published TypeScript one.

**Why this priority**: The Rust SDK is the reference implementation and the one
the server's own behaviour is designed against, so a defect here is usually a
defect everywhere. It sits alongside the TypeScript canary rather than ahead of
it only because its consumer surface is narrower.

**Independent Test**: Run the live profile with only the Rust scenarios
selected and confirm they complete without the TypeScript scenarios running.

**Acceptance Scenarios**:

1. **Given** a scenario defined once, **When** the Rust scenarios are selected,
   **Then** that scenario executes through the Rust SDK and reports its own
   outcome.
2. **Given** a registered multisig account, **When** a proposal is created,
   signed to threshold, and executed through the Rust SDK, **Then** the account
   state commitment reported by Guardian converges with the chain.
3. **Given** the Rust scenario set, **When** it is compared with the TypeScript
   scenario set, **Then** any flow covered on one side and not the other is
   listed as an explicit gap with a reason.

---

### User Story 3 - Test accounts fund themselves from a CI-held treasury (Priority: P1)

The live profile needs accounts that hold the chain's native asset, because
the multisig authentication component pays the transaction fee out of the
account vault. Each target network has one long-lived treasury account, topped
up by a maintainer from that network's public faucet and held in CI
configuration. Each run derives fresh ephemeral accounts and funds them from
the treasury belonging to the network it targets, with the minimum amount the
scenarios need. Nobody has to fund anything by hand for a run to succeed.

**Why this priority**: Without this, the live profile cannot run unattended at
all. It is the enabling constraint on User Stories 1 and 2.

**Independent Test**: Trigger a live run on a clean CI runner with only the
treasury configuration present and confirm every scenario account is funded
automatically and the run reports the total amount spent.

**Acceptance Scenarios**:

1. **Given** a treasury with sufficient balance, **When** a run starts,
   **Then** each ephemeral run account is funded before it is first used, and
   no scenario fails for lack of funds.
2. **Given** a treasury whose balance is below what the run needs, **When** a
   run starts, **Then** the run stops immediately with a distinct, actionable
   "treasury underfunded" outcome naming the current balance and the required
   amount, rather than failing partway through a scenario.
3. **Given** any completed run, **When** the report is read, **Then** it states
   the treasury balance at start, the amount spent, and the projected number of
   further runs the remaining balance supports.
4. **Given** a target chain that charges a zero verification base fee,
   **When** a run starts, **Then** funding is skipped and the run records that
   it was not required.
5. **Given** any run, **When** its logs and artifacts are inspected, **Then**
   the treasury signing key does not appear in any of them.
6. **Given** a run targeting one network, **When** it funds its accounts,
   **Then** it draws only on that network's treasury.

---

### User Story 4 - One command reproduces any qualification failure locally (Priority: P1)

A developer who sees a red qualification run can execute the same suite on
their own machine with a single documented command. The command provisions the
stack it needs, runs the same named scenarios, and tears everything down
afterwards. Selecting a single scenario or profile is possible so that a
narrowed failure can be iterated on quickly.

**Why this priority**: A test that can only be run in CI is a test nobody
debugs. The issue lists this as the first acceptance criterion.

**Independent Test**: On a clean checkout with a working container runtime, run
the documented command and confirm the deterministic profile completes without
any additional manual setup.

**Acceptance Scenarios**:

1. **Given** a clean checkout, **When** the documented command is run,
   **Then** the stack is built or started, the deterministic scenarios execute,
   and the stack is removed, with no residual containers, volumes, or databases.
2. **Given** a developer who wants one scenario, **When** they pass the
   scenario name to the command, **Then** only that scenario runs.
3. **Given** a run that is interrupted, **When** the developer checks their
   machine, **Then** no resources created by the run are left behind.
4. **Given** local credentials for a Miden test environment and a treasury,
   **When** the developer selects the live profile, **Then** the same scenarios
   CI runs execute locally and produce the same report shape.

---

### User Story 5 - Pull requests are gated on the assembled system, not just its parts (Priority: P2)

A deterministic profile runs against the real server image built from the
commit under test, backed by Postgres, exercised through the published HTTP and
gRPC ports by both base clients. It verifies the reported build identity,
asserts a structured API error, and proves that data survives restarting only
the Guardian container. It carries no external chain dependency, so it is
reliable enough to be a required check.

**Why this priority**: This closes the "the pieces work but the assembly does
not" gap on every change rather than once a night, and it is cheap because it
touches no external network. It is P2 only because the live canary covers the
riskier surface.

**Independent Test**: Run the deterministic profile against a freshly built
image and confirm it passes, then point it at a stale image and confirm the
build-identity check fails.

**Acceptance Scenarios**:

1. **Given** a commit under test, **When** the deterministic profile runs,
   **Then** the server reports a version, commit, and environment matching that
   commit, and a mismatch fails the run.
2. **Given** a running stack, **When** fixture account and proposal flows are
   driven through the TypeScript HTTP client and through the Rust gRPC client,
   **Then** both complete successfully against the image's published ports.
3. **Given** a request that the server must reject, **When** it is sent,
   **Then** the client surfaces a structured error whose code and message shape
   are asserted, not just a non-success status.
4. **Given** data written during the run, **When** only the Guardian container
   is restarted, **Then** the account, state, and proposal data remain readable
   through both clients.
5. **Given** the stack is not ready yet, **When** the harness waits, **Then** it
   polls for readiness against a bounded deadline and fails with a clear
   timeout message, never by exhausting a fixed sleep.

---

### User Story 6 - Cross-SDK divergence is caught, not just discovered later (Priority: P2)

With both SDKs running the same named scenarios, the report shows their
outcomes side by side, and where the two are expected to produce the same
on-chain result the suite asserts that they do.

**Why this priority**: Rust and TypeScript drift is a named high-risk area in
the repository guide, and several past defects were exactly this: a convention
that changed on one side only. It is P2 rather than P1 because it is only
deliverable once both canaries exist, and because each canary already catches
its own side's regressions.

**Independent Test**: Run a scenario on both SDKs with a deliberate
convention difference introduced on one side and confirm the parity assertion
fails and names both sides.

**Note on what is comparable**: two SDKs driving two separately created and
separately funded accounts will not produce equal state commitments, so
commitment equality is only a valid assertion where both drive the same
account. The default comparison is therefore over typed outcomes, with
same-account commitment comparison reserved for scenarios that set it up
deliberately.

**Acceptance Scenarios**:

1. **Given** a scenario defined once, **When** the live profile runs both SDKs,
   **Then** both outcomes appear in the report against the same scenario
   identifier.
2. **Given** a scenario where both SDKs are expected to agree, **When** their
   results are compared, **Then** any divergence in typed outcome fails the run
   and names both sides. Typed outcome means proposal status, threshold
   accounting, structured error code, and executability decision: values that
   are comparable between two independently funded accounts.
3. **Given** a scenario where both SDKs act on the same account, **When** their
   results are compared, **Then** the resulting state commitment is compared
   directly as well.
4. **Given** the two SDKs pinned to different Miden dependency lines, **When** a
   run starts, **Then** it fails fast with a pin-mismatch outcome rather than
   producing confusing downstream signature failures.

---

### User Story 7 - Offline signing and guardian migration are covered (Priority: P2)

Beyond the online happy path, the suite covers exporting a proposal, signing it
outside the originating client, importing it back, and executing it, plus
migrating an account to a different guardian.

**Why this priority**: These are the flows least likely to be exercised by hand
and most likely to break silently, but they are lower frequency than the core
create-sign-execute path.

**Independent Test**: Run the offline and migration scenarios alone against a
live environment and confirm they complete without touching the online
execution scenarios.

**Acceptance Scenarios**:

1. **Given** a pending proposal, **When** it is exported, signed by a cosigner
   outside the originating client, and imported back, **Then** the imported
   signature counts toward the threshold and the proposal executes.
2. **Given** an executed proposal, **When** the online and offline paths are
   compared, **Then** they produce equivalent resulting account state.
3. **Given** a registered account, **When** it is migrated to a different
   guardian, **Then** both the old and the new guardian report the expected
   ownership, and subsequent proposals execute against the new one.

---

### User Story 8 - Operator and dashboard APIs are qualified against the real stack (Priority: P3)

The operator client is exercised against the real Postgres-backed server:
authenticating with a deterministic operator key, establishing and inspecting a
session, listing accounts and reading account detail, being denied when a
permission is missing, picking up an allowlist change without a restart,
logging out, and leaving the expected audit trail in the database.

**Why this priority**: Explicitly deferred by the requester in favour of the
Miden flows. It is valuable but it guards an operator-facing surface rather
than the custody path.

**Independent Test**: Run the operator scenarios against a stack with a seeded
operator allowlist and confirm each assertion independently of the multisig
scenarios.

**Acceptance Scenarios**:

1. **Given** a deterministic operator key on the allowlist, **When** it
   authenticates, **Then** a session is established and its properties can be
   inspected.
2. **Given** an authenticated operator lacking a required permission,
   **When** it calls a protected endpoint, **Then** the call is denied with the
   expected structured error.
3. **Given** a running server, **When** the operator allowlist changes,
   **Then** the change takes effect without restarting the server.
4. **Given** an authenticated session, **When** the operator logs out,
   **Then** the session is invalid for subsequent calls.
5. **Given** a completed operator session, **When** the database is inspected,
   **Then** the expected audit events are present.

---

### User Story 9 - Releases carry a recorded qualification result (Priority: P3)

Before a release is published, the live profile runs against the exact ref
being released, on each target network, and its result is recorded against that
ref. The result informs the release decision but does not automatically block
it, because a red run may reflect a test defect or an unavailable network
rather than a product defect. Skipped or disabled scenarios are reported as
skipped and never counted as passes, so whoever makes the call knows precisely
what was proven.

**Why this priority**: It converts the suite's output into something a release
decision can be made against. It depends on every earlier story existing first.

**Independent Test**: Trigger a pre-release run against a tagged ref and
confirm the recorded result names the ref, the target network, and the
per-scenario outcome.

**Acceptance Scenarios**:

1. **Given** a ref being prepared for release, **When** the pre-release run
   executes, **Then** the result is recorded against that ref and the resolved
   image digest, with per-scenario outcomes, the pairing used, and the target
   network identified.
2. **Given** a run where some scenarios were disabled or rotated out,
   **When** the result is read, **Then** those scenarios appear as skipped with
   a reason, and the run does not report full coverage.
3. **Given** a run that failed because the external network was unavailable,
   **When** the result is read, **Then** it is labelled as an environment
   failure and distinguished from a product failure.
4. **Given** a live run that was not fully green, **When** the release is
   published anyway, **Then** publication is not blocked, and the release
   record names who decided to proceed and what was outstanding at that moment.
5. **Given** a server image has just been published, **When** the publishing
   pipeline triggers qualification, **Then** the run targets that exact digest
   and its result is discoverable from the release rather than only from run
   logs.

---

### Edge Cases

- The Miden node, prover, or RPC endpoint is unreachable, rate limiting, or
  running a protocol version the pinned SDKs do not support. The run must
  report an environment failure naming the observed and expected versions, not
  a product failure.
- Devnet and testnet disagree: the same scenario passes on one network and
  fails on the other. Both results must stand on their own, and the divergence
  itself is the signal, most often a protocol-line difference during an
  upgrade window.
- A run is dispatched against one network while configured with the other
  network's treasury or RPC endpoint.
- A reviewer opts a pull request into a live run, and that pull request
  modifies the qualification workflow itself or the treasury handling code.
- Several pull requests are opted into live runs at once, competing for the
  same treasury and the same proving capacity as the scheduled full run.
- A proposal is accepted but never canonicalizes within the scenario's
  deadline. The run must fail with a timeout naming the last observed status
  rather than hanging.
- A state-changing submission times out with an unknown outcome. The suite must
  not blindly resubmit; it must record the unknown outcome and resolve it by
  observation.
- Two runs (a scheduled one and a manually dispatched one) overlap. Each must
  operate on its own accounts and its own stack without interfering.
- The treasury is drained by a concurrent run between the balance check and the
  funding transfers.
- The target chain charges a zero fee, so funding is unnecessary and any
  funding assertion would be vacuous.
- The image build fails, or the built image does not match the commit under
  test.
- A run is cancelled mid-flight, leaving containers, volumes, or ephemeral
  accounts behind.
- A scenario fails in a way that would put a signed payload or a private key
  into the captured diagnostics.
- Diagnostics from a long-running failure grow without bound.

## Requirements *(mandatory)*

### Functional Requirements

#### Scenario model

- **FR-001**: The suite MUST express its coverage as named scenarios, each with
  a stable identifier that appears unchanged in local output, CI output, and
  recorded results.
- **FR-002**: Each scenario MUST declare the dimensions it covers, at minimum:
  execution profile (deterministic or live), image source (built from a ref or
  pulled from the registry), target network for live scenarios, signature
  scheme, multisig shape (threshold of total), execution mode (online or
  offline), the client SDK driving it, the SDK source (in-repo or installed
  from a registry), and for TypeScript scenarios the runtime it runs in.
- **FR-003**: A scenario MUST report exactly one of: passed, failed, skipped
  with a reason, or blocked by the external environment. Skipped and blocked
  outcomes MUST NOT be reported as passes.
- **FR-003a**: Scenario outcomes MUST map to run conclusions by a stated rule:
  a deterministic-profile failure concludes the run as failed and blocks a
  merge; a live-profile product failure concludes the run as failed and
  notifies, without blocking publication; a live-profile run whose only
  non-passing scenarios are environment-blocked MUST NOT conclude as failed,
  and MUST surface the blocked scenarios in its result.
- **FR-003b**: Runs against different target networks MUST conclude
  independently. One network's outcome MUST NOT determine another's, and the
  suite MUST NOT present a single combined status for a multi-network run. A
  summary stating whether every required network passed is permitted, provided
  the per-network results remain separately visible.
- **FR-003c**: The suite MUST declare a coverage matrix naming which scenarios
  are required for qualification, per profile, network, SDK, and pairing.
- **FR-003d**: A run MAY claim qualification only when every required entry in
  that matrix passed. A required entry that is skipped, environment-blocked,
  missing from the run, or cancelled MUST prevent the qualification claim, even
  where FR-003a keeps the run's conclusion out of the failed state.
- **FR-003e**: A filtered run MUST report that the selected scenarios passed
  and MUST NOT present itself as a qualification. On-demand runs are the common
  case here, and the distinction is what stops a narrow green run being read as
  full coverage.
- **FR-003f**: A result MUST state what it does not cover. Qualification is
  scoped to the Miden system: the EVM surface is out of scope entirely and the
  operator surface is deferred, so a green result MUST NOT be presentable as
  evidence that every Guardian feature works.
- **FR-004**: Scenarios MUST be selectable individually and by dimension, both
  locally and when dispatched in CI.

#### Stack provisioning

- **FR-005**: The suite MUST provision a Guardian server backed by Postgres and
  MUST NOT depend on a previously running stack. The server image MUST come
  from one of two declared sources: built from a named ref, or pulled from the
  registry by tag or digest.
- **FR-006**: The suite MUST wait for stack readiness by polling against a
  bounded deadline and MUST fail with an explicit timeout message. Fixed sleeps
  as a readiness mechanism are prohibited.
- **FR-007**: The suite MUST assert the identity of the server it is testing
  and MUST fail the run on mismatch. In built-from-ref mode, the reported
  version, commit, and environment MUST match the ref under test. In
  pulled-from-registry mode, the assertion MUST be against the resolved image
  digest and the revision recorded in the image's own metadata, not against
  whatever ref the run was launched from.
- **FR-007a**: A run MUST record the resolved image digest in its result, so
  that a result can be traced to the exact artifact it qualified.
- **FR-007b**: In built-from-ref mode the commit identity MUST be injected at
  image build time. The build context excludes version-control metadata, so an
  image built without it reports an unknown commit and the FR-007 assertion
  would silently compare nothing.
- **FR-008**: The suite MUST isolate all per-run state (database, storage,
  container names, account identifiers) so that concurrent runs cannot collide.
- **FR-008a**: The provisioned server's request rate limits MUST be configured
  to accommodate the suite's own request rate. The limiter keys by client
  address, so an automated driver presents as a single caller and would trip
  the default allowance, producing throttling failures that look like product
  defects.
- **FR-009**: The suite MUST remove every local and CI resource it created
  (containers, volumes, databases, working directories) on completion, on
  failure, and on cancellation. On-chain accounts and transactions cannot be
  removed and are explicitly outside this guarantee; FR-033a governs their
  residual balances.
- **FR-009a**: The suite MUST be able to recover local resources abandoned by
  an earlier run that died without cleaning up, so that a killed run does not
  degrade the next one.
- **FR-010**: A single documented command MUST run the suite locally, and MUST
  work on a clean checkout with no manual setup beyond a container runtime and,
  for the live profile, environment credentials.

#### Deterministic profile

- **FR-011**: The deterministic profile MUST exercise the assembled system
  through the server's published HTTP and gRPC ports, using the TypeScript HTTP
  client and the Rust gRPC client respectively.
- **FR-012**: The deterministic profile MUST assert at least one structured API
  error over the wire, checking the stable error code and the presence and
  shape of the error envelope rather than only a failure status.
- **FR-012a**: Assertions MUST key on the error code, never on message wording.
  The code is the stable contract; the user-facing sentence is explicitly not.
- **FR-012b**: The asserted error MUST be one raised inside a handler or
  service, so that both transports carry the envelope. Errors rejected at the
  transport boundary before a handler runs do not carry it on every transport,
  and asserting cross-transport parity on one of those would be asserting
  something untrue.
- **FR-012c**: The profile MUST NOT re-assert the per-handler envelope shape
  that existing in-process tests already cover. Its distinct value is proving
  the envelope survives a real socket and a real client, end to end.
- **FR-013**: The deterministic profile MUST restart only the Guardian
  component and then confirm that account, state, and proposal data written
  before the restart remain readable.
- **FR-013a**: The provisioned server MUST be configured with a persistent
  acknowledgement identity. Under the development default the acknowledgement
  keypair is regenerated on every boot. This breaks the profile twice over:
  account registration binds the server's acknowledgement commitment, so a
  fixture prepared against one boot is rejected by the next, and the restart
  assertion would report a failure caused by its own configuration rather than
  by any durability defect.
- **FR-014**: The deterministic profile MUST NOT depend on any external
  network, chain, or prover reachable outside the run's own boundary.
- **FR-014a**: The profile MUST supply a local stand-in for the chain RPC
  endpoint the server dials. The server establishes that connection eagerly at
  startup and will not finish booting without something answering there, so a
  profile that simply leaves the endpoint unset does not start. The stand-in
  need only complete the transport handshake: the request paths this profile
  exercises perform no chain calls.
- **FR-014b**: The profile MUST NOT assert any behaviour that requires a real
  chain. Canonicalization to a canonical state and abandon resolution both
  need a live node and belong to the live profile.
- **FR-015**: The deterministic profile MUST run on every pull request other
  than documentation-only ones, on pushes to the default branch, and on manual
  dispatch. Filtering by which layers a pull request touches is prohibited: the
  cross-layer breaks this profile exists to catch are precisely the ones a
  path filter would skip.
- **FR-015a**: The deterministic profile MUST be a required check, and its
  failure MUST block a merge.
- **FR-015b**: The deterministic profile MUST also be runnable against a
  published image digest, so that a newly published artifact is checked for
  assembly and durability before any live scenario spends treasury funds on it.

#### Live profile: Miden coverage

- **FR-016**: The live profile MUST cover account configuration and Guardian
  registration for every signature scheme the product supports.
- **FR-017**: The live profile MUST cover the multisig shapes `1-of-1`,
  `2-of-3`, and `3-of-3`, each in a homogeneous Falcon and a homogeneous ECDSA
  variant.
- **FR-017a**: Mixed-scheme accounts, where signers on one account use
  different signature schemes, MUST be recorded as a known capability gap and
  MUST NOT be required coverage. The on-chain storage layout holds a scheme per
  signer, but neither SDK's account builder can construct such an account
  today: both assign one configured scheme to every signer. Requiring this
  coverage would block qualification on unauthorised SDK work. Closing the gap
  is a separate decision.
- **FR-018**: The live profile MUST cover the full proposal lifecycle: create,
  collect signatures up to the threshold, and execute on chain. Completion MUST
  be asserted as the conjunction of: the transaction confirmed on chain, a
  canonical delta recorded in the account's history, the account commitment
  matching between Guardian and the chain, and the proposal no longer appearing
  among pending proposals.
- **FR-018a**: The suite MUST NOT wait for a proposal to reach a terminal
  status through Guardian. Proposals are removed once canonicalized, so a
  terminal status is never observed and any such wait would hang until its
  deadline.
- **FR-019**: The live profile MUST verify, after execution, that the account
  state commitment reported by Guardian agrees with the chain.
- **FR-018b**: A proposal MUST complete signature collection and execution
  within the target network's historical-state window. Re-execution at the
  anchor block loads every foreign account the transaction touches, and every
  fee-paying transaction touches the fee faucet, so once the node prunes that
  block the proposal cannot be verified or executed by anyone, including its
  proposer. Scenario design MUST treat this window as a hard budget rather than
  discovering it as flakiness.
- **FR-018c**: A proposal lost to anchor pruning MUST report environment-
  blocked, naming the window, and MUST NOT report a product failure. The suite
  MUST NOT respond by re-proposing silently, because a silent re-propose turns
  a structural limit into an invisible cost.
- **FR-018d**: Scenarios whose step count cannot fit a given network's window
  MUST be declared unavailable for that network in the coverage matrix, per
  FR-026f. Multi-step flows such as offline export and import, and cross-SDK
  handoffs, are the ones at risk.
- **FR-019a**: The live profile MUST cover, for both SDKs and both networks,
  a minimum set of user-facing actions rather than a single generic lifecycle:
  account creation and Guardian registration; account recovery by another
  cosigner; asset transfer and note consumption, each asserted against
  before-and-after balances rather than transaction acceptance alone;
  below-threshold rejection; and duplicate-signature handling.
- **FR-019b**: A generic proposal lifecycle passing MUST NOT be sufficient to
  report the actions in FR-019a as covered. Each action is a separately named
  scenario with its own outcome.
- **FR-020**: The live profile MUST assert the negative case: a proposal with
  fewer than threshold signatures is not executable.
- **FR-021**: The live profile MUST cover exporting a proposal, signing it
  outside the originating client, importing it back, and executing it, and MUST
  assert that the resulting state matches the equivalent online path. This
  export and import path applies to any proposal type.
- **FR-021a**: Creating a proposal while offline is supported only for guardian
  migration; every other proposal type must be created online before it can be
  exported. Scenario coverage MUST respect that boundary rather than assuming
  offline creation is general, and a scenario asserting offline creation for
  another proposal type would be asserting an error path, not a feature.
- **FR-022**: The live profile MUST cover migrating an account to a different
  guardian.
- **FR-023**: The live profile MUST execute its scenarios through both the Rust
  and the TypeScript multisig SDKs and MUST fail the run when the two diverge
  on a scenario where they are expected to agree.
- **FR-023e**: The live profile MUST cover cross-SDK handoffs on a shared
  account, in both directions: an account or proposal created in Rust and
  signed or executed in TypeScript, and the reverse. Running each SDK
  independently proves neither interoperates with the other, and the handoff is
  where cross-client convention drift actually surfaces.
- **FR-023a**: The TypeScript scenario set MUST be at least as complete as the
  Rust one. Any flow covered on one side and not the other MUST be recorded as
  an explicit, reasoned gap rather than left unstated.
- **FR-023b**: The TypeScript SDK MUST be exercised through the artifacts a
  consumer would install, not through sources compiled only for the test.
- **FR-023f**: Where the suite must apply a workaround to consume a published
  artifact, and a consumer installing that artifact would need the same
  workaround, the suite MUST record it as a finding against the artifact. It
  MUST NOT absorb the workaround silently. A harness that quietly patches
  around a broken consumer entry point reports a pass for something a consumer
  cannot do.
- **FR-023h**: Where one SDK can perform a scenario's step and the other
  cannot, the leg that cannot MUST report an outcome naming the capability gap
  rather than passing or failing. A pass would claim coverage that does not
  exist; a failure would be indistinguishable from a regression. Two such gaps
  are known: mixed-scheme accounts (FR-017a), and offline signature collection
  for proposal types that require a GUARDIAN acknowledgement, which the Rust
  SDK refuses and the TypeScript SDK allows. A third is threshold change, which
  the contract supports and the TypeScript SDK drives, but which the Rust
  transaction builder rejects outright.
- **FR-023g**: The existing browser determinism check MUST be retained
  alongside the broad runtime matrix, not replaced by it. It is the only check
  that the browser bundling pipeline still produces byte-identical accounts,
  and a server-side runtime does not subsume it.
- **FR-023d**: Runtime MUST be a scenario dimension for TypeScript scenarios.
  The package declares support for more than one runtime, and its failure modes
  differ between them: the browser path carries bundling, WASM loading, and
  browser storage risks that a server-side runtime does not exercise, while a
  server-side runtime is cheap enough to carry the broad scenario matrix. The
  suite MUST cover the broad matrix in whichever runtime is cheaper to run
  unattended, and MUST cover the browser-specific risks in at least one
  scenario. Neither substitutes for the other.
- **FR-023c**: The suite MUST verify that the TypeScript SDK's shipped
  artifacts correspond to the commit under test, and MUST fail with a
  stale-artifact outcome otherwise. Artifacts that survive a branch switch and
  silently exercise older behaviour are a known failure mode of this surface.
- **FR-024**: The live profile MUST verify before running that both SDKs are
  pinned to the same Miden dependency line, and MUST fail fast with a
  pin-mismatch outcome otherwise.
- **FR-025**: The live profile MUST NOT be a required check and MUST NOT run on
  a pull request by default. It MUST be runnable on a schedule, on manual
  dispatch with every dimension selectable, on publication of a server image,
  against a release candidate before a release, and on explicit opt-in for a
  pull request.
- **FR-025a**: The suite MUST support three pairings of image and SDK sources,
  and every live run MUST declare which it used:
  - *Branch pairing*: image built from a named ref, both SDKs taken from that
    same ref.
  - *Release pairing*: published image resolved to a digest, both SDKs taken
    from the tag matching that image.
  - *Published pairing*: published image resolved to a digest, both SDKs
    installed from their registries at their published versions.
  Mixing sources across a pairing is prohibited, because SDKs from one ref
  against an image from another produce contract-mismatch failures that look
  like product defects.
- **FR-025f**: In the published pairing, the SDKs MUST be installed the way a
  consumer installs them: from the registry, resolved fresh, in a location
  where the repository's own workspace linking cannot substitute local packages
  for published ones. This repository links its packages to each other by path
  during development, so a published-pairing run executed inside the workspace
  would silently exercise local code and report a false pass.
- **FR-025g**: A published-pairing run MUST record the exact artifact set it
  qualified: the resolved image digest, the installed version of each SDK
  package, the integrity hash of each downloaded package, and the Miden
  dependency versions those packages resolved to.
- **FR-025h**: A published-pairing run MUST verify that the image digest and
  the installed SDK versions belong to a compatible set, and MUST fail fast
  with a skew outcome when they do not. The published image and the published
  SDKs are released by separate pipelines and can diverge.
- **FR-025i**: The published pairing MUST run after a successful publication of
  any of its artifacts, before a release, and on a recurring cadence of at
  least once a week, so that a regression in what consumers actually install is
  found without waiting for the next release.
- **FR-025b**: The scheduled run MUST use the branch pairing against the
  default branch, so that regressions introduced by merges are caught within a
  day.
- **FR-025c**: Publication of a server image MUST be able to trigger a live run
  against the exact digest just published, using the release pairing. The
  trigger MUST be initiated by the publishing pipeline itself rather than
  inferred from an unrelated completed run.
- **FR-025d**: A release candidate MUST be qualifiable before release using the
  release pairing against both networks.
- **FR-025e**: Scenario selection MUST be allocated by trigger as follows.
  Scenarios excluded by this allocation MUST be reported as skipped, per
  FR-003, and a run carrying less than the full set MUST NOT claim
  qualification, per FR-003d.
  - *Scheduled*: the full scenario set, both networks, branch pairing.
  - *Pull request, opt-in only*: the core subset, meaning account configuration
    and registration, proposal creation, threshold signing, and execution,
    across both signature schemes and both SDKs. Off unless a reviewer asks
    for it.
  - *Pre-release and post-publication*: the full scenario set, both networks,
    against the published digest.
  - *On demand*: any subset.
- **FR-025j**: The live profile MUST be opt-in on a pull request, off by
  default, so a reviewer who wants chain-level evidence for a change can
  request it without every pull request paying for it.
- **FR-025k**: An opt-in pull-request live run MUST NOT execute a workflow
  definition taken from the pull request's own branch while holding treasury
  credentials, and MUST NOT run for a pull request from a fork. A pull request
  can modify the workflow that would read the treasury secret, so the opt-in
  path MUST resolve its workflow definition from the default branch and take
  the pull request only as the code under test.
- **FR-025l**: An opt-in pull-request live run MUST record which person
  requested it, so treasury spend initiated from a pull request is
  attributable.
- **FR-026**: The live profile MUST support both public Miden networks, devnet
  and testnet, as target environments, selected per run by an explicit input.
  One run targets exactly one environment.
- **FR-026a**: The scheduled run MUST cover both target environments.
- **FR-026b**: Each target environment MUST have its own treasury account and
  its own credentials. A run MUST NOT be able to spend one environment's
  treasury while targeting the other.
- **FR-026c**: A run result MUST name its target environment, and results for
  different environments MUST be recorded separately. The suite MUST NOT
  combine them into a single aggregate verdict.
- **FR-026g**: The target server's account-scheme policy MUST admit every
  signature scheme the run intends to cover. A server configured to accept only
  one scheme rejects registration for the others, and that rejection is correct
  configuration behaviour. The run MUST detect this before executing scenarios
  and report the excluded schemes as configuration-excluded rather than failed.
- **FR-026e**: The coverage matrix MUST be expressed per network and SDK pair
  and MUST NOT assume every SDK can reach every network. The two SDKs use
  different transports, and a network that serves one may refuse the other.
- **FR-026f**: A network and SDK pair that cannot connect at all MUST be
  declared in the matrix and MUST report environment-blocked, never failed and
  never silently omitted. A run MUST NOT present a matrix as complete when a
  declared pair was unreachable.
- **FR-026d**: A run MUST record the protocol version the target environment
  reports. When that version is incompatible with the pinned SDK line, the run
  MUST report environment-blocked rather than failed, because the two public
  networks are expected to diverge during upgrade windows.

#### Funding

- **FR-027**: The suite MUST read treasury account credentials from CI
  configuration or the local environment, never from a file committed to the
  repository.
- **FR-028**: The suite MUST create ephemeral per-run accounts rather than
  reusing accounts across runs, except for the treasury itself.
- **FR-029**: The suite MUST fund each ephemeral account from the treasury with
  the minimum amount its scenarios require, before that account is first used.
- **FR-030**: The suite MUST check the treasury balance before starting and
  MUST stop with a distinct "treasury underfunded" outcome, naming the observed
  and required amounts, when the balance is insufficient.
- **FR-031**: The suite MUST report the treasury balance at start, the total
  spent, and the projected number of remaining runs the balance supports.
- **FR-032**: The suite MUST detect a zero-fee target chain and record that
  funding was not required, rather than asserting a funding step that cannot
  occur.
- **FR-033**: The suite MUST bound the total value any single run can move out
  of the treasury.
- **FR-033a**: The suite MUST state its policy for residual balances left in
  ephemeral on-chain accounts, which cannot be deleted. Either those balances
  are swept back to the treasury at the end of a run, or the residue is
  accepted as a cost and accounted for in the per-run spend reported by FR-031.
  Silence on this point is not acceptable, because unswept residue is what
  drains the treasury over time.
- **FR-033b**: Treasury mutations MUST be serialized per network, so that two
  concurrent runs cannot interleave balance checks and transfers against the
  same treasury and both proceed on a balance neither will have.
- **FR-033c**: Requests signed with the treasury key MUST be serialized per
  key. Replay protection is enforced per signer, so two runs signing with the
  same key concurrently will be rejected as replays. That rejection is correct
  server behaviour and MUST NOT be reported as a product failure.
- **FR-033d**: The treasury MUST be verified as usable before a run spends
  against it: present, holding the fee asset the target chain names, and
  compatible with the contract version the pinned SDKs expect. A treasury
  account is invalidated by a chain data reset and by an SDK pin bump that
  moves the account contract, so a stale treasury is an expected recurring
  state, not an anomaly.
- **FR-033e**: An unusable treasury MUST report a setup failure naming the
  remediation, distinct from both product failure and scenario failure, and the
  remediation MUST be documented per FR-045.
- **FR-034**: Treasury signing keys MUST be held as protected environment
  secrets and exposed only to the jobs that use them. A repository-wide secret
  readable by every workflow is not acceptable.
- **FR-034a**: The live profile MUST NOT run on any fork-originated event, so
  that treasury credentials are never reachable from a fork.
- **FR-034b**: Each treasury MUST have a named owner responsible for topping it
  up, recorded in the documentation required by FR-045.

#### Diagnostics and reporting

- **FR-035**: On failure the suite MUST capture test output, Guardian logs, and
  the state of the provisioned components, retained as run artifacts.
- **FR-036**: Captured diagnostics MUST be bounded in size and MUST NOT contain
  private keys, treasury credentials, session cookies, or signed payloads.
- **FR-037**: The suite MUST classify each failure as a product failure or an
  external-environment failure, and MUST surface that classification in the run
  result.
- **FR-038**: The suite MUST NOT automatically retry a state-changing
  submission. Read and readiness polling may retry within their deadlines.
- **FR-038a**: Where an SDK's own transport retries submissions internally and
  the suite cannot disable it, the run MUST record that the scenario ran under
  embedded retry. A submission that times out with an unknown outcome MUST be
  resolved by observing chain and Guardian state, never by resubmitting from
  the suite. This applies to the TypeScript SDK, whose bundled client retries
  below the level this project controls.
- **FR-039**: A failing scheduled run MUST produce a persistent notification
  that names the failing scenarios.
- **FR-040**: A pre-release or post-publication run's result MUST be recorded
  against the exact artifact it tested, identified by ref and by resolved image
  digest, including the target network, the pairing used, and per-scenario
  outcomes. The result MUST be discoverable from the release itself rather than
  only from run logs that expire.
- **FR-041**: A failed live run MUST NOT block publication. It MUST be recorded
  against the ref as a failure, and the release decision MUST be made by a
  person.
- **FR-041a**: When a release proceeds over a live run that was not fully
  green, the release record MUST name the person who decided to proceed and
  list which scenarios were failed, skipped, or environment-blocked at that
  moment. The decision MUST be recorded on a durable surface attached to the
  release, not left implicit in the act of publishing.
- **FR-041b**: The suite MUST NOT downgrade a product failure to an
  environment-blocked outcome in order to present a cleaner result. The
  classification in FR-037 MUST reflect the observed evidence.
- **FR-041c**: A decision to publish over a failed run MUST NOT alter the
  qualification result. The recorded outcome stands as it was observed;
  permission to publish is a separate fact recorded alongside it, never an
  amendment to it.

#### Operator surface

- **FR-042**: The operator scenarios MUST cover authentication with a
  deterministic operator key, session establishment and inspection, account
  listing and detail, denial for a missing permission, allowlist change without
  a restart, logout invalidating the session, and the presence of the expected
  audit records in the database.

#### Workflow hygiene and documentation

- **FR-043**: CI workflows introduced by this feature MUST pin third-party
  actions to immutable revisions and MUST request the minimum permissions they
  need.
- **FR-044**: Secrets MUST be exposed only to the jobs that require them.
- **FR-045**: Documentation MUST cover running each profile locally, running it
  in CI, interpreting each outcome class, and topping up the treasury.

### Key Entities

- **Scenario**: A named, independently runnable end-to-end flow. Carries a
  stable identifier, the dimensions it covers, and its expected outcome.
- **Execution profile**: Either deterministic (no external chain) or live
  (real Miden transactions). Determines where and how often a scenario runs.
- **Image source**: Either built from a named ref or pulled from the registry
  and resolved to a digest. Determines how server identity is asserted and
  which artifact the result speaks for.
- **Pairing**: The rule binding image source to SDK source. Branch pairing uses
  one ref for both; release pairing uses a published digest with the SDKs from
  its matching tag; published pairing uses a published digest with the SDKs
  installed from their registries.
- **Artifact set**: The exact things a run qualified: image digest, installed
  SDK versions and their integrity hashes, and the resolved Miden dependency
  versions. Recorded with every result so a result can be traced to what it
  spoke for.
- **Qualification run**: One execution of a selected set of scenarios against
  one artifact and one target network, under one pairing. Produces per-scenario
  outcomes, a run conclusion derived from them, the resolved image digest, a
  funding summary, and, on failure, diagnostics.
- **Target network**: The external Miden network a live run drives, devnet or
  testnet, identified in the run result along with the protocol version it
  reported.
- **Treasury account**: One long-lived, externally funded account per target
  network, whose credentials are held as a protected CI secret and which funds
  that network's ephemeral run accounts.
- **Run account**: An ephemeral account created for one run, funded from the
  treasury, never reused.
- **Run result**: The recorded outcome of a run, attributable to a ref, with
  per-scenario pass, fail, skip, or environment-blocked status.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer can reproduce any failure reported by a qualification
  run on their own machine with one documented command and no manual account,
  database, or funding setup.
- **SC-002**: The deterministic profile completes in under 15 minutes on a
  standard CI runner, making it viable as a required check on pull requests.
- **SC-003**: Every flow the team currently verifies by hand before a release
  is either covered by a named scenario or listed as an explicit, reasoned gap.
  No flow is silently uncovered.
- **SC-004**: A regression in any covered flow is detected and attributed to a
  named scenario within 24 hours of the change reaching the default branch.
- **SC-005**: No release is published without a recorded qualification result
  for the exact ref being released, on each target network. Where the result
  was not fully green, the release record names who decided to proceed.
- **SC-006**: Every scheduled-run failure carries a product or environment
  label, and over any rolling 30-day window, measured per network, fewer than
  10% of runs labelled a product failure turn out on investigation to be a
  network or test defect. The measure is misclassification, not raw failure
  count: on shared public networks some environment failures are expected, and
  the property that matters is that the team can trust the label.
- **SC-007**: No run, including cancelled ones, leaves behind containers,
  volumes, databases, or working directories requiring manual cleanup. On-chain
  accounts are exempt because they cannot be deleted; their residual balances
  are governed by FR-033a and appear in the per-run spend report.
- **SC-008**: Treasury depletion is visible at least 10 runs before it would
  block a run.
- **SC-009**: No private key, treasury credential, session cookie, or signed
  payload appears in any run's logs or retained artifacts.
- **SC-010**: The manual pre-release verification checklist shrinks to flows
  this suite explicitly does not cover, and reviewers stop recording per-PR
  manual smoke notes for covered flows.
- **SC-011**: The TypeScript scenario set covers every flow the Rust set
  covers. Any asymmetry is a listed gap with a reason, never an accident.
- **SC-012**: No regression reaches a published TypeScript SDK release without
  having first been reported by a named scenario, or else classified afterwards
  as a coverage gap and added to the scenario set.
- **SC-013**: The scheduled full run completes within one off-peak window,
  targeting 3 hours per network, so a failure is actionable the same morning.
  Scenarios that do not share an account run concurrently; the ceiling is the
  shared public networks' proving capacity, not the scenario count.
- **SC-013a**: If the scheduled full run does not fit its window, the response
  is to raise the window or add proving capacity, recorded as such. Quietly
  dropping scenarios from the scheduled set is prohibited: a schedule that
  claims the full set while running less than it is worse than a slow one.
- **SC-014**: Every published server image is qualified against both networks
  within 24 hours of publication, and the result is reachable from the release
  in one step.
- **SC-014a**: A packaging defect in a published SDK, such as missing build
  output or an unsatisfiable dependency range, is caught by a published-pairing
  run rather than by a consumer reporting it.
- **SC-015**: During a Miden upgrade window that leaves one network
  incompatible with the pinned SDK line, the schedule continues to report a
  non-failed conclusion for the compatible network, so the signal stays
  readable rather than being ignored as permanently red.

## Assumptions

- The multisig authentication component pays transaction fees out of the
  account vault on fee-charging chains, so live scenarios need funded accounts.
  The suite treats funding as conditional on the target chain's fee
  configuration rather than assuming a fee is always charged.
- Each network's treasury is topped up manually by a maintainer from that
  network's public faucet. Automating faucet claims is not assumed.
- Funding an account is a full transaction, not a helper call. The repository
  provides no plain-wallet abstraction, so a treasury is either an account of
  the same guarded kind the suite tests, whose every transfer goes through the
  complete propose, sign, and execute round trip, or an externally created
  account driven outside the SDKs this feature qualifies. The choice belongs to
  the plan; either way the funding step costs a transaction per funded account
  and that cost is part of the run budget.
- A new account can bootstrap from an inbound transfer without a pre-funded
  vault, because the consuming transaction can pay its fee out of the note it
  consumes. Scenario design may rely on that; it is what makes per-run
  ephemeral accounts viable on a fee-charging chain.
- The treasury is long-lived only within one contract pin and one chain
  generation. Re-creating and re-funding it is expected maintenance, not an
  incident.
- Devnet and testnet are expected to run different protocol lines during Miden
  upgrade windows, so the pinned SDK line will periodically match only one of
  them. This is a normal operating state, reported as environment-blocked on
  the mismatched network, not a defect in the suite.
- "Different schemas" in the request means different signature schemes for
  account signers, and different multisig shapes (threshold of total, and
  homogeneous versus mixed signer sets).
- Nightly means at least once per day, on a schedule chosen to avoid peak load
  on the shared external environment.
- Scenario definitions can draw on the flows already exercised by the existing
  example harnesses; those harnesses remain as interactive debugging surfaces
  and are not replaced by this feature.
- The TypeScript SDK's store is not durable across process boundaries in a
  server-side runtime, so a TypeScript scenario is assumed to complete within
  one process. Scenario design must not depend on stopping and resuming a
  TypeScript client, and any requirement to do so needs a separate mechanism
  proven first.
- Wallet-backed signers cannot be driven unattended and are out of the
  scenario set; qualification covers the SDK's own signer implementations.
- The TypeScript SDK supports more than one consumer runtime. Its package
  declares a server-side engine requirement, and its underlying Miden
  dependency ships a dedicated server-side entry point alongside the browser
  one, so a server-side canary is viable and cheaper. The browser remains the
  runtime where bundling, WASM loading, and browser storage failures appear, so
  it is covered too, at a smaller scenario count. FR-023d treats runtime as a
  dimension rather than mandating one.
- The TypeScript SDK's bundled Miden client retries submissions internally and
  that behaviour cannot be disabled from this project. FR-038a exists because
  of it, not as a general exception.
- Both multisig SDKs remain pinned to the same Miden dependency line, as the
  repository's versioning policy already requires. The suite asserts this
  rather than reconciling a mismatch.
- The deterministic profile builds the server image from the commit under test.
  Qualifying an already-published image is a distinct mode, not the same run
  with a different ref: the published artifact is produced by a separate
  pipeline, exists only for releases and manual dispatches, and must be
  addressed by digest.
- The published `latest` tag moves only on a full release, not on a
  pre-release, so it can lag the default branch by weeks. Any statement about
  "the image operators run" refers to that artifact, not to the default
  branch's current state.
- The published image carries its source revision in its own metadata, which is
  what makes identity assertion possible in pulled-from-registry mode.
- The operator surface is deferred behind the Miden flows at the requester's
  direction, not because it is low value.

## Delivery Sequencing

Story priority above is a ranking of risk, not a build order. The two live
canaries carry the most value at risk and are therefore P1, but they depend on
a treasury, a funded-account lifecycle, an unattended TypeScript runner, and
two networks, so they take the longest to land.

The deterministic profile (User Story 5) has none of those dependencies and is
the fastest route to a required, green, merge-blocking check that proves the
assembled system. It should be the first delivery slice. Suggested order:

1. User Story 5, deterministic profile, built from the ref under test, plus the
   User Story 4 local command that runs it.
2. User Story 3, treasury and funded-account lifecycle, against one network.
3. User Story 1, TypeScript live canary, core scenario subset, one network.
4. User Story 2, Rust live canary, then the second network for both.
5. User Stories 6 and 7, parity assertions, cross-SDK handoffs, and the offline
   and migration flows.
5a. The published pairing, once the release pairing works, since it is the same
   run with a different install step and a skew check.
6. User Stories 8 and 9, operator surface and the release record.

Delivering in this order means a required green check exists well before the
first funded canary does.

## Dependencies

- A server image buildable from any commit with database-backed storage
  enabled, since the published image requires an external database and offers
  no filesystem fallback.
- Availability of the public devnet and testnet, including transaction proving
  capacity sufficient for the scenario count. Proving throughput on the shared
  public provers is a known constraint and bounds how many live scenarios a
  single run can drive.
- CI support for protected environment secrets scoped to individual jobs.
- Availability of the package registries, and published SDK versions that
  correspond to a published server image, for the published pairing to have
  anything to qualify.
- A funded treasury account on each target network, each with a named owner
  responsible for topping it up.
- Existing server contract surfaces (per-account HTTP and gRPC APIs, operator
  dashboard API) remaining stable enough that scenario definitions do not
  require contract changes.

## Out of Scope

- Automated coverage of the EVM proposal surface.
- Qualifying downstream consumer projects. The published SDK packages
  themselves are in scope, via the published pairing; third-party projects
  that depend on them are not.
- Redesigning the release process so publication is gated on an exact qualified
  commit. This feature records the result; it does not restructure publishing.
- Load, soak, or production performance testing. The existing benchmark harness
  owns that.
- Replacing the interactive example harnesses.
- Automating treasury top-ups from a public faucet.
