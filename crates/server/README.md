# Private State Manager Server

Server for managing private account states and deltas.

## Protocols

Runs both HTTP REST and gRPC simultaneously:
- HTTP: port 3000
- gRPC: port 50051

## Configuration

### Environment Variables

- `PSM_ENV` - Environment (default: `dev`)
- `PSM_STORAGE_PATH` - Storage backend path (default: `/var/psm/storage`)
- `PSM_METADATA_PATH` - Metadata store path (default: `/var/psm/metadata`)

### Account Configuration

Each account has:
- `account_id` - Network-specific identifier
- `auth` - Auth type with authorization data (e.g., cosigner public keys)
- `storage_type` - Which backend stores this account's data

### API Endpoints

#### HTTP REST API (Port 3000)

- **POST** `/configure` - Configure a new account with initial state
- **POST** `/delta` - Submit a new delta for an account
- **GET** `/delta?account_id=<id>&nonce=<n>` - Retrieve a specific delta by account ID and nonce
- **GET** `/head?account_id=<id>` - Get the latest delta (highest nonce) for an account
- **GET** `/state?account_id=<id>` - Retrieve the current state of an account

#### gRPC API (Port 50051)

All methods are available through the `state_manager.StateManager` service:
- `Configure(ConfigureRequest) -> ConfigureResponse`
- `PushDelta(PushDeltaRequest) -> PushDeltaResponse`
- `GetDelta(GetDeltaRequest) -> GetDeltaResponse`
- `GetDeltaHead(GetDeltaHeadRequest) -> GetDeltaHeadResponse`
- `GetState(GetStateRequest) -> GetStateResponse`

See `proto/state_manager.proto` for the complete protocol buffer definitions.


## Testing

Run all tests:

```bash
cargo test
```

Run HTTP E2E tests:

```bash
cargo test --package private-state-manager-server --test e2e_http_auth_test -- --test-threads=1
```

Run gRPC E2E tests:

```bash
cargo test --package private-state-manager-server --test e2e_grpc_auth_test -- --test-threads=1
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

#### Benefits

- **Binary verification**: Users can verify published binaries match source code
- **Supply chain security**: Detect if binaries have been tampered with
- **TEE attestation**: Enable cryptographic verification of code running in Trusted Execution Environments
- **Build integrity**: Ensure builds are not influenced by local environment

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

Update the digests in `Dockerfile` to maintain reproducibility.

## Manual Testing

### HTTP with curl

#### Configure an account

```bash
curl -X POST http://localhost:3000/configure \
  -H "Content-Type: application/json" \
  -d '{
    "account_id": "0x1234567890abcdef1234567890abcd",
    "auth": {
      "MidenFalconRpo": {
        "cosigner_pubkeys": ["0xpubkey1", "0xpubkey2"]
      }
    },
    "initial_state": {},
    "storage_type": "Filesystem"
  }'
```

#### 2. Push a delta

```bash
curl -X POST http://localhost:3000/delta \
  -H "Content-Type: application/json" \
  -H "x-pubkey: 0xpubkey1" \
  -H "x-signature: 0xsignature_hex" \
  -d '{
    "account_id": "0x1234567890abcdef1234567890abcd",
    "nonce": 1,
    "prev_commitment": "prev_commit_hash",
    "delta_hash": "delta_hash_123",
    "delta_payload": {
      "operation": "transfer",
      "amount": 100
    },
    "ack_sig": "ack_signature",
    "candidate_at": "2025-10-15T12:00:00Z",
    "canonical_at": null,
    "discarded_at": null
  }'
```

#### 3. Get a specific delta

```bash
curl "http://localhost:3000/delta?account_id=0x1234567890abcdef1234567890abcd&nonce=1"
```

#### 4. Get the latest delta (head)

```bash
curl "http://localhost:3000/head?account_id=0x1234567890abcdef1234567890abcd"
```

#### 5. Get account state

```bash
curl "http://localhost:3000/state?account_id=0x1234567890abcdef1234567890abcd"
```

### Testing with grpcurl

Install grpcurl:
```bash
brew install grpcurl
```

#### 1. List available services
```bash
grpcurl -plaintext localhost:50051 list
```

#### 2. Configure an account
```bash
grpcurl -plaintext -d '{
  "account_id": "0x1234567890abcdef1234567890abcd",
  "auth": {
    "miden_falcon_rpo": {
      "cosigner_pubkeys": ["0xpubkey1", "0xpubkey2"]
    }
  },
  "initial_state": "{}",
  "storage_type": "Filesystem"
}' localhost:50051 state_manager.StateManager/Configure
```

#### 3. Push a delta
```bash
grpcurl -plaintext \
  -H "x-pubkey: 0xpubkey1" \
  -H "x-signature: 0xsignature_hex" \
  -d '{
  "account_id": "0x1234567890abcdef1234567890abcd",
  "nonce": 1,
  "prev_commitment": "prev_commit_hash",
  "delta_hash": "delta_hash_123",
  "delta_payload": "{\"operation\":\"transfer\",\"amount\":100}",
  "ack_sig": "ack_signature",
  "candidate_at": "2025-10-15T12:00:00Z"
}' localhost:50051 state_manager.StateManager/PushDelta
```

#### 4. Get a specific delta
```bash
grpcurl -plaintext -d '{
  "account_id": "0x1234567890abcdef1234567890abcd",
  "nonce": 1
}' localhost:50051 state_manager.StateManager/GetDelta
```

#### 5. Get the latest delta head
```bash
grpcurl -plaintext -d '{
  "account_id": "0x1234567890abcdef1234567890abcd"
}' localhost:50051 state_manager.StateManager/GetDeltaHead
```

#### 6. Get account state
```bash
grpcurl -plaintext -d '{
  "account_id": "0x1234567890abcdef1234567890abcd"
}' localhost:50051 state_manager.StateManager/GetState
```

### Building gRPC Clients

Use the proto file at `proto/state_manager.proto` to generate client code:
- **Rust**: `tonic` and `prost`
- **Python**: `grpcio` and `grpcio-tools`
- **Go**: Official `protoc` compiler with Go plugins
- **JavaScript/TypeScript**: `@grpc/grpc-js` and `@grpc/proto-loader`