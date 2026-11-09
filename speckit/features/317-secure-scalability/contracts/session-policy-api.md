# Contract: Session Policy Advertisement and New Error Code

## gRPC (`crates/server/proto/guardian.proto`)

```protobuf
message CreateSessionResponse {
  string signer_commitment = 1;
  string expires_at = 2;               // RFC 3339 UTC
  uint32 request_auth_version = 3;     // 2; 0 means unsupported
}
```

## HTTP `POST /session` response

```json
{
  "signer_commitment": "0x...",
  "expires_at": "2026-10-30T12:00:00Z",
  "request_auth_version": 2
}
```

## HTTP `GET /status`

```json
{
  "sessions": {
    "max_ttl_seconds": 28800,
    "request_auth_version": 2
  }
}
```

`request_auth_version` is required in the server response and validated at
the SDK boundary, both when a session starts (before the wallet signs the
grant, from `GET /status`) and when a stored session is resumed. A missing
field, version `0` or a version other than `2` is an unsupported server: the
SDK raises a typed compatibility error and does not sign in any other format.
A session the Rust SDK registered before refusing is logged out.

Version 2 fixes how requests are judged, so it is the only field clients
need: each signed session read is admitted once, in any order, within 60 s
of server time, except the canonical nonce poll; session writes advance a
per-key timestamp floor within 5 minutes. The session-signed canonical nonce
poll (`GetCanonicalNonce`, `GET /state/nonce`) is accepted every time while
its session is live and its timestamp is at most 60 s behind and 30 s ahead
of server time, and leaves no record, so version 2 is not an at-most-once
guarantee for that operation. This exception was accepted in security
review on 2026-11-05 ([decision](../decision-nonce-read-window.md)) and
applies to no other session read. A wallet-signed nonce poll
keeps its floor. (Earlier drafts also advertised `read_replay_policy` and
`read_timestamp_skew_ms`; with one fixed policy they told clients nothing the
version does not, and they were removed before release.)

The policy is not configurable: every session reads under `unique`,
including sessions stored by an earlier release, which keep working without
a new grant.

## Error code `timestamp_out_of_window`

| Property | Value |
|---|---|
| HTTP | 401 |
| gRPC | `UNAUTHENTICATED` |
| Retryable | No |
| When | The request timestamp is outside its window (60 s for session reads, 60 s behind and 30 s ahead for the session nonce poll, 5 minutes otherwise), on arrival or by the store's clock at its replay or serving check |
| SDK behaviour | The session is kept; the client's clock must be fixed |

## Error code `unsupported_auth_version`

| Property | Value |
|---|---|
| Wire code | `unsupported_auth_version` (repository prefix convention per error taxonomy) |
| HTTP | 400 |
| gRPC | `INVALID_ARGUMENT` |
| Retryable | No |
| Meta | Standard `meta` only (`retryable: false`); no extra fields |
| SDK behavior | Terminal. Do not clear the grant, do not switch to wallet signing. |

Added to `GuardianError` (`http_status`, `grpc_status`, `code`,
`user_message`, `Display`, `retryable`), `GUARDIAN_ERROR_CODES` in
`packages/guardian-client/src/error-codes.ts`, the Rust client error enum,
`spec/api.md` and `docs/TROUBLESHOOTING.md`.

## OpenAPI

Regenerate with `cargo run --features evm --bin gen-openapi -- docs` after
`utoipa` annotations change (`CreateSessionResponse`, `SessionsStatus`,
`x-auth-format` description in `openapi.rs`).
