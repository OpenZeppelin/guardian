# Contract: Session Request Authentication v2

Applies to every session-signed per-account request on HTTP and gRPC.
Normative byte layout as in `spec/api.md` (Session Authentication; design rationale in the [session-auth RFC](https://github.com/OpenZeppelin/guardian/issues/317#issuecomment-6667740923), archived on #317);
operation table extended in [data-model.md](../data-model.md#sessionoperation-shared-rust-and-typescript).

## Headers / metadata

| Name | Value | Change |
|---|---|---|
| `x-auth-format` | `session-v2` | New value. `session` is rejected on per-account routes after cutover. |
| `x-pubkey` | compressed SEC1 P-256, hex | Unchanged |
| `x-signature` | 64-byte `r \|\| s`, hex | Unchanged encoding, new message |
| `x-timestamp` | Unix ms | Unchanged |

No header carries the operation or transport. The server derives both.

## Signed message

```text
bytes = "GUARDIAN_SESSION_REQUEST" || 0x00
     || u16be(2)
     || u8(transport)            # 1 = HTTP JSON, 2 = gRPC protobuf
     || u16be(operation)         # SessionOperation ID
     || AuthRequestMessage(account_id, timestamp_ms, payload).to_word().as_bytes()
signature = ECDSA_P256_SHA256(session_key, bytes)   # WebCrypto: sign(bytes, hash SHA-256)
signed_digest = SHA-256(bytes)
```

The payload rules per transport are unchanged (HTTP canonical JSON of the
query/body; gRPC protobuf encoding of the request message;
`ExecuteDeltaProposal` keeps its existing `SignedOperation` payload tag).

## Server acceptance

| Check | Failure |
|---|---|
| `x-auth-format` = `session` on a per-account route | `unsupported_auth_version` (HTTP 400, gRPC `INVALID_ARGUMENT`, not retryable) |
| `session-v2` on a wallet-only route | existing `wallet_signature_required` |
| `abs(now - ts)` above 60000 for a recorded read, 300000 for a write; for the session nonce poll `now - ts` above 60000 or `ts - now` above 30000 | `timestamp_out_of_window` (not retryable) |
| Signature invalid for the derived operation and transport | existing `authentication_failed` |
| Session expired / revoked / not a cosigner / Guardian or network mismatch | existing codes, unchanged |
| Recorded read (every session read except the nonce poll): digest already recorded for this session, or `ts <= read watermark` | `authentication_replay` (retryable with a fresh signature) |
| Session nonce poll (`GetCanonicalNonce`), proposed exception pending security review | no replay check: a copy inside its window is served again while the session is live; it is still refused on any other operation and after revocation or expiry |
| Write: `ts <= floor` or `ts <= floor watermark` | `authentication_replay` |
| Duplicate record or floor store error | fail closed (`internal`), no data returned |

For recorded reads, the duplicate record is keyed by
`record_key = SHA-256(session_public_key || bytes)`, never by signature
bytes, so the high-S and low-S forms of one signature are the same request
and the second is a duplicate. The nonce poll takes no record
([decision](../decision-nonce-read-window.md)).

## Test vectors

`crates/shared/tests/fixtures/session_request_vectors.json`, generated from
the Rust constructors and read by Rust and TypeScript tests. The file holds
one key pair (`secret_hex`, `public_key_hex`), the operation registry, and
the vectors. Each vector:

```json
{
  "name": "GetState_http",
  "account_id_hex": "0x...",
  "timestamp_ms": 1767225600000,
  "payload_hex": "0x...",
  "transport": 1,
  "operation": 1,
  "request_word_hex": "0x...",
  "envelope_hex": "0x...",
  "signed_digest_hex": "0x...",
  "record_key_hex": "0x...",
  "signature_hex": "0x..."
}
```

Required cases: every operation on both transports; same-payload pairs
(`GetState`/`GetCanonicalNonce`, `GetDeltaProposals`/`GetDeltaHistory` with
no cursor); a synthetic same-payload write pair. The high-S variant and the
single-field mutations (version, transport, operation, account, timestamp,
payload) failing verification are derived from the fixture in the Rust test
rather than stored in it.
