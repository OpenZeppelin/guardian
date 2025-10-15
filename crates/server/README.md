# Private State Manager Server

Server for managing private account states and deltas.

The server supports both **HTTP REST** and **gRPC** protocols simultaneously:
- HTTP server runs on port **3000**
- gRPC server runs on port **50051**

### Configuration

#### Environment Variables

- `PSM_APP_PATH` - Base directory for filesystem storage (default: `/var/psm/app`)

#### Storage Backends

Currently supported:
- **Filesystem** - Local file-based storage (default)

#### Metadata Storage

Account metadata is stored in `.metadata/accounts.json` and includes:
- `account_id` - Unique identifier for the account
- `storage_type` - Backend storage type (e.g., "local")
- `cosigner_pubkeys` - List of authorized cosigner public keys
- `created_at` - Account creation timestamp (ISO 8601)
- `updated_at` - Last update timestamp (ISO 8601)

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

### Manualesting with curl


#### Manual tests using curl

#### 1. Configure an account

```bash
curl -X POST http://localhost:3000/configure \
  -H "Content-Type: application/json" \
  -d '{
    "account_id": "alice",
    "initial_state": {
      "nonce": 0
    },
    "storage_type": "local"
  }'
```

#### 2. Push a delta

```bash
curl -X POST http://localhost:3000/delta \
  -H "Content-Type: application/json" \
  -H "x-pubkey: pubkey_xyz" \
  -H "x-signature: signature_xyz" \
  -d '{
    "account_id": "alice",
    "nonce": 1,
    "prev_commitment": "prev_commit_hash",
    "delta_hash": "delta_hash_123",
    "delta_payload": {
      "operation": "transfer",
      "amount": 100
    },
    "ack_sig": "ack_signature",
    "candidate_at": "2025-10-08T12:00:00Z",
    "canonical_at": null,
    "discarded_at": null
  }'
```

#### 3. Get a specific delta

```bash
curl "http://localhost:3000/delta?account_id=alice&nonce=1"
```

#### 4. Get the latest delta (head)

```bash
curl "http://localhost:3000/head?account_id=alice"
```

#### 5. Get account state

```bash
curl "http://localhost:3000/state?account_id=alice"
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
  "account_id": "alice",
  "initial_state": "{}",
  "storage_type": "local",
  "cosigner_pubkeys": []
}' localhost:50051 state_manager.StateManager/Configure
```

#### 3. Push a delta
```bash
grpcurl -plaintext -d '-H "x-pubkey: pubkey_xyz" -H "x-signature: signature_xyz"' '{
  "account_id": "alice",
  "nonce": 1,
  "prev_commitment": "prev_commit_hash",
  "delta_hash": "delta_hash_123",
  "delta_payload": "{\"operation\":\"transfer\",\"amount\":100}",
  "ack_sig": "ack_signature",
  "candidate_at": "2025-10-08T12:00:00Z"
}' localhost:50051 state_manager.StateManager/PushDelta
```

#### 4. Get a specific delta
```bash
grpcurl -plaintext -d '{
  "account_id": "alice",
  "nonce": 1
}' localhost:50051 state_manager.StateManager/GetDelta
```

#### 5. Get the latest delta head
```bash
grpcurl -plaintext -d '{
  "account_id": "alice"
}' localhost:50051 state_manager.StateManager/GetDeltaHead
```

#### 6. Get account state
```bash
grpcurl -plaintext -d '{
  "account_id": "alice"
}' localhost:50051 state_manager.StateManager/GetState
```

### Building gRPC Clients

Use the proto file at `proto/state_manager.proto` to generate client code:
- **Rust**: `tonic` and `prost`
- **Python**: `grpcio` and `grpcio-tools`
- **Go**: Official `protoc` compiler with Go plugins
- **JavaScript/TypeScript**: `@grpc/grpc-js` and `@grpc/proto-loader`