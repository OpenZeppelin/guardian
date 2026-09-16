# Quickstart: running the qualification suite

**Feature**: `001-system-e2e-qualification` · Satisfies FR-010, FR-045.

## Deterministic profile, locally

Needs a container runtime. Nothing else.

```bash
qualification/stack/run.sh --profile deterministic
```

This builds the server image from your checkout, starts it with Postgres and a
local stand-in for the chain RPC endpoint, waits for both ports, runs the
fixture flows through the TypeScript HTTP client and the Rust gRPC client,
restarts only the server and re-reads the data, then tears everything down.

Expect around 15 minutes on a cold cache.

One scenario at a time, while iterating:

```bash
qualification/stack/run.sh --profile deterministic --scenario restart-durability
```

A filtered run reports which scenarios passed and does not claim
qualification. That is by design, not a limitation.

## Live profile, locally

Additionally needs a treasury for the network you target.

```bash
export QUAL_TREASURY_KEY_TESTNET=<hex secret key>
qualification/stack/run.sh --profile live --network testnet --sdk rust
```

The run refuses to start if the treasury is missing, underfunded, holding the
wrong fee asset, or built against a contract version the current SDK pin no
longer accepts. Each of those exits 2 with the remediation named, rather than
failing partway through a scenario.

## Reading the outcome

Exit codes carry meaning: `0` concluded successfully, `1` product failure, `2`
setup failure, `3` environment blocked and nothing else ran, `4` usage error.

**A zero exit does not mean full coverage.** Check `qualification_claim` in the
result: `full` means every required matrix entry passed, `partial` means
something required did not run or pass, `none` means the run was filtered. A
run whose only non-passing scenarios were environment-blocked concludes
successfully and claims `partial`, on purpose, so an upgrade window does not
leave the schedule permanently red.

## When the treasury runs low

Every run reports the starting balance, what it spent, and how many further
runs the remainder supports. When that projection gets short, top the treasury
up from the target network's public faucet.

The treasury also needs re-creating, not just topping up, after a chain data
reset or an SDK contract pin bump. Deployed accounts are immutable and their
procedure roots fix at creation, so a pin bump that moves the account contract
invalidates the existing treasury. This is expected recurring maintenance.

## In CI

The same command with the same arguments. The deterministic profile runs on
every non-documentation pull request and blocks merge. The live profile runs on
the nightly schedule (full set, both networks), before a release, after
publication, and on a reviewer's explicit opt-in for a pull request. It never
runs automatically on a pull request and never for a fork.
