---
name: run-guardian-qualification
description: Set up and run the end-to-end qualification suite in this repository: the deterministic profile against a provisioned stack, and the live profile driving real Miden transactions from a CI-held treasury. Use when Codex needs to run or debug `qualification/stack/run.sh`, add or change a scenario, drive one SDK's live scenarios against testnet, reproduce a qualification failure locally, or interpret a run result's outcome classes and qualification claim.
---

# Run Guardian Qualification

Two profiles. **Deterministic** provisions a stack and drives fixture flows with
no chain; it is fast enough to gate merges. **Live** drives real transactions on
a public Miden network and spends real funds.

Read [`docs/QUALIFICATION.md`](../../../docs/QUALIFICATION.md) for what the suite
covers and what it deliberately does not. This skill is how to run it.

## Pick the smallest thing that answers the question

| Question | Command |
|---|---|
| Does the manifest hold together? | `./target/debug/qualification-driver validate` |
| Does driver logic still work? | `cargo test -p guardian-qualification-driver` |
| Do the shell libs still work? | `qualification/stack/tests/harness-test.sh` |
| Does a scenario body work? | one scenario, one SDK, against a hand-started server |
| Does the assembled system work? | `qualification/stack/run.sh` with Docker |

Do not reach for the full stack to debug a scenario body. It rebuilds an image.

## Build the binary first, always

```bash
cargo build -p guardian-qualification-driver
```

`qualification/stack/run.sh` invokes the driver through `cargo run`, which
builds it if needed. The TypeScript leg's funding bridge does not: it shells out
to `target/debug/qualification-driver` (override with `QUAL_DRIVER_BIN`),
because `cargo run` costs about 3.5s of build-graph checking per call and can
stall on a rebuild in the middle of a chain operation. The commands below use
the built binary directly for the same reason.

## Fast path: one scenario, no Docker

The quickest loop for scenario work. Start a server with `cargo` directly, then
drive one scenario.

Deterministic scenarios need the **fixture** acknowledgement key: the committed
fixture account binds one specific guardian identity, and a server booted with a
generated key rejects it with `403 ... not an authorized signer`. Point
`GUARDIAN_ACK_FALCON_SECRET_PATH` at a file containing `guardian_secret_key`
from `crates/server/src/testing/fixtures/keys.json`, or accept that only the
live and error-envelope scenarios will pass.

```bash
# Rust, one live scenario
export QUAL_TREASURY_KEY="$QUAL_TREASURY_KEY_testnet"   # from qualification/.treasury-secrets.env
./target/debug/qualification-driver run \
  --profile live --network testnet --sdk rust \
  --scenario live-execute-2of3-falcon \
  --run-id local-$(date +%s) \
  --http-endpoint http://127.0.0.1:3000 \
  --grpc-endpoint http://127.0.0.1:50051 \
  --out /tmp/qual-results/result.json
```

```bash
# TypeScript, same scenario
cd packages/miden-multisig-client
QUAL_PROFILE=live QUAL_NETWORK=testnet \
QUAL_SCENARIOS=live-execute-2of3-falcon \
QUAL_HTTP_ENDPOINT=http://127.0.0.1:3000 \
QUAL_GRPC_ENDPOINT=http://127.0.0.1:50051 \
QUAL_OUT_DIR=/tmp/qual-results QUAL_RUN_ID=local-$(date +%s) \
  npx vitest run --config vitest.qualification.config.ts
```

The two drivers reach GUARDIAN over **different transports**: Rust speaks gRPC,
TypeScript speaks HTTP. Passing one the other's endpoint fails in ways that look
like an outage.

## Full stack

```bash
qualification/stack/run.sh --profile deterministic
qualification/stack/run.sh --profile live --network testnet
```

`--keep-stack` leaves the containers up so you can read logs; without it the
stack is torn down even on failure, and diagnostics land in
`<out>/diagnostics/`.

**Any change under `crates/` rebuilds the server image.** The build context
copies the workspace, so a one-line driver edit costs a full release build
before the stack starts. This is the main reason to iterate against a
hand-started server and use the stack only to confirm.

**Never edit `run.sh` while a run is in flight.** Bash reads scripts
incrementally, so an edit mid-run produces a syntax error at a stale byte offset
in a file that passes `bash -n`. Wait for the run, then edit.

The stack starts a **second GUARDIAN** (`server-migration-target`) with its own
acknowledgement identity and its own database, because migrating an account to
the GUARDIAN it already uses is not a state change and the kernel rejects it. It
exports `QUAL_GUARDIAN_MIGRATION_GRPC` and `QUAL_GUARDIAN_MIGRATION_HTTP`; the
migration scenario reports environment-blocked when they are absent.

## Treasury

Live runs need `QUAL_TREASURY_KEY`. Locally it lives in
`qualification/.treasury-secrets.env` (gitignored, mode 0600) with one key per
network.

