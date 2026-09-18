# Qualification suite

Automated end-to-end qualification of the assembled system: the shipped server
image against a real database over real sockets, driven by both base clients
and both multisig SDKs, with the multisig flows executing real transactions on
public Miden networks.

Scope is the Miden system. The EVM surface is out of scope and the operator
surface is still being brought in, so a green result does **not** mean every
Guardian feature works. Every result states what it does not cover.

## Two profiles

| Profile | External dependency | Where it runs today | Intended |
|---|---|---|---|
| `deterministic` | none | Manual dispatch | Required check on every non-documentation pull request |
| `live` | a public Miden network and a funded treasury | Nightly schedule, plus manual dispatch, default branch only | Nightly, pre-release, post-publication, and reviewer opt-in |

The live profile runs nightly. The deterministic profile is not yet a required
check, and neither profile can follow a pull request. See
[Current limits](#current-limits).

## Setting up a local environment

| Tool | Why | Notes |
|---|---|---|
| Rust `1.98.1` | builds the Rust driver | pinned in `rust-toolchain.toml`, so `rustup` selects it for you |
| `protoc` | the driver's build tree includes `tonic-build` | a build failure mentioning protoc means this is missing |
| Docker, daemon running | the stack: server, Postgres, RPC stub, migration target, scheme-gated server | Docker Desktop on macOS is supported; its bind mount can serve a torn view of a replaced file, which the allowlist reload now retries through |
| Node 18 or newer, `npm` | the TypeScript leg | |
| `python3` | the shell harness parses JSON with it | any 3.x |
| `curl` | health waits | |

`libpq` is not needed. The driver's dependency tree contains no `pq-sys`; CI
installs `libpq-dev` for the server jobs, not for this suite.

On macOS:

```bash
brew install protobuf node
# plus Docker Desktop, and let `rustup` honour rust-toolchain.toml
```

Then, once per checkout:

```bash
npm ci --prefix packages                              # links the workspace SDKs
cargo build -p guardian-qualification-driver          # ~first build is slow
```

Verify the setup without starting anything, which is also what CI's
`Qualification Harness` job runs:

```bash
cargo run -p guardian-qualification-driver -- validate   # manifest parses
qualification/stack/tests/harness-test.sh                # shell libraries
npm run typecheck:tests --prefix packages/miden-multisig-client
```

Then the smallest real run, one scenario through the full stack:

```bash
qualification/stack/run.sh --profile deterministic --scenario det-status-identity
```

If that passes, the environment is good and anything that fails afterwards is
about the product or the network rather than your machine.

### What the stack provisions for you

Nothing in this list is a setup step. `run.sh` does all of it per run, into
gitignored directories, and tears it down afterwards:

| Thing | How |
|---|---|
| Acknowledgement keys, per server | `ack-keygen` from the built image, into `qualification/stack/ack-keys/`, mode 0600 |
| The migration target's own identity | a second, separate key directory, because migrating an account to the Guardian it already uses is not a state change |
| A third Guardian restricted to ECDSA | `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa`, so the registration gate is exercised as an operator would configure it rather than only as parsed |
| Operator allowlist | generated from the server fixtures via `qualification-driver operator-keys`, so the identities the scenarios sign with cannot drift from the ones the server accepts |
| Postgres password | random per run |
| Ports | picked per run, so concurrent runs do not collide |

The deterministic profile is the exception worth knowing about: it registers a
**committed fixture account** whose stored state binds one specific guardian
commitment, so a freshly generated identity is rejected with
`403 ... not an authorized signer`. `run.sh` handles this by seeding the
fixture's own key from `crates/server/src/testing/fixtures/keys.json`.

You only touch acknowledgement keys when running a server **by hand**, outside
the stack, to iterate on a scenario. Then point
`GUARDIAN_ACK_FALCON_SECRET_PATH` at a file holding `guardian_secret_key` from
that same fixtures file, or accept that only the live and error-envelope
scenarios will pass. Under the development default the keypair is regenerated
on every boot, which breaks fixture registration and would also make the
restart assertion fail for a configuration reason rather than a durability
defect.

## Running locally

```bash
qualification/stack/run.sh --profile deterministic
```

Needs no chain and no treasury. The command builds the server image from your
checkout, starts it with Postgres and a local stand-in for the chain RPC
endpoint, waits for both ports, runs the scenarios, and tears everything down.

Narrow to one scenario while iterating:

```bash
qualification/stack/run.sh --profile deterministic --scenario det-status-identity
```

A filtered run reports which scenarios passed and does not claim qualification.
That is deliberate.

The live profile additionally needs a treasury for the network you target:

```bash
export QUAL_TREASURY_KEY=<hex secret key>
qualification/stack/run.sh --profile live --network testnet --sdk rust
```

The maintained treasuries live in `qualification/.treasury-secrets.env`
(gitignored, mode 0600), one key per network, so a local run loads the right
one rather than pasting a secret into a shell:

```bash
set -a && . qualification/.treasury-secrets.env && set +a
export QUAL_TREASURY_KEY="${QUAL_TREASURY_KEY_testnet}"
```

Check the balance before a full matrix, which `run.sh` also does as a preflight
and refuses to start on a shortfall:

```bash
cargo run -p guardian-qualification-driver -- treasury-check --network testnet
```

## Reading the outcome

| Exit code | Meaning |
|---|---|
| 0 | Concluded successfully |
| 1 | Product failure |
| 2 | Setup failure (treasury, image identity, scheme policy, pairing) |
| 3 | The environment took every scenario and nothing else ran |
| 4 | Usage error |

**A zero exit does not mean full coverage.** Read `qualification_claim` in the
result:

- `full` means every required matrix entry for that network passed.
- `partial` means something required did not run or did not pass, including
  scenarios blocked by the network.
- `none` means the run was filtered.

A run whose only non-passing scenarios are down to the environment, whether
blocked before they ran or broken under by the network while they ran, concludes
successfully and claims `partial`. This is on purpose: leaving the schedule red
through a Miden upgrade window is how a signal stops being read. Every run
prints what it lost that way, so a green night is still readable.

Every failure carries a `classification`:

- `product` is a defect in this repository.
- `environment` is the network, the prover, or a protocol-version mismatch.
- `setup` is the harness or its configuration.

An `environment` failure does not block the conclusion, so a run that lost
scenarios to the network still concludes successfully and claims `partial`.

### How `environment` is assigned

A live run drives a public Miden network and a remote prover. Neither is under
this repository's control, and both fail in ways that read exactly like a
scenario failing: a connection dropped mid-execution, a prover deadline, a node
that stops answering. Calling those product defects is how a nightly stops being
read.

So on the **live profile only**, a failure whose evidence points at the link is
reclassified `environment`. The rule is the SDK clients' own transient-error
classifier, unchanged: permanent status evidence anywhere (an invalid argument,
a failed precondition) vetoes transient evidence anywhere, and the
transient-wording fallback applies only when nothing carried a status. Both
drivers apply it and both are pinned to
`fixtures/qualification/environment-classification.json`, which holds the
verbatim reasons from real runs on both sides of the line. Add a vector there
when a new wording shows up; both drivers pick it up.

The deterministic profile is deliberately exempt. It gates pull requests against
a stack this repository brings up itself, so a failure there is the product's
whatever its wording, and softening it would cost the one gate that has to stay
hard.

The reclassification reads evidence, not profile: a live scenario that failed on
its own terms, such as a quorum refusing an under-signed proposal, still fails as
`product`. Beyond that, the classification is never softened to present a
cleaner result.

## Scenario manifest

Scenarios and the coverage matrix live in `qualification/manifest/` as data.
Both drivers read them; neither defines them.

`manifest.json` is generated. After editing `scenarios.toml` or `matrix.toml`,
regenerate it:

```bash
cargo run -p guardian-qualification-driver -- export-manifest \
  --out qualification/manifest/manifest.json
```

A test fails if the generated file drifts from its sources. The TypeScript
driver consumes the JSON rather than parsing TOML itself, so the two drivers
cannot disagree about what a scenario means.

### Adding a scenario

First decide which of two jobs you have. A scenario that composes **existing**
actions is data only and needs no code in either driver. A scenario that needs
the drivers to do something new needs an action, which is code in both.

**Composing existing actions.** Add an entry to `scenarios.toml`:

```toml
[[scenario]]
id = "live-remove-signer-2of3-falcon"
title = "A signer is removed and the account reports the smaller set"
profile = "live"                 # live | deterministic
sdk = "both"                     # both | rust | typescript
runtime = "server-side"
scheme = "falcon"                # falcon | ecdsa | n/a
shape = "2-of-3"
mode = "online"                  # online | offline | n/a
actions = ["account-create", "account-register", "asset-transfer",
           "note-consume", "signer-remove", "proposal-sign",
           "proposal-execute", "signer-removed-refused", "signer-set-assert"]
step_budget = "480s"
required = true
core = false
```

Actions run in order, and a scenario claims only the actions it names: passing a
generic lifecycle never implies an action it did not list. Ordering carries
weight beyond convenience. `signer-removed-refused` sits before
`signer-set-assert` deliberately, so the authorization check still runs when the
listing assertion fails.

The valid action names are the `From<String>` arms in
`crates/qualification-driver/src/manifest/mod.rs`. That is the authoritative
list; anything else is rejected at load time rather than skipped:

```
error: scenario `det-status-identity` names action `totally-made-up`,
       which is outside the vocabulary
```

Then regenerate and validate:

```bash
cargo run -p guardian-qualification-driver -- export-manifest \
  --out qualification/manifest/manifest.json
cargo run -p guardian-qualification-driver -- validate
```

**Adding an action.** Six edits across five files. Missing any one fails closed
on a required scenario rather than silently skipping, which is the point, but it
is still cheaper to make all six at once:

| Where | What |
|---|---|
| `crates/qualification-driver/src/manifest/mod.rs` | the `Action` variant |
| the same file | its `From<String>` arm, the wire name |
| `crates/qualification-driver/src/scenario/mod.rs` | the Rust dispatch arm |
| `crates/qualification-driver/src/scenario/{live,account,identity,error_envelope}.rs` | the Rust body, in the file for its family |
| `packages/miden-multisig-client/tests/qualification/runner.ts` | the TypeScript dispatch case |
| `packages/miden-multisig-client/tests/qualification/actions/{live,account,identity,errorEnvelope,operator}.ts` | the TypeScript body |

An action implemented on one SDK only is legitimate when it records a real
capability gap, but it must report a skip naming the gap rather than a pass. Two
such gaps (the Rust SDK could not change a threshold, and could not collect
signatures off-channel) were closed once the suite made them visible, which is
the outcome to aim for.

**Then the matrix.** `required = true` in `scenarios.toml` means a pass is part
of the qualification claim. `matrix.toml` decides where it must hold: list the
scenario under a pair's `excluded_scenarios` when a network cannot support it.
Use that for a structural limitation, such as a flow whose `step_budget`
exceeds devnet's historical window, and not for a network that is merely down.
A network that is down stays available and reports environment-blocked at run
time, so recovery needs no edit here. The validator refuses a scenario whose
budget exceeds a network's window while still required there.

**Then run just that scenario**, which does not need the stack:

```bash
cargo run -p guardian-qualification-driver -- run \
  --profile live --network testnet --sdk rust \
  --scenario live-remove-signer-2of3-falcon \
  --run-id local-$(date +%s) \
  --http-endpoint http://127.0.0.1:3000 \
  --grpc-endpoint http://127.0.0.1:50051 \
  --out /tmp/qual-results/result.json
```

The two drivers reach Guardian over **different transports**, Rust over gRPC
and TypeScript over HTTP, so passing one the other's endpoint fails in a way
that looks like an outage. Once the scenario passes standalone, confirm it
through the stack with `run.sh --scenario <id>`.

**Then break it on purpose.** A new scenario that has only ever passed is not
known to detect anything. Make the behaviour it checks wrong, confirm that
scenario fails by name, and revert. `qualification/README.md` records the
procedure and the controls run so far.

## Treasury

Each network has one long-lived treasury account, held as a protected
environment secret and topped up by a named owner from that network's public
faucet.

`treasury-check` reports the starting balance, what a run is expected to cost,
and how many further runs the remainder supports. Top up when that projection
gets short.

`run.sh` runs this projection before a live run starts and refuses the run when
the treasury cannot cover it, so a shortfall costs nothing rather than being
discovered mid-scenario. A live run's result reports what it spent; the opening
balance and the projection stay with `treasury-check`, which runs in its own
process.

The treasury needs **re-creating**, not just topping up, after:

- a chain data reset, or
- an SDK contract pin bump that moves the account contract.

Deployed accounts are immutable and their procedure roots fix at creation, so a
pin bump invalidates the existing treasury. This is expected recurring
maintenance, not an incident. A run against an unusable treasury exits 2 and
names the remediation rather than failing partway through a scenario.

## Treasury commands

| Command | Purpose |
|---|---|
| `treasury-new --network <n>` | Create a key, print the address to fund, emit the secret on stdout |
| `treasury-address --network <n>` | Print the configured treasury's address, for topping up |
| `treasury-status --network <n>` | What the chain shows: deployment, vault, waiting notes, transactions |
| `treasury-bootstrap --network <n>` | Consume waiting notes, which deploys the account and funds its vault |
| `treasury-check --network <n>` | Preflight: lock, fee model, usability, depletion projection |
| `fund --network <n> --recipient <id> --amount <n>` | Fund one account; both drivers call this |
| `treasury-sweep --network <n>` | Move funds out of a superseded private treasury |

The treasury is a public single-signature account, so its state is served by the
node and reconstructible from the key alone. That is what makes a fresh CI
runner viable: a private account's full state lives only in the local store, and
a runner without it would build the next transaction on a stale nonce.

A note sent to the address is not a balance. The account does not exist on chain
until it transacts, and `treasury-bootstrap` is that first transaction, paying
its own fee out of the note it consumes.

Residue left in ephemeral accounts is accepted and charged to the run rather
than swept, because a sweep transaction usually costs more than the dust it
recovers.

### Creating one from zero

Needed for a new network, after a chain data reset, or after a contract pin
bump. The maintained testnet treasury is already funded, so this is not part of
setting up a machine.

```bash
cargo run -p guardian-qualification-driver -- treasury-new --network testnet
```

That prints the secret **once**, and the address to fund. Save the secret to
`qualification/.treasury-secrets.env` as `QUAL_TREASURY_KEY_<network>` before
doing anything else: it is not recoverable, and that file is gitignored at mode
0600.

Then send funds to the printed address from the network's public faucet
(testnet: <https://faucet.testnet.miden.io/>) and deploy:

```bash
export QUAL_TREASURY_KEY="${QUAL_TREASURY_KEY_testnet}"
cargo run -p guardian-qualification-driver -- treasury-status    --network testnet
cargo run -p guardian-qualification-driver -- treasury-bootstrap --network testnet
cargo run -p guardian-qualification-driver -- treasury-check     --network testnet
```

`treasury-status` should show the faucet note waiting before you bootstrap, and
`treasury-check` afterwards reports the balance and how many runs it supports.

Individual test accounts need no setup at all. Each scenario funds its own
ephemeral accounts from the treasury through `fund`.

## Known coverage gaps

- **Account pausing on chain.** `det-account-paused` is required and does drive
  a paused account through an SDK: it pauses through the operator API, confirms
  the proposal is refused with `GUARDIAN_ACCOUNT_PAUSED`, and unpauses. That is
  GUARDIAN's enforcement. What is still unproven is a paused account on a live
  network, where the refusal would have to hold against the chain rather than
  against the server's own gate.
- **A GUARDIAN refusal's code is not reachable from the multisig client.**
  GUARDIAN answers `GUARDIAN_ACCOUNT_PAUSED`, and `guardian_client::ClientError`
  exposes it through `guardian_code()`, but by the time the same refusal
  surfaces as a `MultisigError` only the gRPC status and the human-readable
  message survive. A scenario driving the multisig SDK therefore cannot assert
  *which* refusal it received without matching user-facing copy, which is
  exactly the fragility that let `live-below-threshold` once accept any error
  containing "signature". `live-account-paused-1of1-ecdsa` works around it by
  proving causation structurally instead, refusing while paused and executing
  the same proposal once unpaused, and treats the wording as a sanity check
  rather than as the evidence. A `guardian_code()` on `MultisigError` would let
  that scenario, and any consumer branching on a refusal, be exact.

- **Scheme coverage is spread, not doubled.** Each flow runs on one scheme, with
  the set split roughly evenly. The exceptions are the flows where the scheme is
  encoded into the advice payload and a scheme-binding defect has already been
  found: add-signer and remove-signer run on both, and GUARDIAN rotation now
  runs on both (ECDSA offline, Falcon online). Threshold change and the
  procedure override still run on one scheme each.
- **Mixed-scheme accounts.** Both account builders assign one configured scheme
  to every signer, so no mixed-scheme account can be constructed. The on-chain
  storage layout supports one; closing the gap is separate SDK work.
- **TypeScript submission behaviour.** The bundled client retries submissions
  below the level this project controls, so a TypeScript scenario cannot
  evidence that a submission was sent exactly once. Results record
  `embedded_retry` accordingly.
- **Devnet retention window.** Devnet serves historical account state for a
  short window, so multi-step flows cannot be required there. They run
  opportunistically and report environment-blocked when the anchor is pruned.
- **The completion rule has never been falsified.** Completion is asserted as
  chain confirmation plus a canonical delta, precisely because a discarded delta
  also leaves the pending set. `det-discarded-delta-hidden` is the negative
  control for that rule: it is specified, optional and **unimplemented**, so the
  assertion is correct by construction and unproven by experiment. A regression
  that read a discard as a success would not fail any required scenario. This is
  the highest-value missing test in the suite, and it is harder to write than it
  looks, so the dead ends are recorded here rather than rediscovered:

  - Every route to a `Discarded` delta runs through the canonicalization worker.
    The at-base route needs a chain read the deterministic profile's RPC stub
    cannot serve. The retry-exhaustion route needs no chain evidence but is
    gated first by `submission_grace_period_seconds` (600s) and then by 18
    retries at 10s, so about thirteen minutes, and none of those three knobs is
    exposed through `GUARDIAN_CANONICALIZATION_*`.
  - Competing executions cannot produce the divergence that would shortcut the
    quarantine: `push_delta` refuses a stale base with `CommitmentMismatch` and
    allows only one candidate per account, so GUARDIAN never holds a candidate
    the chain has moved past unless something advanced the account without
    telling it.
  - Re-pushing a proposal's `tx_summary` as a delta does not work either, even
    though `push_delta_proposal` verifies the same summary against the same
    stored state moments earlier: GUARDIAN answers `invalid_delta`. So a
    candidate that never lands cannot currently be built from a proposal, and
    the only supported way to create one is to execute, which lands it.

  Closing this therefore needs a deliberate change rather than another scenario:
  either those canonicalization timings exposed to the environment, or a
  supported way to obtain an acknowledged delta without submitting it. The
  attempt itself is kept on the `spike/discarded-delta-control` branch, which
  runs and fails, so the next attempt can start from the code rather than from
  this list.
- **Deterministic multisig coverage stops at submission.** GUARDIAN's request
  path never calls the chain, so the proposal API is testable without one and
  `det-proposal-lifecycle` exercises it from a committed fixture summary.
  Creating a proposal means executing a transaction locally against synced
  chain state, and executing one means proving and submitting, so both stay in
  the live profile.

**A single-SDK run produces no merged report.** `--sdk rust` or
`--sdk typescript` is a debugging convenience: the run is filtered, so it claims
no qualification, and the only artifact is that leg's own results file. Reach for
it to reproduce one failure, not to qualify anything.

## Why completion is asserted the way it is

A proposal leaves GUARDIAN's pending set for two opposite reasons: because
canonicalization **applied** its delta, or because it **gave up** on it, logged
as `Deleting matching proposal as its delta left the candidate path`. The two
are indistinguishable from the client side, so a discarded delta reads exactly
like a successful execution. Absence from the pending set is therefore a
precondition, never the proof.

Both drivers assert completion directly instead: chain confirmation and
commitment agreement through `verify_state_commitment` / `verifyStateCommitment`,
plus a canonical delta in `delta_history` / `deltaHistory` carrying that
commitment. A proposal that vanished without a canonical delta is a product
failure, not a pass.

Two things found while writing that assertion, both easy to trip over again:

- the pending listing is the TypeScript client's own cache, and an executed
  proposal stays in it marked `finalized`. Presence alone is not pending; the
  status decides.
- a GUARDIAN migration repoints the client at the GUARDIAN it moved to, which
  has no history for an account it was just handed. Completion there is chain
  agreement plus the new GUARDIAN serving the account.

Separately, GUARDIAN's own view lags briefly after a change lands. Proposing
again immediately is refused with `There's already a pending change for this
account`, and a signer admitted by an executed add-signer is refused until
`authorized_count` catches up. Both are races only a fast client hits: the Rust
driver hit them where the TypeScript driver, about four times slower, did not.
Both drivers poll against bounded deadlines rather than racing.

## Current limits

**A green run is not a release qualification.** It is evidence from one profile
against one artifact. What it is entitled to claim is `qualification_claim`, and
the limits below are what no run currently proves regardless of that claim.

**Neither profile gates a pull request.** The deterministic workflow is
dispatch-only. The live workflow runs on a nightly schedule against the
`qualification-devnet` and `qualification-testnet` environments and can also be
dispatched, but it cannot follow a pull request until the treasury handoff
exists. So this suite is a manual and nightly instrument today, not automatic
regression protection on the path to `main`.

**The deterministic profile is not a required check.** Its required scenarios
are implemented and pass; two optional ones are not (`discarded-delta-hidden`
and `operator-audit`), and actions without a driver implementation fail closed on
a required scenario so a gap can never read as a pass. The allowlist-reload
failure that previously blocked this is fixed. What is left is the rule that a
gate should be seen to go red on a real defect before anything depends on it. It
has now done that twice: the allowlist reload, and `det-proposal-lifecycle`,
which failed on its first run because the proposal fixture predated the metadata
requirement and the driver was sending the wrong object. Whether that clears the
bar is a judgement for whoever owns the gate, since both were found by adding the
scenario rather than by catching a regression in existing coverage.

**Whether accounts created under an earlier contract pin are still drivable is
not tested.** A deployed Miden account is immutable and its procedure roots fix
at creation, so a contract pin bump strands every account created before it, and
nothing else in this suite would notice. A scenario for it (`live-heritage-account`)
was written and then removed, because it cannot work against an ephemeral stack:
Guardian holds the only full copy of a private account, and the stack tears its
database down with the run. An account the suite transacts with is therefore
unrecoverable once the run ends, so no long-lived account can survive between
runs without a store outside the run. Every option for that store (a committed
snapshot, a cached snapshot, a persisted database) was judged to cost more than
it returns while the property is better checked at the moment of a pin bump.
The checklist that replaces it is in
[MIDEN_COMPATIBILITY.md](./MIDEN_COMPATIBILITY.md#before-bumping-the-miden-pin).

**No run proves browser behaviour, and none installs from the registry.** Every
TypeScript scenario declares `runtime = server-side`: Node, with a WASM alias, a
fake IndexedDB and an HTTP/2 shim. The published SDK's consumers are browsers,
and the manifest schema allows `runtime = browser`, but no scenario uses it. Nor
does any run consume the package as a consumer would, from a tarball outside
this workspace, which is why the `published` pairing is refused rather than
faked. A green TypeScript leg therefore says the driver works against the
workspace source under Node, not that `examples/web` or a wallet still works.
The workarounds below are recorded in every run's `consumer_findings` so a pass
cannot quietly speak for a consumer who has neither.

**Consuming the published TypeScript SDK from Node needs two workarounds.**
Both are carried by this suite and both apply to any Node consumer, so they are
recorded as findings against the published artifact rather than as harness
quirks.

The first is module resolution. `@miden-sdk/miden-sdk` exports a native Node
binding under the `node` condition, but that entry omits `FeltArray`,
`NoteAndArgsArray` and `NoteArray`, which `@openzeppelin/miden-multisig-client`
imports at 27 call sites. A plain `import` from Node therefore fails on
`FeltArray is not a constructor`. The suite aliases the package to its
browser WASM build (`dist/st/index.js`) instead.

The second is transport. The Miden RPC and prover endpoints sit behind a load
balancer whose gRPC target group accepts HTTP/2 only, while Node's built-in
fetch is HTTP/1.1. The balancer answers HTTP 464 with no headers, which the
SDK's gRPC-web client reports as `missing content-type header in gRPC
response`. Browsers are unaffected because they negotiate HTTP/2 through ALPN.
`tests/qualification/h2Fetch.ts` routes gRPC-web calls over `node:http2` to
work around it. Without that shim the remote prover is unreachable and the SDK
falls back to in-WASM proving, which costs roughly twenty-five times the CPU: a
2-of-3 lifecycle takes about two minutes instead of under thirty seconds.

**GUARDIAN migration needs a second deployment.** The stack starts one
(`server-migration-target`, its own acknowledgement identity and its own
database) and passes its address as `QUAL_GUARDIAN_MIGRATION_ENDPOINT`. Runs
driven without the stack, against a hand-started server, leave it unset and the
migration scenario reports environment-blocked rather than migrating an account
to the GUARDIAN it already uses, which changes nothing on chain and gives the
transaction no state change to commit.

**The deterministic profile has run against a real Docker daemon, and is still
not a required check.** The stack provisions: the image builds, Postgres and
both GUARDIANs come up, readiness passes on all four ports, the Rust
deterministic scenarios pass including the post-restart durability assertion,
artifacts are redacted and scanned, and teardown is clean. It stays
dispatch-only until it has been seen to go red on a real regression; a gate that
has never failed is not known to be a gate.

One defect surfaced on that first real run and is now fixed: the operator
allowlist scenario rewrote a file the server re-reads on every request without
an atomic rename, so the server parsed a half-written file. The same run also
saw the image build stall fetching crates, which looked like cargo multiplexing
every download onto one HTTP/2 connection through the VM NAT. That did not
reproduce: two cold `cargo fetch --locked` runs of the whole workspace succeed
with multiplexing on, and disabling it is measurably slower, so it is recorded
here as a transient network failure rather than carried as a build setting.

`det-operator-allowlist-reload` failed there at first, and the host turned out
to be only half the cause: Docker Desktop's bind mount serves a torn view of a
replaced file, but GUARDIAN answered that transient read with a 500 and no
retry. The allowlist load now retries within a bounded budget, so the scenario
passes and an operator editing the file in place no longer takes the dashboard
down.

**The live profile has run through the stack** against testnet on both SDKs,
including the treasury preflight, funding, the full proposal lifecycle, both
cross-SDK handoffs and GUARDIAN migration. The outcomes, and which scenarios
still fail, are recorded in the feature's `tasks.md`.

**Any change under `crates/` rebuilds the server image.** The build context
copies the workspace, so a one-line driver edit costs a full release build
before the stack starts. Iterate against a hand-started server and use the stack
to confirm.

**The TypeScript `load()` returns stale membership.** For five reproductions
`live-remove-signer-2of3-falcon` failed on TypeScript and passed on Rust, and
the suite reported it as GUARDIAN serving a pre-removal signer set. Measuring
both sources on every poll showed GUARDIAN correct from the first poll: the
stale value came from the reader's local store, because `load()` keeps an
existing store record rather than overwriting it with what GUARDIAN returned,
while reads go through the store. The assertion now reads what GUARDIAN
returned and the scenario passes on both SDKs. The SDK defect behind it is also
fixed: `load()` now reconciles with the store the way `syncState()` already did.

**The Rust SDK could not change a threshold, and now can.** The transaction
builder rejected `TransactionType::UpdateSigners` outright, redirecting callers
to `AddCosigner` or `RemoveCosigner`, which both pin the threshold and so could
not serve the request. Every other layer already handled the variant, so only
creation was blocked. It now has a builder arm that moves the threshold and
refuses a membership change, and the Rust leg of
`live-change-threshold-2of3-ecdsa` passes.

**Offline signing was an SDK divergence, and is fixed.** The Rust SDK tied
offline signing to offline execution (`supports_offline_execution` is true only
for `SwitchGuardian`), so it refused to collect signatures off-channel for any
proposal needing a GUARDIAN acknowledgement at execution, while TypeScript
signed these offline and contacted GUARDIAN only to execute. The gate is gone
from `sign_imported_proposal`, and execution fetches the acknowledgement when
the transaction type requires one, so the Rust leg of
`live-offline-export-import-2of3-falcon` now runs rather than skipping.

**The treasury cannot yet follow a pull request.** The live workflow refuses any
ref other than the default branch, because the scenario driver would otherwise
run from an unreviewed ref with the treasury key in its environment. Editing the
workflow file is not needed to reach the key; editing the harness that ref
carries would be enough, which is why the harness is always checked out from the
default branch.

To lift that restriction, funding has to be separated from scenario execution: a
trusted step funds ephemeral accounts from the treasury and hands the scenario
driver only those ephemeral keys. The reviewer opt-in path described in the
specification depends on that separation.

**Before arming the treasury secret**, lock the deployment branches of the
`qualification-devnet` and `qualification-testnet` environments to the default
branch. The workflow's guard job inspects the requested `ref` but cannot
constrain which branch the workflow file itself is run from: anyone with write
access can dispatch from a branch whose edited copy of the workflow drops the
guard, and the job would still enter the environment. Only the environment's own
branch restriction closes that. Treat the guard job as defence in depth, never
as the control.

The schedule is also deliberately absent until the per-network environments
exist, an environment-blocked run stops failing the job, and a failure
notification is wired.

The reviewer opt-in on a pull request is specified but **not implemented**. The
`core-only` checkbox on a dispatch against the default branch is not the same
thing, and should not be recorded as satisfying it.
