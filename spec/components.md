# Components

## API

The API exposes a simple interface for operating states and deltas over HTTP and gRPC. Behaviour is consistent across transports so clients can switch between them without semantic changes. See `api.md` for endpoint shapes and error semantics, including the multi-party delta proposal workflow (`push_delta_proposal`, `get_delta_proposal`, `sign_delta_proposal`) that allows cosigners to coordinate before submitting a canonical delta.

## Metadata

- Stores per-account configuration required to authorise requests and route to storage.
- Records: `account_id`, authentication policy, storage backend type, timestamps, and `last_auth_timestamp` for replay protection.
- Offers CRUD operations for metadata and a simple list operation to iterate accounts.

## Auth

- Request authentication is configured per account.
- Current policy: Miden Falcon RPO with an allowlist of `cosigner_commitments` (commitments of authorised public keys).
- Requests carry `x-pubkey`, `x-signature`, and `x-timestamp`; verification derives the commitment from the supplied public key, checks it is authorised, and verifies the signature over a digest of `(account_id, timestamp, request_payload_digest)`.
- Replay protection: the signed timestamp is validated against a 300-second skew window and must be strictly greater than the account's `last_auth_timestamp`.

## Acknowledger

- Produces tamper -evident acknowledgements for accepted deltas.
- Current policy: sign the digest of `new_commitment` and return the signature in `ack_sig`.
- A public discovery endpoint exposes the server’s acknowledgement key (as a commitment) for clients to cache.

## Network

- Computes commitments, validates/executes deltas against the target network’s rules, and merges multiple deltas into a single snapshot payload.
- Validates account identifiers and request credentials against network-owned state when applicable.
- Surfaces suggested auth updates (e.g., rotated cosigner commitments) so metadata remains aligned with the network.

## Storage

- Persists account snapshots and deltas.
- Provides efficient retrieval by account and nonce, plus range queries for canonicalisation.
- Stores pending delta proposals in a per-account namespace keyed by proposal commitment so the canonicalization worker (and optimistic flow) can delete proposals once their corresponding delta becomes canonical.
- Backends are pluggable (e.g., filesystem, database) without altering API semantics.
