# Private State Manager Server

Server for managing private account states and deltas.

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

POST `/configure`
Configure a new account with initial state.

POST `/delta`
Submit a new delta for an account.

GET `/delta`
Retrieve a specific delta by account ID and nonce.

GET `/head`
Get the latest delta (highest nonce) for an account.

GET `/state`
Retrieve the current state of an account.


### Testing with curl

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
    "publisher_pubkey": "pubkey_xyz",
    "publisher_sig": "publisher_signature",
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