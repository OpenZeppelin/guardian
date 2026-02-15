# Private State Manager Server

Server for managing private account states and deltas.

## Protocols

Can run either or both of gRPC and HTTP APIs:

```rust
use server::builder::ServerBuilder;

let builder = ServerBuilder::new()
    .http(true, 3000)
    .grpc(true, 50051);
```

## Configuration

### Environment Variables

- `POSTGRES_PASSWORD` - PostgreSQL password (required for Postgres storage/metadata)
- `DATABASE_URL` - PostgreSQL connection URL (required for Postgres storage/metadata, e.g., `postgres://psm:${POSTGRES_PASSWORD}@localhost:5432/psm`)
- `PSM_KEYSTORE_PATH` - Keystore path for cryptographic keys (default: `/var/psm/keystore`)
- `RUST_LOG` - Logging level (default: `info`)

#### Rate Limiting

- `PSM_RATE_BURST_PER_SEC` - Maximum requests per second (burst limit, default: `10`)
- `PSM_RATE_PER_MIN` - Maximum requests per minute (sustained limit, default: `60`)

#### Request Size Limits

- `PSM_MAX_REQUEST_BYTES` - Maximum request body size in bytes (default: `1048576` = 1 MB)

Requests exceeding this limit receive a 413 Payload Too Large response.

### Account Configuration

Each account has:
- `account_id` - Network-specific identifier
- `auth` - Auth type with authorization data (e.g., cosigner public keys)

### Storage Backends

The server uses a single storage backend per instance: `Filesystem` by default, or `Postgres` when built with the `postgres` feature.

#### Filesystem Storage

```rust
use server::storage::filesystem::FilesystemService;
use std::path::PathBuf;

let storage = FilesystemService::new(PathBuf::from("/var/psm/storage")).await?;
```

Filesystem is the default when the binary is built without the `postgres` feature.

#### Postgres Storage

Postgres support is optional and must be enabled at build time with the `postgres` feature.
When enabled, provide `DATABASE_URL` and the server will use Postgres by default.
Migrations run automatically at startup (the server runs migrations on boot).

```rust
use server::storage::postgres::PostgresService;

let database_url = "postgres://psm:psm_dev_password@localhost:5432/psm";

let storage = PostgresService::new(&database_url).await?;
```

```bash
DATABASE_URL=postgres://psm:psm_dev_password@localhost:5432/psm \
cargo run --features postgres --package private-state-manager-server
```

### Metadata Store

The server supports configuring the metadata store separately from the storage backends.

#### Filesystem Metadata Store

```rust
use server::metadata::filesystem::FilesystemMetadataStore;
use std::path::PathBuf;
use std::sync::Arc;

let metadata = FilesystemMetadataStore::new(PathBuf::from("/var/psm/metadata")).await?;

let builder = ServerBuilder::new()
    .metadata(Arc::new(metadata));
```

#### Postgres Metadata Store

```rust
use server::metadata::postgres::PostgresMetadataStore;
use std::sync::Arc;

let database_url = "postgres://psm:psm_dev_password@localhost:5432/psm";

let metadata = PostgresMetadataStore::new(&database_url).await?;

let builder = ServerBuilder::new()
    .metadata(Arc::new(metadata));
```

### Logging

The server uses structured logging via the `tracing` crate. Configure logging programmatically:

```rust
use server::builder::ServerBuilder;
use server::logging::LoggingConfig;
use tracing::Level;

ServerBuilder::new()
    .with_logging(LoggingConfig::new(Level::DEBUG))
    // ... other configuration
```

Or use the `RUST_LOG` environment variable to override:

```bash
# Debug level for entire server
RUST_LOG=debug cargo run --package private-state-manager-server

# Trace only canonicalization jobs
RUST_LOG=server::jobs::canonicalization=trace cargo run

# Multiple modules
RUST_LOG=server::jobs=debug,server::services=info cargo run
```

### Rate Limiting

The HTTP API includes built-in rate limiting to protect against abuse. Rate limits are applied per client IP, with enhanced keying when authentication headers or account IDs are present.

#### How It Works

- **IP-based limits**: All requests are tracked by client IP address
- **Enhanced keying**: When `x-pubkey` header or `account_id` query parameter is present, limits are applied per IP+account/signer combination
- **Two windows**: Burst (per second) and sustained (per minute) limits are enforced independently
- **Proxy support**: Respects `X-Forwarded-For` and `X-Real-IP` headers for proxied requests

#### Response When Limited

When rate limited, the server returns HTTP 429 with a JSON body:

```json
{
  "success": false,
  "error": "Rate limit exceeded (burst limit). Retry after 1 seconds.",
  "retry_after_secs": 1
}
```

The `Retry-After` header is also set with the recommended wait time.

#### Programmatic Configuration

```rust
use server::builder::ServerBuilder;
use server::middleware::{RateLimitConfig, BodyLimitConfig};

// Custom limits
ServerBuilder::new()
    .with_rate_limit(RateLimitConfig::new(20, 120))  // 20/sec, 120/min
    .with_body_limit(BodyLimitConfig::new(5 * 1024 * 1024))  // 5 MB
    // ...

// Load from environment (PSM_RATE_BURST_PER_SEC, PSM_RATE_PER_MIN, PSM_MAX_REQUEST_BYTES)
ServerBuilder::new()
    .with_rate_limit(RateLimitConfig::from_env())
    .with_body_limit(BodyLimitConfig::from_env())
    // ...
```

