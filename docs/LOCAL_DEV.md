# Local Development

How to run Guardian on a developer machine, what choices you have, and which
example to reach for once it's up.

For protocol concepts (State, Delta, Nonce, Commitment) read
[`spec/index.md`](../spec/index.md) first — this guide assumes you know them.
For the deployed AWS topology see
[`docs/architecture/infra.md`](./architecture/infra.md).

## What you're choosing

Three decisions when running Guardian locally:

1. **Storage backend** — filesystem (default) or Postgres. See
   [Storage modes](./architecture/services.md#storage-modes). Filesystem is
   fine for most local work; pick Postgres only if you are testing
   migrations, audit persistence, or multi-replica behavior.
2. **Cargo features** — `postgres` and/or `evm`. Default builds do
   **not** include EVM routes and do **not** include the Postgres backend.
3. **How to launch** — `cargo run` (fastest iteration) or
   `docker compose` (closer to the deployed shape).

## Prerequisites

- Rust toolchain pinned by [`rust-toolchain.toml`](../rust-toolchain.toml).
- Node 18+ if you will run any TS examples or packages.
- Docker if you will use `docker-compose.*.yml`.
- A Miden node — required for almost every flow. Either point at a
  Miden Devnet endpoint or run one locally; configure via
  `GUARDIAN_NETWORK_TYPE`.

## Environment file

The server calls `dotenvy::dotenv()` on startup, so `cargo run --bin server`
automatically reads a root `.env` file when one exists. A `.env` file is not
strictly required for the default filesystem server, but it is recommended for
local `cargo run` because the built-in filesystem defaults live under
`/var/guardian`, which may not exist or be writable on a developer machine.

Minimal local `.env`:

```bash
GUARDIAN_STORAGE_PATH=.guardian/storage
GUARDIAN_METADATA_PATH=.guardian/metadata
GUARDIAN_KEYSTORE_PATH=.guardian/keystore
GUARDIAN_NETWORK_TYPE=MidenDevnet
RUST_LOG=info
```

Create the directories once before the first run:

```bash
mkdir -p .guardian/storage .guardian/metadata .guardian/keystore
```

Use `.env.example` as a broader template when you need deploy variables,
Postgres, dashboard operators, or EVM settings. Docker Compose does not inject
the root `.env` into the server container by default; the checked-in compose
file already sets the container filesystem paths.

## Path A — `cargo run` with filesystem (fastest)

```bash
cargo run --bin server
```

This builds with no extra features and uses the **filesystem** backend.
Useful env:

| Variable | Notes |
|---|---|
| `GUARDIAN_STORAGE_PATH` | Local path for state + deltas. Defaults to `/var/guardian/storage`. |
| `GUARDIAN_METADATA_PATH` | Local path for accounts, auth, network. Defaults to `/var/guardian/metadata`. |
| `GUARDIAN_KEYSTORE_PATH` | ACK key files, auto-generated on first run. Defaults to `/var/guardian/keystore`. |
| `RUST_LOG` (`info`) | `info`, `debug`, or e.g. `server::jobs::canonicalization=debug`. |
| `GUARDIAN_NETWORK_TYPE` (`MidenDevnet`) | Miden network name. |

At startup the server emits a warning that audit events will **not** be
persisted — that's expected for filesystem mode
([`builder/storage.rs:133`](../crates/server/src/builder/storage.rs#L133)).

The HTTP server binds on `:3000`, gRPC on `:50051`.

## Path B — `cargo run` with Postgres

```bash
docker compose -f docker-compose.postgres.yml up -d

DATABASE_URL=postgres://guardian:guardian@localhost:5432/guardian \
  cargo run -p guardian-server --features postgres --bin server
```

The Postgres path runs SQL migrations on startup
([`builder/storage.rs:109`](../crates/server/src/builder/storage.rs#L109)) and
wires `PostgresAuditor` so admin actions land in the `admin_actions` table.
Pool sizing is controlled by `GUARDIAN_DB_POOL_MAX_SIZE` and
`GUARDIAN_METADATA_DB_POOL_MAX_SIZE`.

## Path C — `cargo run` with EVM support

```bash
GUARDIAN_EVM_RPC_URLS=31337=http://127.0.0.1:8545 \
GUARDIAN_EVM_ENTRYPOINT_ADDRESS=0x... \
  cargo run -p guardian-server --features evm --bin server
```

EVM routes (`/evm/auth/*`, `/evm/accounts`, `/evm/proposals*`) only register
when the `evm` feature is on. Combine with `postgres` for prod-like local
setups: `--features postgres,evm`. Pair with an Anvil node — the
`smoke-test-evm-proposal-support` skill walks through the full flow.

## Path D — Docker Compose

```bash
docker compose up --build -d
docker compose logs -f
```

This is the default Compose flow — **filesystem backend**, no Postgres, no
root `.env` required. For a Postgres-backed compose stack use
`docker-compose.postgres.yml`. Endpoints are the same as Path A (`:3000`,
`:50051`).

## Choosing a feature flag combo

| Goal | Features | Backend |
|---|---|---|
| Hack on a service handler quickly | _none_ | filesystem |
| Touch migrations, audit, or multi-replica behavior | `postgres` | Postgres |
| Exercise the EVM proposal flow | `evm` | filesystem |
| Reproduce a prod issue locally | `postgres,evm` | Postgres |
| Run the operator dashboard | _any_ | either (Postgres for durable history) |

The deploy script builds with `postgres,evm` when the EVM stack is requested
— see [`SERVER_AWS_DEPLOY.md`](./SERVER_AWS_DEPLOY.md#quick-start).

## Verifying the server is up

```bash
curl http://localhost:3000/                   # liveness
curl http://localhost:3000/pubkey             # ACK key commitment
grpcurl -plaintext \
  -import-path crates/server/proto -proto guardian.proto \
  -d '{}' localhost:50051 guardian.Guardian/GetPubkey
```

If `GetPubkey` returns a key, the server is wired correctly and the gRPC
target group's health check
([`infra/alb.tf:55`](../infra/alb.tf#L55)) would pass in production.

## Reaching for an example

| Example | What it exercises | SDK |
|---|---|---|
| [`examples/demo`](../examples/demo/README.md) | End-to-end multisig flow in a Rust TUI — recommended starting point. | Rust multisig client |
| [`examples/rust`](../examples/rust/README.md) | Low-level Rust binaries for both local-node and mockchain flows. | Rust client |
| [`examples/smoke-web`](../examples/smoke-web/README.md) | Browser harness for multisig + wallet integrations. | TS multisig client |
| [`examples/operator-smoke-web`](../examples/operator-smoke-web/README.md) | Local Falcon operator login + dashboard account APIs. | `@openzeppelin/guardian-operator-client` |
| [`examples/evm-smoke-web`](../examples/evm-smoke-web/README.md) | EVM proposal lifecycle against Anvil + an EVM-enabled server. | `@openzeppelin/guardian-evm-client` |
| [`examples/web`](../examples/web/README.md) | Reference web integration. | TS multisig client |

Follow each example's README to drive it manually. Agents in this repo
have matching skills (`smoke-test-rust-multisig-sdk`,
`smoke-test-ts-multisig-sdk`, `smoke-test-operator-dashboard`,
`smoke-test-evm-proposal-support`) that automate the same flows.

## Running tests

```bash
cargo test --workspace
cargo test -p guardian-server --features integration
cargo test -p guardian-server --features e2e
```

TypeScript packages each carry their own `npm test` — see the root
[`README.md`](../README.md#typescript-tests).

Cargo feature gates (`integration`, `e2e`) document what each suite
needs at the top of the relevant test modules under
[`crates/server/src/testing`](../crates/server/src/testing). Agents in
this repo have a `guardian-validation-matrix` skill that picks the
smallest meaningful set for a given change.

## Common gotchas

- **`DATABASE_URL` missing under `--features postgres`** — the builder
  fails fast with `"DATABASE_URL environment variable is required"`
  ([`builder/storage.rs:97`](../crates/server/src/builder/storage.rs#L97)).
- **Filesystem dirs don't exist** — the server creates them on demand, but
  the parent path must be writable.
- **ACK keypair changes between runs in filesystem mode** — keys
  auto-generate on first start. If clients pinned an old pubkey, point them
  at the new one or persist the keystore directory.
- **gRPC reflection over Cloudflare** — works locally but Cloudflare's
  free tier rejects gRPC unless explicitly enabled on the zone.
- **Apple Silicon + `--platform linux/amd64`** — slow but works. For
  faster local Docker builds, build `linux/arm64` images and deploy with
  `cpu_architecture = "ARM64"`.
