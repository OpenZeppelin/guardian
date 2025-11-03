# Private State Manager

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![CLA Assistant](https://github.com/OpenZeppelin/private-state-manager/actions/workflows/cla.yml/badge.svg)](https://github.com/OpenZeppelin/private-state-manager/actions/workflows/cla.yml)

Warning: This is a work in progress.

### Specification

See the [Specification](spec/index.md) for an overview of the system design. It describes core concepts (State and Delta), components (API, Metadata, Auth, Acknowledger, Network, Storage), and key processes such as canonicalization. If you’re integrating or extending the system, start there to understand invariants, defaults, and extension points.

### Project Structure

- **[crates/server](crates/server/README.md)** - Server for managing private account states and deltas
  - Reproducible builds for binary verification and TEE deployment
- **[crates/client](crates/client/README.md)** - Client SDK for interacting with the PSM server
- **[crates/shared](crates/shared/README.md)** - Shared types and utilities
- **[crates/miden-rpc-client](crates/miden-rpc-client/README.md)** - Lightweight wrapper around Miden node RPC API - inspired in `miden-client` implementation.
- **[crates/miden-keystore](crates/miden-keystore/README.md)** - Keystore implementation for Miden cryptographic keys - inspired in `miden-client` implementation.

### Quick Start

See the [Server README](crates/server/README.md) for detailed API documentation and usage examples.

### Configuration

#### Environment Variables

- `PSM_STORAGE_PATH` - Storage backend path (default: `/var/psm/storage`)
- `PSM_METADATA_PATH` - Metadata store path (default: `/var/psm/metadata`)
- `PSM_KEYSTORE_PATH` - Keystore path for cryptographic keys (default: `/var/psm/keystore`)
- `PSM_ENV` - Environment (default: `dev`)
- `RUST_LOG` - Logging level (default: `info`)
  - Supports: `trace`, `debug`, `info`, `warn`, `error`
  - Module-specific: `RUST_LOG=server::jobs::canonicalization=debug`

### Running

#### Running with Docker Compose

1. Copy `.env.example` to `.env`

```bash
cp .env.example .env
```

2. Edit `.env` with your configuration

3. Start the server:

```bash
docker-compose up --build -d
```

4. View logs:

```bash
docker-compose logs -f
```

5. Stop services:

```bash
docker-compose down
```

The HTTP server will be available at `http://localhost:3000`

The gRPC server will be available at `localhost:50051`

### Testing

Run the full workspace test suite:

```bash
cargo test --workspace
```

Feature-gated test groups:

```bash
# Run only integration tests
cargo test -p private-state-manager-server --features integration

# Run only e2e tests
cargo test -p private-state-manager-server --features e2e
```
