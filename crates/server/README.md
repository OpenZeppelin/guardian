# Guardian Server

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

- `DATABASE_URL` - PostgreSQL connection URL (required for Postgres storage/metadata, e.g., `postgres://guardian:guardian_dev_password@localhost:5432/guardian`)
- `GUARDIAN_ENV` - Runtime environment (`prod` uses Secrets Manager-backed ack bootstrap, anything else uses filesystem ack keys)
- `GUARDIAN_KEYSTORE_PATH` - Keystore path for cryptographic keys (default: `/var/guardian/keystore`)
- `AWS_REGION` - AWS region used to fetch production ack keys from Secrets Manager
- `RUST_LOG` - Logging level (default: `info`)

#### Rate Limiting

- `GUARDIAN_RATE_LIMIT_ENABLED` - Enable or disable HTTP rate limiting entirely (default: `true`)
- `GUARDIAN_RATE_BURST_PER_SEC` - Maximum requests per second (burst limit, default: `10`)
- `GUARDIAN_RATE_PER_MIN` - Maximum requests per minute (sustained limit, default: `60`)

#### Request Size Limits

- `GUARDIAN_MAX_REQUEST_BYTES` - Maximum request body size in bytes (default: `1048576` = 1 MB)
- `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` - Maximum pending delta proposals per account (default: `20`)

Requests exceeding this limit receive a 413 Payload Too Large response.

#### Operator Dashboard

- `GUARDIAN_OPERATOR_ALLOWLIST_JSON` - JSON array of operator entries, each with `operator_id` and `commitment`
- `GUARDIAN_DASHBOARD_DOMAIN` - Domain marker bound into operator login challenges. Set `*` to leave the domain open for now (default: `*`)
- `GUARDIAN_DASHBOARD_ALLOW_INSECURE_HTTP` - Explicit local-only opt-in to omit the `Secure` cookie attribute (default: `false`)
- `GUARDIAN_OPERATOR_SESSION_COOKIE_NAME` - Session cookie name for dashboard auth (default: `guardian_operator_session`)
- `GUARDIAN_OPERATOR_NONCE_TTL_SECS` - Challenge lifetime in seconds (default: `300`)
- `GUARDIAN_OPERATOR_SESSION_TTL_SECS` - Absolute dashboard session lifetime in seconds, capped at 24h (default: `28800`)
- `GUARDIAN_OPERATOR_MAX_OUTSTANDING_CHALLENGES` - Maximum active challenges retained per operator commitment (default: `8`)
- `GUARDIAN_OPERATOR_PUBKEY_RATE_BURST_PER_SEC` - Per-operator challenge/verify burst limit keyed by commitment (default: `5`)
- `GUARDIAN_OPERATOR_PUBKEY_RATE_PER_MIN` - Per-operator challenge/verify sustained limit keyed by commitment (default: `30`)

Example allowlist payload:

```json
[
  {
    "operator_id": "alice",
    "commitment": "0x..."
  }
]
```

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

let storage = FilesystemService::new(PathBuf::from("/var/guardian/storage")).await?;
```

Filesystem is the default when the binary is built without the `postgres` feature.
It is also the default ack-key mode for local runs, using `GUARDIAN_KEYSTORE_PATH`.

#### Postgres Storage

Postgres support is optional and must be enabled at build time with the `postgres` feature.
When enabled, provide `DATABASE_URL` and the server will use Postgres by default.
Migrations run automatically at startup (the server runs migrations on boot).

```rust
use server::storage::postgres::PostgresService;

let database_url = "postgres://guardian:guardian_dev_password@localhost:5432/guardian";

let storage = PostgresService::new(&database_url).await?;
```

```bash
DATABASE_URL=postgres://guardian:guardian_dev_password@localhost:5432/guardian \
cargo run --features postgres --package guardian-server
```

### Ack Key Backends

Local runs and non-prod environments default to filesystem-backed ack keys under `GUARDIAN_KEYSTORE_PATH`.

Production ECS runs bootstrap the filesystem keystore from Secrets Manager when:

```bash
GUARDIAN_ENV=prod
AWS_REGION=us-east-1
```

On startup, the server fetches these two fixed Secrets Manager entries once, imports them into `GUARDIAN_KEYSTORE_PATH`, and then uses the normal filesystem keystore for signing:

- `guardian-prod/server/ack-falcon-secret-key`
- `guardian-prod/server/ack-ecdsa-secret-key`

Local and `dev` deployments stay on the filesystem-only path.

### Metadata Store

The server supports configuring the metadata store separately from the storage backends.

#### Filesystem Metadata Store

```rust
use server::metadata::filesystem::FilesystemMetadataStore;
use std::path::PathBuf;
use std::sync::Arc;

