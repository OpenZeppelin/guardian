# Processes

## Services overview

- **configure_account**: creates a Miden account by validating the provided network configuration and auth policy, then storing account metadata and initial state. EVM accounts are not configured through this service.
- **push_delta**: verifies a Miden delta against the current state, computes the new commitment, attaches an acknowledgement, and either enqueues it as a candidate (canonicalization enabled) or immediately applies it and marks it canonical (optimistic mode). EVM accounts do not support `push_delta` in v1.
- **get_state**: authenticates and returns the latest persisted account state.
- **get_delta**: authenticates and returns a specific delta by nonce.
- **get_delta_since**: authenticates, fetches deltas after a given nonce (excluding discarded), merges their payloads via the network client, and returns a single merged delta snapshot.
- **push_delta_proposal**: creates a pending Miden proposal by validating `tx_summary` against state and deriving IDs through the Miden network client.
- **sign_delta_proposal**: appends one signer signature to a pending Miden proposal.
- **evm_session**: issues an EIP-712 wallet challenge, recovers the EOA with `ecrecover`, consumes the nonce once, and creates a cookie-backed session.
- **evm_accounts**: registers EVM smart accounts under `/evm/accounts` by validating the cookie session signer, server-owned chain config, ERC-7579 validator installation, and signer snapshot before storing account metadata without state or acknowledgement data.
- **evm_proposals**: creates, lists, approves, fetches executable data for, and cancels EVM proposals with opaque payloads, UserOperation hashes, signer snapshots, TTL, and lazy EntryPoint nonce cleanup.

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
  S->>N: validate_credential(initial_state, credential)
  S->>S: auth.verify(account_id, timestamp, request_payload_digest, credential)
  S->>N: get_state_commitment(account_id, initial_state)
  S->>ST: submit_state(state_json, commitment)
  S->>M: set(account_id, auth, network_config, timestamps, last_auth_timestamp)
  S-->>C: 200 {account_id, ack_pubkey, ack_commitment}
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
  S->>ST: pull_state(account_id)
  S->>N: verify_delta(prev_commitment, state_json, tx_summary)
  S->>N: delta_proposal_id(account_id, nonce, tx_summary)
  S->>ST: submit_delta_proposal(id, pending_delta)
  S-->>C: 200 {delta, commitment:id}
```

#### sign_delta_proposal
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: PUT /delta/proposal {account_id, commitment, signature}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp
  S->>M: update last_auth_timestamp
  S->>ST: pull_delta_proposal(account_id, commitment)
  S->>S: ensure status.pending & signer not recorded
  S->>S: derive signer commitment from x-pubkey
  S->>ST: update_delta_proposal(commitment, append signature)
  S-->>C: 200 {delta_with_signatures}
```

#### evm_accounts_and_proposals
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant A as SmartAccount
  participant V as Validator
  participant E as EntryPoint
  participant ST as Storage
  participant M as Metadata
  C->>S: GET /evm/auth/challenge?address=...
  S-->>C: EIP-712 challenge
  C->>S: POST /evm/auth/verify {address, nonce, signature}
  S->>S: ecrecover challenge signer & consume nonce
  S-->>C: Set-Cookie guardian_evm_session
  C->>S: POST /evm/accounts {chain, account, validator}
  S->>A: isModuleInstalled(1, validator, 0x)
  S->>V: getSignerCount/getSigners/threshold
  S->>S: verify session EOA is a validator signer
  S->>M: store account metadata
  C->>S: POST /evm/proposals {account_id, user_op_hash, payload, nonce, signature}
  S->>M: load EVM account metadata
  S->>A: isModuleInstalled(1, validator, 0x)
  S->>V: getSignerCount/getSigners/threshold
  S->>S: verify proposer and initial signature against signer snapshot
  S->>ST: store active EVM proposal
  C->>S: POST /evm/proposals/{id}/approve {account_id, signature}
  S->>ST: load EVM proposal
  S->>E: getNonce(account, nonce_key)
  S->>S: delete if expired or finalized
  S->>S: verify signer is in stored snapshot and signature is unique
  S->>ST: append signature
  C->>S: GET /evm/proposals/{id}/executable?account_id=...
  S-->>C: {hash, payload, signatures, signers} once threshold is met
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

EVM proposals are not processed by Miden canonicalization. They are stored in the EVM proposal store and deleted lazily when expired or when the configured EntryPoint nonce indicates finality.

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
