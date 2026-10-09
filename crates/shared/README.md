## GUARDIAN Shared Crate

This crate contains shared types and utilities for the GUARDIAN project.

### Features

- `auth`: Authentication utilities for Miden Falcon RPO-512
- `hex`: Hex utilities for converting between types and hex strings
- `auth_request_eip712`: EIP-712 digests for Guardian request, account lookup,
  session grant and session revoke-all authentication
- `session_grant`: Miden account session grants (issue #219): the grant fields,
  origin validation, and the grant, logout and revoke-all digests every client
  must reproduce byte for byte (`tests/fixtures/session_grant_vectors.json`)
- `session_key`: the delegated P-256 session key: generation, raw `r || s`
  signing and verification
- `retry`: Transient-failure classification, jittered backoff, and the retry
  policy types shared by the Guardian server and the Miden SDK clients
- `account_delta`: Applying a Miden `AccountDelta` to an account, with or
  without an additional storage patch, and reconstructing an account from the
  delta that created it. A delta that carries account code creates an account
  only while the account is new (nonce zero); on an existing account it is a
  code upgrade. Shared so the server and the multisig client agree
  byte-for-byte on the resulting state commitment
