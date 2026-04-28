# Components

## API

The API exposes a simple interface for operating states, deltas, and proposal coordination over HTTP and gRPC. Behaviour is consistent across transports so clients can switch between them without semantic changes. See `api.md` for endpoint shapes and error semantics, including the multi-party delta proposal workflow (`push_delta_proposal`, `get_delta_proposal`, `sign_delta_proposal`) that allows cosigners to coordinate before submitting a Miden canonical delta or an EVM on-chain proposal.

## Metadata

- Stores per-account configuration required to authorise requests, dispatch network-specific behavior, and route to storage.
- Records: `account_id`, authentication policy, `network_config`, storage backend type, timestamps, and `last_auth_timestamp` for replay protection.
- `network_config` is the durable source for account network identity:
  - Miden accounts store a Miden network type.
  - EVM accounts store `chain_id`, `account_address`, `multisig_module_address`, and `rpc_endpoint`.
- Offers CRUD operations for metadata and a simple list operation to iterate accounts.

## Auth

- Request authentication is configured per account.
- Supported policies:
  - Miden Falcon RPO with an allowlist of `cosigner_commitments`.
  - Miden ECDSA with an allowlist of `cosigner_commitments`.
  - EVM ECDSA with signer EOA addresses.
- Requests carry `x-pubkey`, `x-signature`, and `x-timestamp`.
- Miden verification derives a commitment from the supplied public key, checks it is authorised, and verifies the signature over `(account_id, timestamp, request_payload_digest)`.
- EVM verification is network-aware: the server recovers the signer from EIP-712 request typed data, checks it matches the normalized `x-pubkey` EOA address, and checks the module still authorises that signer.
- Replay protection: the signed timestamp is validated against a 300-second skew window and must be strictly greater than the account's `last_auth_timestamp`.
- Default server builds reject EVM auth/config/proposal requests with `evm_support_disabled` before storage mutation.

## Acknowledger

- Produces tamper-evident acknowledgements for accepted deltas.
- Current policy: sign the digest of `new_commitment` and return the signature in `ack_sig`.
- A public discovery endpoint exposes the server’s acknowledgement key (as a commitment) for clients to cache.
- EVM v1 proposal coordination does not produce canonical deltas or acknowledgement signatures.

## Network

- Computes commitments, validates/executes deltas against the target network’s rules, and merges multiple deltas into a single snapshot payload.
- Dispatches behavior by account `network_config`.
- Miden behavior remains the canonical state/delta path.
- EVM behavior is feature-gated and currently covers account configuration plus pending proposal create/list/get/sign; execution tracking and reconciliation are out of scope.
- EVM account configuration and request auth read an ERC-7579-style module for signers and threshold through Alloy when the `evm` feature is enabled.
- Validates account identifiers and request credentials against network-owned state when applicable.
- Surfaces suggested auth updates (e.g., rotated cosigner commitments) so metadata remains aligned with the network.

## Storage

- Persists account snapshots and deltas.
- Provides efficient retrieval by account and nonce, plus range queries for canonicalisation.
- Stores pending delta proposals in a per-account namespace keyed by proposal commitment.
- Miden proposals are deleted once their corresponding delta becomes canonical.
- EVM proposals remain pending-only in Guardian v1 and are used as off-chain signature coordination records for on-chain submission.
- Backends are pluggable (e.g., filesystem, database) without altering API semantics.
