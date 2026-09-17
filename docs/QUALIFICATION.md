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
| `live` | a public Miden network and a funded treasury | Manual dispatch, default branch only | Nightly, pre-release, post-publication, and reviewer opt-in |

Neither workflow is wired to its intended triggers yet. See
[Current limits](#current-limits).

## Setting up a local environment

| Tool | Why | Notes |
|---|---|---|
| Rust `1.98.1` | builds the Rust driver | pinned in `rust-toolchain.toml`, so `rustup` selects it for you |
| `protoc` | the driver's build tree includes `tonic-build` | a build failure mentioning protoc means this is missing |
| Docker, daemon running | the stack: server, Postgres, RPC stub, migration target | Docker Desktop on macOS is supported; its bind-mount behaviour is what F12 covers, now fixed on the server side |
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
| 3 | Environment blocked and nothing else ran |
| 4 | Usage error |

**A zero exit does not mean full coverage.** Read `qualification_claim` in the
result:

- `full` means every required matrix entry for that network passed.
- `partial` means something required did not run or did not pass, including
  scenarios blocked by the network.
- `none` means the run was filtered.

A run whose only non-passing scenarios are environment-blocked concludes
successfully and claims `partial`. This is on purpose: leaving the schedule red
through a Miden upgrade window is how a signal stops being read.

Every failure carries a `classification`:

- `product` is a defect in this repository.
- `environment` is the network, the prover, or a protocol-version mismatch.
- `setup` is the harness or its configuration.

The classification is never softened to present a cleaner result.

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

## Treasury

Each network has one long-lived treasury account, held as a protected
environment secret and topped up by a named owner from that network's public
faucet.

`treasury-check` reports the starting balance, what a run is expected to cost,
and how many further runs the remainder supports. Top up when that projection
gets short.

A run does **not** yet carry that summary in its own result: `funding_summary`
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

## Known coverage gaps

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

## Findings

Defects, capability gaps and cross-SDK divergences the suite has found are
logged in [QUALIFICATION_FINDINGS.md](./QUALIFICATION_FINDINGS.md), with the
evidence for each and the workarounds the suite carries.

## Current limits

Both workflows are dispatch-only, and the deterministic profile is not a
required check. Three things have to land first.

**The deterministic profile is not a required check.** Its required scenarios
are implemented and pass; two optional ones are not (`discarded-delta-hidden`
and `operator-audit`), and actions without a driver implementation fail closed on
a required scenario so a gap can never read as a pass. F12, previously the
blocker here, is fixed. What is left is the rule that a gate should be seen to
go red on a real defect before anything depends on it. It has now done that
twice: F12, and `det-proposal-lifecycle`, which failed on its first run because
the proposal fixture predated the metadata requirement and the driver was
sending the wrong object. Whether that clears the bar is a judgement for
whoever owns the gate, since both were found by adding the scenario rather than
by catching a regression in existing coverage.

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
down. See F12 in
[QUALIFICATION_FINDINGS.md](./QUALIFICATION_FINDINGS.md).

**The live profile has run through the stack** against testnet on both SDKs,
including the treasury preflight, funding, the full proposal lifecycle, both
cross-SDK handoffs and GUARDIAN migration. The outcomes, and which scenarios
still fail, are recorded in the feature's `tasks.md`.

**Any change under `crates/` rebuilds the server image.** The build context
copies the workspace, so a one-line driver edit costs a full release build
before the stack starts. Iterate against a hand-started server and use the stack
to confirm.

**GUARDIAN never advances past a TypeScript remove-signer.**
`live-remove-signer-2of3-falcon` fails on TypeScript and passes on Rust,
reproducibly. GUARDIAN is left one nonce behind the chain and keeps serving the
pre-removal signer set; the client's local and on-chain commitments agree, so
the removal executed and the client is not stale. GUARDIAN logs nothing at
`warn` level. Full evidence in
[QUALIFICATION_FINDINGS.md](./QUALIFICATION_FINDINGS.md) (F2).

**The Rust SDK cannot change a threshold.** `TransactionType::UpdateSigners` is
a public constructor whose transaction builder rejects it unconditionally with
"Use AddCosigner or RemoveCosigner for signer updates", so there is no Rust path
to a threshold change at all. The on-chain contract supports it
(`update_signers_and_threshold`, exercised by the contract tests) and the
TypeScript SDK drives it through `createChangeThresholdProposal`. The Rust leg
of `live-change-threshold-2of3-ecdsa` reports a skip naming the gap.

**Offline signing is a documented SDK divergence.** The Rust SDK ties offline
signing to offline execution (`supports_offline_execution` is true only for
`SwitchGuardian`), so it refuses to collect signatures off-channel for any
proposal that needs a GUARDIAN acknowledgement at execution. TypeScript signs
these offline and contacts GUARDIAN only to execute. The Rust leg of
`live-offline-export-import-2of3-falcon` reports a skip naming the gap rather
than passing.

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

## Design documents

Specification, plan, research and contracts live in
[`speckit/features/001-system-e2e-qualification/`](../speckit/features/001-system-e2e-qualification/).
