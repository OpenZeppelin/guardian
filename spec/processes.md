# Processes

## Services overview

- **configure_account**: creates a new account by validating the provided network configuration and auth policy, then storing account metadata. Miden accounts also validate and persist the initial state. EVM accounts are feature-gated, require empty `initial_state`, and store normalized signer/module metadata after ERC-7579-style module checks.
- **push_delta**: verifies a Miden delta against the current state, computes the new commitment, attaches an acknowledgement, and either enqueues it as a candidate (canonicalization enabled) or immediately applies it and marks it canonical (optimistic mode). EVM accounts do not support `push_delta` in v1.
- **get_state**: authenticates and returns the latest persisted account state.
- **get_delta**: authenticates and returns a specific delta by nonce.
- **get_delta_since**: authenticates, fetches deltas after a given nonce (excluding discarded), merges their payloads via the network client, and returns a single merged delta snapshot.
- **push_delta_proposal**: creates a pending proposal. Miden proposals validate `tx_summary` against state and derive IDs through the Miden network client. EVM proposals are feature-gated, validate EVM payload shape, enforce signer authority, and derive IDs from `(chain_id, account_address, mode, keccak256(execution_calldata))`.
- **sign_delta_proposal**: appends one signer signature to a pending proposal. EVM signatures are EIP-712 proposal signatures recovered to signer EOA addresses.

### Diagrams

#### configure_account
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant N as Network
  participant ST as Storage
  participant M as Metadata
  C->>S: POST /configure {account_id, auth, network_config?, initial_state} + credentials
  S->>S: verify timestamp (within 300s skew window)
  S->>S: validate network_config for account_id
  alt EVM config and evm feature disabled
    S-->>C: error evm_support_disabled
  else Miden account
    S->>N: validate_credential(initial_state, credential)
    S->>S: auth.verify(account_id, timestamp, request_payload_digest, credential)
    S->>N: get_state_commitment(account_id, initial_state)
    S->>ST: submit_state(state_json, commitment)
    S->>M: set(account_id, auth, network_config, timestamps, last_auth_timestamp)
    S-->>C: 200 {account_id, ack_pubkey, ack_commitment}
  else EVM account with evm feature
    S->>S: verify EIP-712 request auth
    S->>N: read ERC-7579 module signers and threshold
    S->>S: require request signer and configured signers are authorized
    S->>M: set(account_id, EvmEcdsa(signers), network_config, timestamps, last_auth_timestamp)
    S-->>C: 200 {account_id}
  end
```

#### push_delta
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  C->>S: POST /delta {delta, credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  alt EVM account
    S-->>C: error unsupported_for_network
  else Miden account
    S->>ST: pull_state(account_id)
    S->>ST: pull_deltas_after(account_id, 0)
    alt pending candidate exists
      S-->>C: 409 ConflictPendingDelta
    else no pending candidate
      S->>N: verify_delta(prev_commitment, prev_state, payload)
      S->>N: apply_delta(prev_state, payload)\n(new_state_json, new_commitment)
      S->>S: ack_delta(delta.new_commitment) -> ack_sig
      alt canonicalization enabled
        S->>ST: submit_delta(candidate)
      else optimistic mode
        S->>ST: submit_state(new_state)
        S->>ST: submit_delta(canonical)
      end
      S-->>C: 200 {delta}
    end
  end
```

#### get_state
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: GET /state?account_id=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  S->>ST: pull_state(account_id)
  S-->>C: 200 {state}
```

#### get_delta
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: GET /delta?account_id=...&nonce=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  S->>ST: pull_delta(account_id, nonce)
  S-->>C: 200 {delta}
```

#### get_delta_since
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  C->>S: GET /delta/since?account_id=...&nonce=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  S->>ST: pull_deltas_after(account_id, nonce)
  S->>S: filter -> only canonical
  S->>N: merge_deltas(delta_payloads) -> merged_payload
  S->>S: build merged_delta (nonce=last, prev=first.prev, new=last.new, status=canonical)
  S-->>C: 200 {merged_delta}
