# Components

## API

The API exposes a simple interface for operating states, deltas, and proposal coordination over HTTP and gRPC. Miden uses the state/delta paths and the delta proposal workflow. EVM is a feature-gated HTTP domain under `/evm/*` for wallet sessions, smart account registration, and opaque proposal coordination.

## Metadata

- Stores per-account configuration required to authorise requests, dispatch network-specific behavior, and route to storage.
- Records: `account_id`, authentication policy, `network_config`, storage backend type, timestamps, and `last_auth_timestamp` for replay protection.
- `network_config` is the durable source for account network identity.
- Miden account metadata is created by `/configure` with initial state and acknowledgement binding.
- EVM account metadata is created by `/evm/accounts` with the canonical smart account address, chain ID, multisig validator address, and signer snapshot auth policy. Chain RPC and EntryPoint addresses remain server-owned configuration.
- Offers CRUD operations for metadata and a simple list operation to iterate accounts.

## Auth

- Miden request authentication is configured per account.
- Supported policies:
  - Miden Falcon RPO with an allowlist of `cosigner_commitments`.
  - Miden ECDSA with an allowlist of `cosigner_commitments`.
- Requests carry `x-pubkey`, `x-signature`, and `x-timestamp`.
- Miden verification derives a commitment from the supplied public key, checks it is authorised, and verifies the signature over `(account_id, timestamp, request_payload_digest)`.
- EVM verification uses `/evm/auth/*`: the server recovers the EOA from an EIP-712 session challenge and stores that address in a secure cookie-backed session.
- Replay protection: the signed timestamp is validated against a 300-second skew window and must be strictly greater than the account's `last_auth_timestamp`.
- Default server builds do not register EVM routes or initialize EVM state, sessions, contract readers, or proposal handlers.

## Acknowledger

- Produces tamper-evident acknowledgements for accepted deltas.
- Current policy: sign the digest of `new_commitment` and return the signature in `ack_sig`.
- A public discovery endpoint exposes the server’s acknowledgement key (as a commitment) for clients to cache.
- EVM v1 proposal coordination does not produce canonical deltas or acknowledgement signatures.

## Network

- Computes commitments, validates/executes deltas against the target network’s rules, and merges multiple deltas into a single snapshot payload.
- Dispatches behavior by account `network_config`.
- Miden behavior remains the canonical state/delta path.
- EVM behavior is feature-gated and covers session auth plus account registration and proposal create/list/get/approve/executable/cancel through `/evm/*`.
- EVM proposal creation verifies the validator is installed on the smart account, then snapshots signer EOAs and threshold through Alloy.
- Validates account identifiers and request credentials against network-owned state when applicable.
- Surfaces suggested auth updates (e.g., rotated cosigner commitments) so metadata remains aligned with the network.

## Storage

- Persists account snapshots and deltas.
- Provides efficient retrieval by account and nonce, plus range queries for canonicalisation.
- Stores pending Miden delta proposals in a per-account namespace keyed by proposal commitment.
- Miden proposals are deleted once their corresponding delta becomes canonical.
- EVM proposals use a domain-specific logical proposal store with opaque payloads, UserOperation hashes, signer snapshots, signatures, and expiry. Implementations may reuse shared storage backend infrastructure, but the EVM API does not expose `DeltaObject` semantics.
- Backends are pluggable (e.g., filesystem, database) without altering API semantics.
