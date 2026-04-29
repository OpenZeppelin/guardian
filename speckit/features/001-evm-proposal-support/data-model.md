# Data Model: Domain-separated EVM proposal support

## Feature Gate

EVM support is opt-in. Default Guardian builds do not register `/evm/*` routes
or initialize EVM session state, contract readers, metadata writes, or proposal
handlers. Miden routes remain available in default builds.

## Account Metadata

Miden account metadata is created through `/configure` and keeps the existing
state, auth, replay, and acknowledgement behavior.

EVM account metadata is created through `/evm/accounts`.

| Field | Type | Notes |
|-------|------|-------|
| `account_id` | `String` | Canonical `evm:<chain_id>:<smart_account_address>` |
| `auth` | `EvmEcdsa` | Snapshot of normalized EOA signers from the validator |
| `network_config` | `Evm` | Chain ID, smart account address, multisig validator address |
| `created_at` / `updated_at` | `String` | Existing metadata timestamps |
| `last_auth_timestamp` | `Option<i64>` | Not used by EVM cookie sessions |

RPC URLs and EntryPoint addresses are deployment configuration keyed by chain
ID. They are not trusted client request fields.

## EVM Session

| Field | Type | Notes |
|-------|------|-------|
| `address` | `String` | Normalized EOA recovered from challenge signature |
| `nonce` | `bytes32` | Single-use challenge nonce |
| `issued_at` / `expires_at` | timestamp | Challenge/session lifetime bounds |
| `guardian_evm_session` | cookie | Opaque server session token |

Challenge nonces are time-limited and single-use. Sessions expire and are the
only EVM route authentication mechanism in v1.

## EVM Proposal

EVM proposals are domain records exposed through `/evm/proposals*`. They are
not public `DeltaObject` records.

| Field | Type | Notes |
|-------|------|-------|
| `proposal_id` | `String` | Deterministic hash for the active proposal |
| `account_id` | `String` | Canonical EVM account ID |
| `chain_id` | `u64` | EVM chain |
| `smart_account_address` | `String` | ERC-7579 smart account |
| `validator_address` | `String` | Multisig validator module |
| `user_op_hash` | `bytes32` | Client-supplied hash signed by EOAs |
| `payload` | `String` | Opaque application payload |
| `nonce` | `String` | Full uint256 EntryPoint nonce |
| `nonce_key` | `String` | EntryPoint nonce key derived from the full nonce |
| `proposer` | `String` | Session EOA that created the proposal |
| `signer_snapshot` | `Vec<String>` | Validator EOA signer snapshot |
| `threshold` | `usize` | Validator threshold snapshot |
| `signatures` | `Vec<EvmProposalSignature>` | Collected EOA signatures |
| `created_at` / `expires_at` | timestamp | Proposal lifetime |

`proposal_id` is derived from `(account_id, validator_address, user_op_hash,
full_nonce)`. Duplicate active creates with the same identifier are idempotent.

## EVM Signature

| Field | Type | Notes |
|-------|------|-------|
| `signer` | `String` | Normalized recovered EOA |
| `signature` | `String` | 65-byte ECDSA signature |
| `signed_at` | timestamp | Server timestamp |

Signatures are verified against the stored `user_op_hash`. Signer authorization
is checked against the proposal signer snapshot.

## Lifecycle

- EVM proposals are active until cancelled, expired, or finalized by EntryPoint
  nonce advancement.
- Executable export is available only when collected signatures meet threshold.
- Miden canonicalization does not process EVM proposals.
- Filesystem and Postgres backends must expose the same observable EVM behavior.
