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

The first two controls were run on 2026-09-16 and the third on 2026-09-17; all
produced the expected verdict. Reverting restores a passing run, which is the
other half of the check: a control that fails in both directions proves nothing.

### What the third control found

The upgrade control passed when it should have failed. With the database
destroyed between the seed and the swap, `det-restart-durability` still reported
a pass.

The cause was in the scenario, not the upgrade plumbing. Its actions are
`account-register` then `restart-durability`, and the whole scenario runs again
on the second pass, so the register put the account back before the assertion
looked for it. The account was present because the scenario had just recreated
it, not because it had survived.

That also means the restart assertion had never proven what it claimed. A
restart does not lose data, so the re-registration never changed the verdict and
nothing drew attention to it; only destroying the data exposed the mask.

`register` is now a no-op on the second pass, so both the restart and the
upgrade read rather than rewrite. With the fix, the wiped-database control fails
and the ordinary run passes.

### What the first control found

It originally reported `environment_blocked`, not a failure. Nothing compared
the account's on-chain state before and after execution, so an execution that
silently submitted nothing was indistinguishable from a slow network, and an
environment-blocked run does not conclude as failed. A real regression in the
execution path would have been reported as someone else's problem.

Both drivers now read the account nonce before executing and again if the
proposal is still pending at the deadline. An unmoved nonce means nothing
reached the chain, which is a product failure; a moved one means GUARDIAN has
not caught up, which is not.

The same control also exposed the two drivers disagreeing about conclusions.
The TypeScript driver failed its process for *any* non-passing required
scenario, including environment-blocked, which contradicts the rule the Rust
driver follows. It now fails only on `failed`, and the qualification claim,
which a blocked scenario still forfeits, is derived from the merged report
rather than from an exit code.