### API Endpoints

#### HTTP REST API (Port 3000)

- **POST** `/configure` - Configure a new account with initial state
- **POST** `/delta` - Submit a new delta for an account
- **GET** `/delta?account_id=<id>&nonce=<n>` - Retrieve a specific delta by account ID and nonce
- **GET** `/head?account_id=<id>` - Get the latest delta (highest nonce) for an account
- **GET** `/state?account_id=<id>` - Retrieve the current state of an account
- **GET** `/delta/since?account_id=<id>&nonce=<n>` - Retrieve the delta since a given nonce
- **POST** `/delta/proposal` - Create a delta proposal for multi-party signing
- **POST** `/delta/proposal/sign` - Add a signature to an existing delta proposal
- **GET** `/delta/proposals?account_id=<id>` - List pending delta proposals for an account

#### gRPC API (Port 50051)

All methods are available through the `state_manager.StateManager` service:
- `Configure(ConfigureRequest) -> ConfigureResponse`
- `PushDelta(PushDeltaRequest) -> PushDeltaResponse`
- `GetDelta(GetDeltaRequest) -> GetDeltaResponse`
- `GetDeltaHead(GetDeltaHeadRequest) -> GetDeltaHeadResponse`
- `GetState(GetStateRequest) -> GetStateResponse`
- `GetDeltaSince(GetDeltaSinceRequest) -> GetDeltaSinceResponse`
- `PushDeltaProposal(PushDeltaProposalRequest) -> PushDeltaProposalResponse`
- `SignDeltaProposal(SignDeltaProposalRequest) -> SignDeltaProposalResponse`
- `GetDeltaProposals(GetDeltaProposalsRequest) -> GetDeltaProposalsResponse`

See `proto/state_manager.proto` for the complete protocol buffer definitions.


## Running with Docker Compose

The project includes a `docker-compose.yml` with a Postgres service for local development:

```bash
# Start the server with Postgres
docker-compose up -d

# The services expose:
# - Server HTTP: localhost:3000
# - Server gRPC: localhost:50051
# - Postgres: localhost:5432 (user: psm, password: psm_dev_password, db: psm)
```

The Postgres service uses a health check and the server waits for it to be ready before starting.

## Benchmarking

Server benchmark harness lives in:

- `crates/server/bench/README.md`

It includes:
- Filesystem vs Postgres comparison runs
- scaling workloads (`state-read`, `state-write`, `mixed`)
- rate-limiting and request-size checks

Quick commands:

```bash
./crates/server/bench/scripts/run_fs.sh
./crates/server/bench/scripts/run_postgres.sh
./crates/server/bench/scripts/run_matrix.sh
```

For benchmark runs that need env-driven `PSM_NETWORK_TYPE` and `PSM_CANONICALIZATION_*`, use the runtime code switch documented in `crates/server/bench/README.md` under `Benchmark Runtime Code Switch (Main Branch)`.

## Testing

Run all tests:

```bash
cargo test
```

Run specific integration tests:

```bash
cargo test --package private-state-manager-server --test e2e_http_auth_test -- --test-threads=1
```

Feature-gated test groups:

```bash
# Integration tests (requires network/mocks as applicable)
cargo test -p private-state-manager-server --features integration

# End-to-end tests
cargo test -p private-state-manager-server --features e2e
```

### Reproducible Builds

The server binary has reproducible builds. Building from the same source code and target architecture always produces bit-for-bit identical binaries, regardless of the build machine.

#### Verifying Published Binaries

To verify a published binary matches the source code:

1. Build for the target architecture and compare hashes:
   ```bash
   ./crates/server/tests/verify-build-hash.sh
   # Compare SHA256 output with published release hash
   ```

2. If hashes match, the binary is verified authentic.

```bash
# Build for linux/amd64 (default - matches official releases)
./crates/server/tests/verify-build-hash.sh

# Build for linux/arm64
PLATFORM=linux/arm64 ./crates/server/tests/verify-build-hash.sh
```

**Note**: Different architectures produce different binaries and hashes. For cross-machine verification, use the same target architecture on all machines.

#### Updating Pinned Versions

To update Docker image digests:

```bash
# Get current digest for rust:1.88
docker pull rust:1.88
docker inspect rust:1.88 | grep -A 1 "RepoDigests"

# Get current digest for debian:bookworm-slim
docker pull debian:bookworm-slim
docker inspect debian:bookworm-slim | grep -A 1 "RepoDigests"
```

Update the digests in `Dockerfile` to maintain reproducibility. Then verify that hash matches across machines:

```bash
./crates/server/tests/verify-build-hash.sh
```

### Building gRPC Clients

Use the proto file at `proto/state_manager.proto` to generate client code:
- **Rust**: `tonic` and `prost`
- **Python**: `grpcio` and `grpcio-tools`
- **Go**: Official `protoc` compiler with Go plugins
- **JavaScript/TypeScript**: `@grpc/grpc-js` and `@grpc/proto-loader`
