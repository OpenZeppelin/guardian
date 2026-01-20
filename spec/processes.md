# Processes

## Services overview

- **configure_account**: creates a new account by validating the provided initial state against the network, persisting it, and storing account metadata (auth, timestamps).
- **push_delta**: verifies the delta against the current state, computes the new commitment, attaches an acknowledgement, and either enqueues it as a candidate (canonicalization enabled) or immediately applies it and marks it canonical (optimistic mode).
- **get_state**: authenticates and returns the latest persisted account state.
- **get_delta**: authenticates and returns a specific delta by nonce.
- **get_delta_since**: authenticates, fetches deltas after a given nonce (excluding discarded), merges their payloads via the network client, and returns a single merged delta snapshot.

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
  C->>S: POST /configure {account_id, auth, initial_state}
  S->>N: validate_credential(initial_state, credential)
  S->>A: auth.verify(account_id, credential)
  S->>N: get_state_commitment(account_id, initial_state)
  S->>ST: submit_state(state_json, commitment)
  S->>M: set(account_id, auth, timestamps)
  S-->>C: 200 {account_id, ack_pubkey}
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
  S->>M: get(account_id) & verify(credentials)
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
  S->>M: get(account_id) & verify(credentials)
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
  S->>M: get(account_id) & verify(credentials)
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
  C->>S: GET /delta/since?account_id=...&from_nonce=... {credentials}
  S->>M: get(account_id) & verify(credentials)
  S->>ST: pull_deltas_after(account_id, from_nonce)
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
  C->>S: POST /delta/proposal {account_id, nonce, delta_payload(tx_summary,...)}
  S->>M: get(account_id) & verify(credentials)
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
  C->>S: PUT /delta/proposal {account_id, commitment, signature,...}
  S->>M: get(account_id) & verify(credentials)
  S->>ST: pull_delta_proposal(account_id, commitment)
  S->>S: ensure status.pending & signer not recorded
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
  S->>M: get(account_id) & verify(credentials)
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
    - Delete matching delta proposal identified via `delta_proposal_id(account_id, nonce, delta_payload)`.
  - Else set delta status to `discarded`.

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