```bash
set -a && . qualification/.treasury-secrets.env && set +a
export QUAL_TREASURY_KEY="$QUAL_TREASURY_KEY_testnet"
./target/debug/qualification-driver treasury-status --network testnet
```

`treasury-address` prints the address to fund from the faucet;
`treasury-bootstrap` consumes the faucet notes into the vault. The treasury is a
**public** single-signature wallet on purpose: a private one keeps its state only
in the local store, which a fresh CI runner does not have, so the next
transaction would build on a stale nonce.

Do not arm the GitHub secret until the `qualification-{devnet,testnet}`
environments have their deployment branches locked to the default branch.

## Changing scenarios

Scenario sources are data, not code: `qualification/manifest/scenarios.toml` and
`matrix.toml`. After editing either:

```bash
./target/debug/qualification-driver export-manifest --out qualification/manifest/manifest.json
./target/debug/qualification-driver validate
```

A committed `manifest.json` that does not match the sources fails a test. The
validator also refuses a scenario whose step budget exceeds a network's
historical window while required there, so exclude it for that network rather than
shrinking a budget the flow cannot meet.

A new action needs **six edits across five files**, and missing any one fails
closed on a required scenario rather than silently skipping: the `Action`
variant and its `From<String>` arm in `manifest/mod.rs`, the Rust dispatch in
`scenario/mod.rs`, the Rust body in `scenario/{live,account,identity,error_envelope}.rs`,
the TypeScript dispatch in `runner.ts`, and the TypeScript body in
`actions/{live,account,identity,errorEnvelope,operator}.ts`. The body goes in
the file for the action's family, not always `live`.

A scenario that only composes existing actions needs no driver code at all. The
full procedure, including the matrix and the negative control, is in
`docs/QUALIFICATION.md`, "Adding a scenario".

## Reading a result

`conclusion` is the gate. `qualification_claim` is the coverage statement, and
the two are independent: a run can conclude successfully and still claim only
`partial`, because a required scenario that skipped or was environment-blocked
forfeits the claim without failing the run.

| Outcome | Meaning |
|---|---|
| `passed` | the flow completed and was verified |
| `failed` + `product` | the system under test is wrong |
| `failed` + `setup` | the harness or environment is wrong |
| `skipped` | a known capability gap, named in the reason |
| `environment_blocked` | the network or a dependency stopped it |

Exit codes: 0 concluded, 1 product failure, 2 setup failure, 3 environment
blocked and nothing else ran. **A zero exit is not full coverage**, so read the
claim.

The TypeScript leg writes `<run-id>-typescript.json` carrying only `run_id` and
`scenario_results`; the Rust merger folds it into the run and restates the claim
over both SDKs. Merge with `qualification-driver report --results <dir>`.

## Negative controls

A suite that has never failed is not known to detect anything. Before trusting a
green run after changing assertion logic, break the thing deliberately and
confirm a **named** scenario fails. The procedure and the two recorded controls
are in [`qualification/README.md`](../../../qualification/README.md).

Do not ship a fault-injection switch to make this easier. A switch that disables
an assertion is a switch that can be left on.

## Gotchas that cost real time

- **TypeScript needs an HTTP/2 shim** to reach Miden at all from Node, and
  without a reachable remote prover the SDK silently falls back to in-WASM
  proving at roughly twenty-five times the CPU. `tests/qualification/h2Fetch.ts`
  is the workaround.
- **The qualification tests typecheck under their own config.** `tsconfig.json`
  covers only `src/`. Run `npm run typecheck:tests` in
  `packages/miden-multisig-client`, or a missing import ships.
- **A stalled image build is usually the network, not the Dockerfile.** A cargo
  fetch through a VM NAT can stall with zero bytes transferred. Re-run it before
  changing anything; `CARGO_HTTP_MULTIPLEXING=false` in the build environment
  trades roughly a third more fetch time for one connection per download, and is
  worth reaching for only if the stall reproduces.
- **GUARDIAN holds one acknowledgement identity per signature scheme.** Asking
  for the pubkey without a scheme returns the default, and an ECDSA account built
  from it is refused at registration with a message about authorization rather
  than the mismatch.
- **A proposal leaving the pending set is not completion.** A discarded delta is
  removed the same way. Assert through `verify_state_commitment` /
  `verifyStateCommitment` plus canonical delta history, which is what the drivers
  now do.

## Before calling a change done

```bash
cargo test -p guardian-qualification-driver
cargo clippy -p guardian-qualification-driver --all-targets
cargo fmt --all -- --check
qualification/stack/tests/harness-test.sh
cd packages/miden-multisig-client && npm run typecheck:tests && npx vitest run
```

Live scenario changes are not verified by any of the above. Run the affected
scenario on testnet for **both** SDKs, since the two drivers diverge in ways the
type system does not catch.