let metadata = FilesystemMetadataStore::new(PathBuf::from("/var/guardian/metadata")).await?;

let builder = ServerBuilder::new()
    .metadata(Arc::new(metadata));
```

#### Postgres Metadata Store

```rust
use server::metadata::postgres::PostgresMetadataStore;
use std::sync::Arc;

let database_url = "postgres://guardian:guardian_dev_password@localhost:5432/guardian";

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
RUST_LOG=debug cargo run --package guardian-server

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
- **Ingress assumption**: GUARDIAN prefers `X-Forwarded-For`, then `X-Real-IP`, then the socket peer IP. Deployments should restrict direct access so only the ingress proxy/load balancer can reach the server
- **Disable switch**: `GUARDIAN_RATE_LIMIT_ENABLED=false` bypasses HTTP rate limiting entirely

If `GUARDIAN_RATE_LIMIT_ENABLED=false`, the HTTP server skips rate limiting regardless of the other rate-limit settings.

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

// Load from environment (GUARDIAN_RATE_LIMIT_ENABLED, GUARDIAN_RATE_BURST_PER_SEC, GUARDIAN_RATE_PER_MIN, GUARDIAN_MAX_REQUEST_BYTES)
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
- **GET** `/delta/proposal?account_id=<id>` - List pending delta proposals for an account
- **GET** `/delta/proposal/single?account_id=<id>&commitment=<c>` - Retrieve a pending proposal by commitment
- **GET** `/auth/challenge?commitment=<commitment>` - Issue an operator dashboard login challenge
- **POST** `/auth/verify` - Verify an operator dashboard signature and set a session cookie
- **POST** `/auth/logout` - Clear the operator dashboard session cookie and invalidate the server-side session
- **GET** `/dashboard/accounts` - List dashboard account summaries for an authenticated operator session
- **GET** `/dashboard/accounts/{account_id}` - Fetch one dashboard account detail record for an authenticated operator session

#### gRPC API (Port 50051)

All methods are available through the `guardian.Guardian` service:
- `Configure(ConfigureRequest) -> ConfigureResponse`
- `PushDelta(PushDeltaRequest) -> PushDeltaResponse`
- `GetDelta(GetDeltaRequest) -> GetDeltaResponse`
- `GetDeltaHead(GetDeltaHeadRequest) -> GetDeltaHeadResponse`
- `GetState(GetStateRequest) -> GetStateResponse`
- `GetDeltaSince(GetDeltaSinceRequest) -> GetDeltaSinceResponse`
- `PushDeltaProposal(PushDeltaProposalRequest) -> PushDeltaProposalResponse`
- `SignDeltaProposal(SignDeltaProposalRequest) -> SignDeltaProposalResponse`
- `GetDeltaProposals(GetDeltaProposalsRequest) -> GetDeltaProposalsResponse`
- `GetDeltaProposal(GetDeltaProposalRequest) -> GetDeltaProposalResponse`

See `proto/guardian.proto` for the complete protocol buffer definitions.


## Running with Docker Compose

The project includes a root [docker-compose.yml](/Users/marcos/repos/guardian/docker-compose.yml) for a filesystem-backed local server:

```bash
# Start the local server
docker compose up -d

# The service exposes:
# - Server HTTP: localhost:3000
# - Server gRPC: localhost:50051
```

If you need a local Postgres container, use [docker-compose.postgres.yml](/Users/marcos/repos/guardian/docker-compose.postgres.yml):

```bash
POSTGRES_PASSWORD=guardian_dev_password docker compose -f docker-compose.postgres.yml up -d
```

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

For benchmark runs that need env-driven `GUARDIAN_NETWORK_TYPE` and `GUARDIAN_CANONICALIZATION_*`, use the runtime code switch documented in `crates/server/bench/README.md` under `Benchmark Runtime Code Switch (Main Branch)`.

## Testing

Run all tests:

```bash
cargo test
```

Run specific integration tests:

```bash
cargo test --package guardian-server --test e2e_http_auth_test -- --test-threads=1
```

Feature-gated test groups:

```bash
# Integration tests (requires network/mocks as applicable)
cargo test -p guardian-server --features integration

# End-to-end tests
cargo test -p guardian-server --features e2e
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

Use the proto file at `proto/guardian.proto` to generate client code:
- **Rust**: `tonic` and `prost`
- **Python**: `grpcio` and `grpcio-tools`
- **Go**: Official `protoc` compiler with Go plugins
- **JavaScript/TypeScript**: `@grpc/grpc-js` and `@grpc/proto-loader`
