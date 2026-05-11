# Components

## API

The API exposes a simple interface for operating states, deltas, and proposal coordination over HTTP and gRPC. Miden uses the state/delta paths and the delta proposal workflow. EVM is a feature-gated HTTP domain under `/evm/*` for wallet sessions, smart account registration, and opaque proposal coordination.

The operator dashboard surface is HTTP-only and lives under `/dashboard/*`. Authentication is established by the `/auth/*` challenge/sign flow, which sets a `guardian_operator_session` cookie. Read-only dashboard endpoints (account list, account detail, info snapshot, per-account delta history, per-account proposal queue) are paginated with a cursor envelope `{ items, next_cursor }` and use a stable error taxonomy: `401 authentication_failed`, `404 account_not_found`, `400 invalid_cursor` / `invalid_limit` / `invalid_status_filter`, `503 data_unavailable`. The breaking change in feature `005-operator-dashboard-metrics` (always-paginated `/dashboard/accounts`, removal of `total_count`) is captured in `spec/api.md`.

## Metadata

- Stores per-account configuration required to authorise requests, dispatch network-specific behavior, and route to storage.
- Records: `account_id`, authentication policy, `network_config`, storage backend type, timestamps, and `last_auth_timestamp` for replay protection.
- `network_config` is the durable source for account network identity.
- Miden account metadata is created by `/configure` with initial state and acknowledgement binding.
- EVM account metadata is created by `/evm/accounts` with the canonical smart account address, chain ID, multisig validator address, and signer snapshot auth policy. Chain RPC URLs and the shared EntryPoint address remain server-owned configuration.
- Offers CRUD operations for metadata and a simple list operation to iterate accounts.
- Supports reverse lookup by Miden cosigner commitment (`find_by_cosigner_commitment`) so a recovering wallet holding only a signing key can resolve the account ID(s) it authorizes — see the `GET /state/lookup` endpoint. The Postgres backend serves this via a GIN index over `auth -> '<scheme>' -> 'cosigner_commitments'` (`jsonb_path_ops`); the filesystem backend serves it as a scan. EVM rows store `signers` rather than `cosigner_commitments` and never match.

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
- The account-less lookup endpoint (`GET /state/lookup`, gRPC `GetAccountByKeyCommitment`) uses a dedicated signing primitive `LookupAuthMessage` whose digest is **domain-separated by construction** from the per-account `AuthRequestMessage` (a fixed RPO domain tag is prepended to the lookup digest input, and the array shapes differ in length and leading felts). A signature crafted under one shape cannot validate against the other in either direction. Lookup auth derives identity from the signature itself — Falcon signatures embed the public key, ECDSA signatures recover it via the recovery byte — and then enforces `commitment_of(derived_pk) == queried_key_commitment`. The `x-pubkey` header is sent for wire-format parity with per-account requests but not consulted on this path, so wallet signers that only expose a 32-byte commitment work without weakening proof-of-possession (the signature is what proves possession). New endpoints (including this one) emit failures via the structured `GuardianError` → `IntoResponse` envelope, NOT the legacy `get_state`-style 404-shaped body.

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
