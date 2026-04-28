# Guardian

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![CLA Assistant](https://github.com/OpenZeppelin/guardian/actions/workflows/cla.yml/badge.svg)](https://github.com/OpenZeppelin/guardian/actions/workflows/cla.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/OpenZeppelin/guardian/badge)](https://api.securityscorecards.dev/projects/github.com/OpenZeppelin/guardian)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/11427/badge)](https://www.bestpractices.dev/projects/11427)

Warning: This is a work in progress.

### Specification

See the [Specification](spec/index.md) for an overview of the system design. It describes core concepts (State and Delta), components (API, Metadata, Auth, Acknowledger, Network, Storage), and key processes such as canonicalization. If you’re integrating or extending the system, start there to understand invariants, defaults, and extension points.

### Project Structure

#### Rust Crates

- **[crates/server](crates/server/README.md)** - Server for managing private account states and deltas
  - Reproducible builds for binary verification and TEE deployment
- **[crates/client](crates/client/README.md)** - Client SDK for interacting with the GUARDIAN server
- **[crates/shared](crates/shared/README.md)** - Shared types and utilities
- **[crates/miden-rpc-client](crates/miden-rpc-client/README.md)** - Lightweight wrapper around Miden node RPC API - inspired in `miden-client` implementation.
- **[crates/miden-keystore](crates/miden-keystore/README.md)** - Keystore implementation for Miden cryptographic keys - inspired in `miden-client` implementation.

#### TypeScript Packages

- **[packages/guardian-client](packages/guardian-client/README.md)** - TypeScript HTTP client for GUARDIAN server
- **[packages/guardian-evm-client](packages/guardian-evm-client/README.md)** - TypeScript EVM client for GUARDIAN proposal workflows
- **[packages/guardian-operator-client](packages/guardian-operator-client/README.md)** - Lean TypeScript HTTP client for operator dashboard auth and account APIs
- **[packages/miden-multisig-client](packages/miden-multisig-client/README.md)** - TypeScript SDK for Miden multisig accounts with GUARDIAN integration

### Quick Start

See the [Server README](crates/server/README.md) for detailed API documentation and usage examples.

### Benchmarking

Server benchmark harness is in [crates/server/bench](crates/server/bench/README.md).
For env-driven benchmark network/canonicalization settings, apply the runtime code switch documented there.

### Configuration

#### Environment Variables

- `DATABASE_URL` - PostgreSQL connection URL (required only for explicit Postgres-backed runs)
- `GUARDIAN_KEYSTORE_PATH` - Keystore path for cryptographic keys (default: `/var/guardian/keystore`)
- `RUST_LOG` - Logging level (default: `info`)
  - Supports: `trace`, `debug`, `info`, `warn`, `error`
  - Module-specific: `RUST_LOG=server::jobs::canonicalization=debug`
- `GUARDIAN_RATE_LIMIT_ENABLED` - Enable or disable HTTP rate limiting entirely (default: `true`)
- `GUARDIAN_RATE_BURST_PER_SEC` - Maximum requests per second (default: `10`)
- `GUARDIAN_RATE_PER_MIN` - Maximum requests per minute (default: `60`)
- `GUARDIAN_MAX_REQUEST_BYTES` - Maximum request body size in bytes (default: `1048576` = 1 MB)
- `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` - Maximum pending delta proposals per account (default: `20`)
- `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` - Optional comma-separated EVM chain allowlist used when the server is built with `--features evm`

### Running

#### Running with Cargo

```bash
cargo run --bin server
```

EVM proposal support is feature-gated. Default builds expose the schema but
reject EVM config/auth/proposal requests with `evm_support_disabled`.

```bash
GUARDIAN_EVM_ALLOWED_CHAIN_IDS=31337 \
cargo run -p guardian-server --features evm --bin server
```

#### Running with Docker Compose

1. Copy `.env.example` to `.env`

```bash
cp .env.example .env
```

2. Edit `.env` with your configuration

3. Start the server:

```bash
docker compose up --build -d
```

4. View logs:

```bash
docker compose logs -f
```

5. Stop services:

```bash
docker compose down
```

The HTTP server will be available at `http://localhost:3000`

The gRPC server will be available at `localhost:50051`

This default Compose flow uses the filesystem backend. If you need a local Postgres container for benchmark or explicit Postgres-backed runs, use [docker-compose.postgres.yml](/Users/marcos/repos/guardian/docker-compose.postgres.yml) instead.

### Testing

#### Rust Tests

Run the full workspace test suite:

```bash
cargo test --workspace
```

Feature-gated test groups:

```bash
# Run only integration tests
cargo test -p guardian-server --features integration

# Run only e2e tests
cargo test -p guardian-server --features e2e
```

#### TypeScript Tests

```bash
# Install dependencies
cd packages/guardian-client && npm install
cd packages/guardian-evm-client && npm install
cd packages/guardian-operator-client && npm install
cd packages/miden-multisig-client && npm install

# Run tests
cd packages/guardian-client && npm test
cd packages/guardian-evm-client && npm test
cd packages/guardian-operator-client && npm test
cd packages/miden-multisig-client && npm test
```
