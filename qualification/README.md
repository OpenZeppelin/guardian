# Qualification suite

Automated end-to-end qualification of the assembled Guardian system: the
shipped server image against a real database over real sockets, driven by both
base clients and both multisig SDKs, with the multisig flows executing real
transactions on public Miden networks.

Start with [`docs/QUALIFICATION.md`](../docs/QUALIFICATION.md): what the suite
covers, how to set up an environment, how to run each profile, how to read a
result, and how to add a scenario.

## Layout

| Path | Contents |
|---|---|
| `manifest/` | Scenario definitions and the coverage matrix, as data. Both drivers read these; neither defines them. |
| `stack/` | Stack provisioning: compose files, the RPC stand-in, shell libraries, and `run.sh`. |
| `report/` | The run result JSON schema. |

The Rust driver lives in `crates/qualification-driver`. The TypeScript driver
lives in `packages/miden-multisig-client/tests/qualification`.

## Running

```bash
qualification/stack/run.sh --profile deterministic
```

See the quickstart for the live profile, treasury configuration, and how to
read the outcome classes.

## Negative controls

A suite that has never failed is not known to detect anything. These are the
checks that a deliberate defect produces a *named* failure rather than a pass,
an ambiguous error, or an outcome class that does not fail a run.

They are run by hand, against testnet, by editing the driver, running one
scenario, and reverting. They are not permanent code: a fault-injection switch
that ships with the driver is a switch that can be left on.

| Defect | Where | Expected verdict |
|---|---|---|
| Execution submits nothing | drop the `executeProposal` call in `tests/qualification/actions/live.ts` | `failed` / `product`, naming the unmoved account nonce |
| Signatures are claimed but never collected | in `collectSignatures`, increment the counter without calling `signProposal` | `failed` / `product`, naming the unmet threshold |
| Stored data does not survive an upgrade | drop the database between the seed and the swap in `run.sh --upgrade-from` | `failed` / `product`, naming the unreadable account |

```bash
cd packages/miden-multisig-client
QUAL_PROFILE=live QUAL_NETWORK=testnet \
QUAL_SCENARIOS=live-execute-2of3-falcon \
QUAL_HTTP_ENDPOINT=http://127.0.0.1:3000 \
QUAL_GRPC_ENDPOINT=http://127.0.0.1:50051 \
QUAL_OUT_DIR=/tmp/nc QUAL_RUN_ID=nc-1 \
  npx vitest run --config vitest.qualification.config.ts
```

Run a control, confirm the named verdict, then revert and confirm the run
passes again. A control that fails in both directions proves nothing.

Two of these controls found real defects when they were first run, and both
fixes carry their reasoning at the code: `account-register` is a no-op on a
post-restart pass, in `scenario/account.rs`, because a scenario that rewrites
the data its assertion reads proves nothing; and both drivers compare the
account nonce across an execution, because without it an execution that
submitted nothing was indistinguishable from a slow network and reported
`environment_blocked`.