```

#### push_delta_proposal
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  C->>S: POST /delta/proposal {account_id, nonce, delta_payload}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  alt EVM payload and evm feature disabled
    S-->>C: error evm_support_disabled
  else Miden proposal
    S->>ST: pull_state(account_id)
    S->>N: verify_delta(prev_commitment, state_json, tx_summary)
    S->>N: delta_proposal_id(account_id, nonce, tx_summary)
    S->>ST: submit_delta_proposal(id, pending_delta)
    S-->>C: 200 {delta, commitment:id}
  else EVM proposal
    S->>S: validate kind, mode, execution_calldata, empty signatures
    S->>N: ensure authenticated signer is module signer
    S->>S: proposal_id = keccak256(chain_id, account_address, mode, calldata_hash)
    alt pending proposal already exists
      S-->>C: 200 {existing_delta, commitment:proposal_id}
    else new proposal
      S->>ST: submit_delta_proposal(proposal_id, pending_delta)
      S-->>C: 200 {delta, commitment:proposal_id}
    end
  end
```

#### sign_delta_proposal
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: PUT /delta/proposal {account_id, commitment, signature,...}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  S->>ST: pull_delta_proposal(account_id, commitment)
  S->>S: ensure status.pending & signer not recorded
  alt EVM account
    S->>S: recover EIP-712 proposal signer
    S->>S: require recovered signer == authenticated signer
  else Miden account
    S->>S: derive signer commitment from x-pubkey
  end
  S->>ST: update_delta_proposal(commitment, append_signature)
  S-->>C: 200 {delta_with_signatures}
```

#### get_delta_proposals
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: GET /delta/proposal?account_id=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  S->>ST: pull_all_delta_proposals(account_id)
  S->>S: filter(status.pending) & sort_by_nonce
  S-->>C: 200 {proposals}
```

## Canonicalization

### Modes
- Candidate mode (enabled): `push_delta` stores deltas as `candidate`; a background worker promotes or discards them after verification.
- Optimistic mode (disabled): `push_delta` marks deltas as `canonical` immediately and updates state.

### Configuration
- Defaults: delay_seconds = 900 (15m), check_interval_seconds = 60 (1m).
- Per deployment configurable.

### Worker Behavior
 - Runs every `check_interval_seconds`.
 - For each account:
  - Pull all deltas and select ready candidates (candidate_at >= delay_seconds); process in nonce order.
  - Apply delta locally to compute expected state and commitment.
  - Verify on-chain commitment. If it matches `new_commitment`:
    - Persist new state (atomic with delta status update when possible).
    - Optionally update auth from chain via `should_update_auth`.
    - Set delta status to `canonical`.
    - Delete matching Miden delta proposal identified via `delta_proposal_id(account_id, nonce, delta_payload)`.
  - Else set delta status to `discarded`.

EVM proposals are not processed by canonicalization in v1. They remain pending records used to collect signatures for on-chain submission, and on-chain execution tracking/reconciliation is out of scope.

#### Canonicalization worker (diagram)
```mermaid
sequenceDiagram
  autonumber
  participant T as Timer
  participant W as Worker
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  T->>W: tick(check_interval)
  W->>M: list()
  loop accounts
    W->>ST: pull_deltas_after(account_id, 0)
    W->>W: filter ready candidates (>= delay_seconds)\nsort by nonce
    loop candidates
      W->>ST: pull_state(account_id)
      W->>N: apply_delta(prev_state, delta)\n(new_state, expected_commitment)
      W->>N: verify_state(account_id, new_state)\n(on_chain_commitment)
      alt commitments match
        W->>ST: submit_state(new_state)
        W->>W: maybe update_auth(should_update_auth)
        W->>ST: submit_delta(canonical)
      else mismatch
        W->>ST: submit_delta(discarded)
      end
    end
  end
```

### State Machine
- candidate -> canonical | discarded. Discarded deltas MUST NOT be returned by default APIs.

### Failure Handling
- Transient failures SHOULD be retried with backoff. Malformed candidates SHOULD be quarantined with logs/metrics.

### Concurrency
- Processing SHOULD be per-account sequential; multi-account processing MAY be parallel with bounded concurrency.
