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

- `PSM_ENV` - Environment (default: `dev`)
- `PSM_STORAGE_PATH` - Storage backend path (default: `/var/psm/storage`)
- `PSM_METADATA_PATH` - Metadata store path (default: `/var/psm/metadata`)
- `PSM_KEYSTORE_PATH` - Keystore path for cryptographic keys (default: `/var/psm/keystore`)
- `RUST_LOG` - Logging level (default: `info`)

### Account Configuration

Each account has:
- `account_id` - Network-specific identifier
- `auth` - Auth type with authorization data (e.g., cosigner public keys)
- `storage_type` - Which backend stores this account's data

```rust
use server::builder::ServerBuilder;

// Create storage registry with filesystem backend
// NOTE: Other storage types will be supported in the future.
let storage_registry = StorageRegistry::with_filesystem(PathBuf::from("/var/psm/storage")).await?;

let builder = ServerBuilder::new()
    .network(NetworkType::MidenTestnet)
    .storage(storage_registry).await?);
```

Also the server supports configuring the networks, storage types, metadata store, and keystore.

```rust
use server::builder::ServerBuilder;
use server::network::NetworkType;
use server::storage::StorageRegistry;
use server::storage::filesystem::FilesystemMetadataStore;
use std::path::PathBuf;

// Store accounts metadata using filesystem backend.
let metadata = FilesystemMetadataStore::new(PathBuf::from("/var/psm/metadata")).await?;

let builder = ServerBuilder::new()
    .metadata(Arc::new(metadata))
    .keystore(PathBuf::from("/var/psm/keystore"));
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