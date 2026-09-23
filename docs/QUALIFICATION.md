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
[What a run does not prove](#what-a-run-does-not-prove).

This page covers, in order: setting up and
[running](#running-locally) the suite, [reading a result](#reading-the-outcome),
[adding a scenario](#adding-a-scenario), the [treasury](#treasury) a live run
spends from, and the limits above.

## Setting up a local environment

| Tool | Why | Notes |
|---|---|---|
| Rust `1.98.1` | builds the Rust driver | pinned in `rust-toolchain.toml`, so `rustup` selects it for you |
| `protoc` | the driver's build tree includes `tonic-build` | a build failure mentioning protoc means this is missing |
| Docker, daemon running | the stack: server, Postgres, RPC stub, migration target, scheme-gated server | Docker Desktop on macOS is supported; its bind mount can serve a torn view of a file rewritten in place, so the allowlist-reload scenario swaps the file with an atomic rename instead |
| Node 18 or newer, `npm` | the TypeScript leg | |
| `python3` | the shell harness parses JSON with it | any 3.x |
| `curl` | health waits | |

`libpq` is not needed locally. The driver's dependency tree contains no
`pq-sys`; the qualification workflows install `libpq-dev` alongside `protoc`
anyway, so CI is not evidence either way.

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
| Acknowledgement keys, per server | `ack-keygen` from the built image, into the run's own directory under `qualification/stack/runs/`, mode 0600 |
| The migration target's own identity | a second, separate key directory, because migrating an account to the Guardian it already uses is not a state change |
| A third Guardian restricted to ECDSA | `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa`, so the registration gate is exercised as an operator would configure it rather than only as parsed |
| Operator allowlist | generated from the server fixtures via `qualification-driver operator-keys`, so the identities the scenarios sign with cannot drift from the ones the server accepts |
| Postgres password | random per run |
| Ports | picked per run, so concurrent runs do not collide |
| Everything written per run | one directory per run under `qualification/stack/runs/`, removed with the stack: the generated environment file, both acknowledgement key directories and the operator allowlist, so a second run cannot overwrite what the first one's server is still mounting |

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

### Qualifying an upgrade

Asks whether the image under test boots on a database an older release wrote,
and whether that data is still there once its migrations have run:

```bash
qualification/stack/run.sh --profile deterministic \
  --upgrade-from ghcr.io/openzeppelin/guardian:v0.17.0
```

Note what is being upgraded *to*. `--upgrade-from` sets only the image the
stack boots on; the image under test is still the one built from your checkout
unless `--image-tag` pulls a published one. So the command above seeds a
database with the last release and then runs **this branch's** migrations
against those rows, which is the question worth asking before merging a
migration, not after shipping it.

Pass `--image-tag` as well to ask the other question, whether an already
published release upgrades cleanly:

```bash
qualification/stack/run.sh --profile deterministic \
  --image-tag v0.18.0 --upgrade-from v0.17.0
```

The seed is whatever the scenarios themselves stored through the product's own
API, rather than a hand-written SQL fixture that would have to be kept in step
with a schema it does not own.

The run has two phases, and only the second one is the claim. The seed phase
talks to the older release, so it is recorded under that image's own revision
and digest and written to a `seed/` subdirectory the merge does not read: a
scenario that fails there has found something about the release being seeded
from, not about the image under test, and it should not survive into the report
as though it had. Its outcome is announced and then set aside rather than folded
into the run's verdict, because a release old enough to be worth upgrading from
is old enough to fail scenarios written after it. What proves the seeding
actually happened is the target phase's own durability assertion, which looks
for the rows the seed phase wrote.

It seeds with the scenarios that write those rows and nothing else. Running the
whole set against the older release is not more thorough, it is wrong:
`det-scheme-gate` asserts that an ECDSA-only GUARDIAN *refuses* a Falcon
registration, and registering that account on a release predating the gate left
it already configured, so the target phase's registration returned idempotent
success and the scenario read the gate as broken on an image where it works.

The target phase then runs **both** SDKs against the upgraded server, with
`--post-restart`, so the durability assertion asserts rather than skipping:
without it the remaining scenarios re-register the fixture account, which is
idempotent and would pass just as happily against an empty database, and an
upgrade check that cannot tell a migrated database from a fresh one proves
nothing.

Needs no treasury, so it belongs to the deterministic profile, and is refused
on the live profile, where it would fund every scenario twice. The
`Qualification (deterministic)` workflow takes the same value as its
`upgrade-from` input, and once its `pull_request` trigger is restored it will
also run this by itself whenever a change touches `crates/server/migrations/`.

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

**That delta must be at the proposal's own nonce.** Matching on the commitment
alone asks whether the account is in a state some canonical delta explains, and
an account whose delta was discarded satisfies that just as well: it never
moved, so it still agrees with chain and the *previous* delta still carries that
commitment. Both drivers read the nonce before executing, while the proposal is
still listed, and neither confirms without it: a lookup that fails is exactly
when the unbound comparison would wrongly confirm, so it fails closed. This is
not hypothetical. The check matched on commitment alone until the discard
control was written, and the first thing that control caught was this.

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

## What a run does not prove

A green run is evidence from one profile against one artifact. What it is
entitled to claim is `qualification_claim`; what follows is what no run proves
regardless of that claim.

### On the path to `main`

**Neither profile gates a pull request.** The deterministic workflow is
dispatch-only. The live workflow runs nightly against the
`qualification-devnet` and `qualification-testnet` environments and can be
dispatched, but it cannot follow a pull request until the treasury handoff
exists. So this suite is a manual and nightly instrument, not automatic
regression protection.

**The deterministic profile is not a required check.** Its required scenarios
pass, and an action without a driver implementation fails closed on a required
scenario so a gap can never read as a pass. What is left is that it has not run
green on a Linux runner, and the rule that a gate should be seen to go red on a
real defect before anything depends on it. It has gone red more than once, but
each time because a scenario was added or tightened rather than because it
caught a regression in existing coverage, so whether that clears the bar is a
judgement for whoever owns the gate.

**The upgrade pass decides for itself, but nothing triggers it.** The
deterministic workflow runs it when a change touches
`crates/server/migrations/`, seeding from the latest published release and
upgrading to the branch build, and skips it otherwise. A dispatch has no base
commit to diff against, so that decision is inert until the `pull_request`
trigger in the workflow header is restored, which is the same condition as
making the profile a required check.

**The treasury cannot follow a pull request.** The live workflow refuses any ref
other than the default branch, because the scenario driver would otherwise run
from an unreviewed ref with the treasury key in its environment. Editing the
workflow file is not needed to reach the key; editing the harness that ref
carries would be enough, which is why the harness is always checked out from the
default branch. Lifting it means separating funding from execution: a trusted
step funds ephemeral accounts and hands the driver only those ephemeral keys.
The reviewer opt-in described in the specification depends on that separation
and is **not implemented**; the `core-only` checkbox on a dispatch is not the
same thing.

The environments' own deployment-branch restriction is what actually confines
the treasury. The workflow's guard job inspects the requested `ref` but cannot
constrain which branch the workflow file itself runs from, so anyone with write
access could dispatch from a branch whose edited copy drops the guard. Treat the
guard as defence in depth, never as the control.

### Consumer surfaces

**No run proves browser behaviour, and none installs from the registry.** Every
TypeScript scenario declares `runtime = server-side`: Node, with a WASM alias, a
fake IndexedDB and an HTTP/2 shim. The published SDK's consumers are browsers,
and the manifest schema allows `runtime = browser`, but no scenario uses it. Nor
does any run consume the package as a consumer would, from a tarball outside
this workspace, which is why the `published` pairing is refused rather than
faked. A green TypeScript leg says the driver works against the workspace source
under Node, not that `examples/web` or a wallet still works.

**Consuming the published TypeScript SDK from Node needs two workarounds**, both
recorded in every run's `consumer_findings` so a pass cannot quietly speak for a
consumer who has neither.

The first is module resolution. `@miden-sdk/miden-sdk` exports a native Node
binding under the `node` condition, but that entry omits `FeltArray`,
`NoteAndArgsArray` and `NoteArray`, which `@openzeppelin/miden-multisig-client`
imports at 27 call sites, so a plain `import` fails on `FeltArray is not a
constructor`. The suite aliases the package to its browser WASM build
(`dist/st/index.js`).

The second is transport. The Miden RPC and prover endpoints sit behind a load
balancer whose gRPC target group accepts HTTP/2 only, while Node's built-in
fetch is HTTP/1.1. The balancer answers HTTP 464 with no headers, which the
gRPC-web client reports as `missing content-type header in gRPC response`.
Browsers negotiate HTTP/2 through ALPN and are unaffected.
`tests/qualification/h2Fetch.ts` routes gRPC-web calls over `node:http2`.
Without it the remote prover is unreachable and the SDK falls back to in-WASM
proving at roughly twenty-five times the CPU.

**The TypeScript client does not surface a note its own account sent itself.**
`live-p2ide-timelock-1of1-ecdsa` sends a timelocked note to the account that
sent it, because a timelock is only observable from the recipient's side. The
Rust client lists that note among the account's own, not yet consumable, which
is exactly the pair the scenario asserts. The TypeScript client never does:
neither a status listing nor an availability listing returned it three minutes
after the transaction canonicalized, in a run whose Rust leg passed against the
same GUARDIAN and the same network minutes earlier. Its own output-note record
is committed throughout, so the note is on chain and the client knows it, just
not as something the account holds. The TypeScript leg therefore reads the
landing from the sending side and asks the account only what it can consume,
which keeps the pair intact. Whether a TypeScript consumer can ever consume a
note it sent itself is not answered here, and is worth answering.

**A proposal's own metadata cannot be read back from GUARDIAN through the
TypeScript client that created it.** `syncProposals` does fetch from GUARDIAN,
but it rebuilds each proposal with the local metadata when it has a copy, so a
label GUARDIAN mangled would still read back correctly on the client that
proposed it, and wrongly only on every other client. The Rust client decodes
GUARDIAN's answer either way. `live-custom-proposal-1of1-ecdsa` therefore reads
the wire directly on the TypeScript leg, decoding it through the SDK's own
metadata codec so what it asserts is still what a consumer would see. The
divergence is in the SDKs, not in the suite, and closing it is SDK work.

**A GUARDIAN refusal's code is not reachable from the multisig client.**
`guardian_client::ClientError` exposes it through `guardian_code()`, but by the
time the same refusal surfaces as a `MultisigError` only the gRPC status and the
human-readable message survive. A scenario driving the multisig SDK cannot
assert *which* refusal it received without matching user-facing copy, which is
the fragility that once let `live-below-threshold` accept any error containing
"signature". `live-account-paused-1of1-ecdsa` proves causation structurally
instead. A `guardian_code()` on `MultisigError` would let that scenario, and any
consumer branching on a refusal, be exact.

### Flows and combinations

**Proposal-embedded note recovery (#415) is not covered, and is hard to exercise
at all.** A v2 `consume_notes` proposal carries the serialized notes it consumes,
so a pending proposal doubles as recovery material for a client whose store lost
them. Four attempts all ended the same way: `import_notes_from_proposals` reads
the local store first and answers `AlreadyPresent`, which cannot tell recovery
from never having lost the note.

What was ruled out, so the next attempt need not repeat it: a private note
behaves no differently from a public one, so ordinary sync discovery is not the
explanation; `reset_miden_client` reopens the same `account_dir` rather than
emptying it, whatever its name suggests; and building the recovering client at
its own fresh directory does not help either, which leaves `pull_account` as the
step that puts the record in place, by a route not chased further. Accepting
`AlreadyPresent` would make the scenario pass and prove nothing. This is not
only a testing problem: a consumer confirming their own recovery path hits the
same wall.

**Pausing does not hold against a GUARDIAN rotation.** `det-account-paused`
proves GUARDIAN's own gate refuses a proposal, and
`live-account-paused-1of1-ecdsa` proves a paused account cannot execute on
chain and transacts again once unpaused. Both rely on the same mechanism: the
pause is enforced in `ensure_account_active_metadata`, which guards
`push_delta`, `push_delta_proposal`, `sign_delta_proposal` and
`abandon_candidate`, so an account that needs GUARDIAN's acknowledgement to
execute is stopped by being refused one.

`SwitchGuardian` is the one transaction type that executes without an
acknowledgement, and an offline-created one deliberately contacts the old
GUARDIAN for nothing at all. It therefore touches none of those four calls, so
a paused account can still rotate to a different GUARDIAN. That is arguably
what the offline switch is for, since its purpose is leaving a GUARDIAN that
will not cooperate, but it means a pause is an operational gate rather than a
freeze: an operator who pauses an account should not read it as one that cannot
move. No scenario covers this, deliberately, because asserting the current
behaviour would enshrine an answer that is a product decision rather than a
test one.

**Mixed-scheme accounts.** Both account builders assign one configured scheme to
every signer, so no mixed-scheme account can be constructed. The on-chain
storage layout supports one; closing the gap is separate SDK work.

**Whether accounts created under an earlier contract pin are still drivable.** A
deployed Miden account is immutable and its procedure roots fix at creation, so a
pin bump strands every account created before it and nothing here would notice. A
scenario for it was written and removed: GUARDIAN holds the only full copy of a
private account and the stack tears its database down with the run, so an account
the suite transacts with is unrecoverable once the run ends. Every store that
would fix that (committed snapshot, cached snapshot, persisted database) costs
more than it returns for a property better checked at the moment of a bump. The
checklist that replaces it is in
[MIDEN_COMPATIBILITY.md](./MIDEN_COMPATIBILITY.md#before-bumping-the-miden-pin).

**Scheme coverage is spread, not doubled.** Most flows run on one scheme, split
roughly evenly. The exceptions are the flows where the scheme is encoded into the
advice payload and a scheme-binding defect has already been found: add-signer,
remove-signer, threshold change, the procedure override, off-channel signing, and
GUARDIAN rotation (offline ECDSA, online Falcon) all run on both. Execute runs on
both schemes at 2-of-3; only the 3-of-3 shape is Falcon-only, which adds a
combination rather than a code path.

**Deterministic multisig coverage stops at submission.** GUARDIAN's request path
never calls the chain, so the proposal API is testable without one and
`det-proposal-lifecycle` exercises it from a committed fixture summary. Creating
a proposal means executing a transaction locally against synced chain state, and
executing one means proving and submitting, so both stay in the live profile.
`operator-audit` is specified and unimplemented; it is optional, so it skips.

**TypeScript submission behaviour.** The bundled client retries submissions below
the level this project controls, so a TypeScript scenario cannot evidence that a
submission was sent exactly once. Results record `embedded_retry` accordingly.

**Devnet's retention window.** Devnet serves historical account state for roughly
fifty blocks, so multi-step flows cannot be required there. They run
opportunistically and report environment-blocked when the anchor is pruned.

### Operating the suite

**GUARDIAN migration needs a second deployment.** The stack starts one
(`server-migration-target`, its own acknowledgement identity and database) and
passes its address as `QUAL_GUARDIAN_MIGRATION_GRPC` for the Rust driver and
`QUAL_GUARDIAN_MIGRATION_ENDPOINT` for the TypeScript one. A run driven against
a hand-started server leaves them unset, and the rotation scenarios report
environment-blocked rather than rotating an account to the GUARDIAN it already
uses, which changes nothing on chain.

**Any change under `crates/` rebuilds the server image.** The build context
copies the workspace, so a one-line driver edit costs a full release build before
the stack starts. Iterate against a hand-started server and use the stack to
confirm.

**A single-SDK run produces no merged report.** `--sdk rust` or
`--sdk typescript` is a debugging convenience: the run is filtered, so it claims
no qualification, and the only artifact is that leg's own results file. Reach for
it to reproduce one failure, not to qualify anything.
