# Quickstart

A Guardian running on your machine in under 60 seconds, with one command
to confirm it's alive.

This is the fast path. For depth (Postgres, EVM, feature flags, examples)
read [`docs/LOCAL_DEV.md`](./LOCAL_DEV.md). For the *why* before the *how*,
read [`docs/CONCEPTS.md`](./CONCEPTS.md). For production readiness, start
with [`docs/PRODUCTION.md`](./PRODUCTION.md).

The compose file sets the container paths it needs, but you must choose a
network explicitly — `GUARDIAN_NETWORK_TYPE` has no default and the server
refuses to start without it (there is no fallback network). Set it in a
local `.env` file or export it in your shell; accepted values are
`MidenLocal`, `MidenTestnet`, and `MidenDevnet`. The same requirement
applies when you run the server directly with `cargo run`; see
[`LOCAL_DEV.md`](./LOCAL_DEV.md#environment-file).

## Run

```bash
echo "GUARDIAN_NETWORK_TYPE=MidenTestnet" > .env
docker compose up --build -d
```

This launches the server with the filesystem backend. HTTP binds on
`:3000`, gRPC on `:50051`.

## Verify

```bash
curl http://localhost:3000/                   # liveness — expect 200 OK
curl http://localhost:3000/pubkey             # expect { "commitment": "0x..." }
```

If both succeed, Guardian is running. The commitment you see is the ACK
key commitment clients will pin.

## Stop

```bash
docker compose down
```

## What you just got

- A filesystem-backed Guardian server (no Postgres, no EVM).
- Auto-generated ACK keypair persisted in the container's keystore.
- HTTP + gRPC on default ports.
- No operator dashboard (allowlist is empty by default).

This is enough to point an example SDK at:

```bash
# Rust TUI demo against your local Guardian
cd examples/demo && cargo run --release
```

The demo also needs a Miden RPC endpoint (Devnet works out of the box).
See [`docs/LOCAL_DEV.md`](./LOCAL_DEV.md#prerequisites) if your network
choice differs.

## Where to next

| Goal | Read |
|---|---|
| Understand what Guardian *is* before going further | [`CONCEPTS.md`](./CONCEPTS.md) |
| Switch to Postgres, enable EVM, run without Docker | [`LOCAL_DEV.md`](./LOCAL_DEV.md) |
| Set every available env var deliberately | [`CONFIGURATION.md`](./CONFIGURATION.md) |
| Enable the operator dashboard locally | [`DASHBOARD.md`](./DASHBOARD.md) |
| Deploy or operate in production | [`PRODUCTION.md`](./PRODUCTION.md) |
| Something broke | [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) |
