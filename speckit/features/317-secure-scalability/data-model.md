# Data Model: Security-Preserving Scalability (317)

**Date**: 2026-10-30 | **Spec**: [spec.md](./spec.md)

## SessionOperation (shared, Rust and TypeScript)

Permanent registry. IDs are never reused or renumbered.

| ID | Variant | HTTP | gRPC (`guardian.Guardian`) | Policy |
|---:|---|---|---|---|
| 1 | `GetState` | `GET /state` | `GetState` | Read |
| 2 | `GetCanonicalNonce` | `GET /state/nonce` | `GetCanonicalNonce` | Read |
| 3 | `GetDelta` | `GET /delta` | `GetDelta` | Read |
| 4 | `GetDeltaSince` | `GET /delta/since` | `GetDeltaSince` | Read |
| 5 | `GetDeltaHistory` | `GET /delta/history` | `GetDeltaHistory` | Read |
| 6 | `GetDeltaProposals` | `GET /delta/proposal` | `GetDeltaProposals` | Read |
| 7 | `GetDeltaProposal` | `GET /delta/proposal/single` | `GetDeltaProposal` | Read |
| 8 | `PushDelta` | `POST /delta` | `PushDelta` | Write |
| 9 | `PushDeltaProposal` | `POST /delta/proposal` | `PushDeltaProposal` | Write |
| 10 | `SignDeltaProposal` | `PUT /delta/proposal` | `SignDeltaProposal` | Write |
| 11 | `ExecuteDeltaProposal` | `POST /delta/proposal/execution` | `ExecuteDeltaProposal` | Write |
| 12 | `GetDeltaProposalExecution` | `GET /delta/proposal/execution` | `GetDeltaProposalExecution` | Read |
| 13 | `GetCurrentExecution` | `GET /delta/execution/current` | `GetCurrentExecution` | Read |

Validation: decoding an unknown ID fails; every variant has exactly one
policy; wallet-only routes (`/configure`, `/delta/candidate/abandon`,
`/state/lookup`, `/session/revoke-all`) have no variant.

## Transport

`Http = 1`, `Grpc = 2`. Set by the server's credential extractor, never by
the client.

## SessionRequestEnvelope

| Field | Type | Notes |
|---|---|---|
| domain | 24 ASCII bytes + `0x00` | `GUARDIAN_SESSION_REQUEST` |
| version | `u16` big endian | `2` |
| transport | `u8` | `Transport` |
| operation | `u16` big endian | `SessionOperation` |
| request_word | 32 bytes | `AuthRequestMessage(account_id, timestamp_ms, payload).to_word()` |

Total 62 bytes. `signed_digest = SHA-256(envelope bytes)` is the value P-256
signs (via `hash: SHA-256`). The duplicate-record key is
`record_key = SHA-256(session_public_key || envelope bytes)`.

## RequestAuthFormat (server)

`Raw | Eip712 | SessionV2`. The header value `session` maps to
`UnsupportedAuthVersion` on per-account routes and is accepted only by the
logout route.

## Read duplicate record (`session_read_seen`, Postgres)

| Column | Type | Notes |
|---|---|---|
| record_key | bytea(32) | `SHA-256(session_public_key \|\| envelope bytes)` |
| request_ts_ms | bigint | Signed timestamp; partition key |

- `PRIMARY KEY (record_key, request_ts_ms)` (40 bytes),
  `PARTITION BY RANGE (request_ts_ms)`, 5-minute partitions.
- Written only by `guardian_authorize_and_record_session_read`, which
  first checks the account and live session and inserts nothing for a
  missing, expired or revoked session.
- Lifecycle: `absent -> recorded` (insert) `-> dropped` (partition dropped
  once its upper bound <= read watermark). No updates.
- Admission (inside the combined function, after the shared lock):
  `abs(db_now_ms - ts) > skew` is `stale` (freshness); `ts <= read
  watermark` is `stale` (fence); otherwise insert.
- Filesystem: same rows appended to
  `.metadata/session_read_seen/<bucket_start_ms>.log`, flushed before the
  response, loaded on start; watermark persisted in
  `.metadata/replay_fence.json`.

## Replay fence (`replay_fence`, Postgres; struct field on filesystem)

| Column | Type | Notes |
|---|---|---|
| fence | text PK | `session_read` or `session_floor` |
| watermark_ms | bigint | Monotonically non-decreasing |

Writers of the watermark hold the exclusive advisory lock for that fence;
record/CAS calls hold the shared lock. Watermark only increases.

## Write floor (`account_auth_state`, unchanged schema)

Same key and CAS. Additional rules for `session-` keys, evaluated after the
shared lock is held: freshness against the database clock, and `ts <=
session_floor watermark` refused. Wallet keys are not purged and have no
fence.

## AccountMetadata additions

| Column | Type | Notes |
|---|---|---|
| auth_version | bigint NOT NULL DEFAULT 0 | `+1` on configure set/create, pause, unpause, release set/clear. Not changed by `has_pending_candidate` or replay CAS. |

## ReadAuthLookup (result of the per-read validation query)

| Field | Notes |
|---|---|
| auth_version | From `account_metadata` (primary) |
| session | For session requests: `auth_sessions` row (expiry, revoked, grant fields) or none |

Not cached. Errors map to the existing session-expired, session-revoked,
not-a-cosigner and mismatch errors; a query failure fails closed.

## MetadataCacheEntry (per replica, memory)

`account_id -> (auth_version, Arc<AccountMetadata>)`. Size-weighted,
bounded (default 256 MB). Never contains state, deltas or proposals.
Reachable only through `ReadAuthorizer`.

## Session policy advertisement

| Field | Value |
|---|---|
| request_auth_version | `2` (implies: session reads admitted once each within 60 s; session writes on a per-key floor within 5 minutes) |

Returned by session creation (HTTP and gRPC) and `GET /status` under
`sessions`.

## WriteSequencer (SDKs)

Owned by the active session. `account_id -> (async write lock, last_ts)`.
A write holds the lock from timestamp minting through the last bounded
retry. Reads use the same per-account `last_ts` (strictly increasing,
`max(last_ts + 1, now)`) without the lock. Drift guard: if the next value
would exceed `now + 1000 ms`, wait until it does not.

## State transitions: session read

```text
received
  -> credentials parsed (SessionV2)        else UnsupportedAuthVersion / AuthenticationFailed
  -> timestamp within +-60 s               else TimestampOutOfWindow
  -> P-256 verify over envelope            else AuthenticationFailed (invalid_signature)
  -> authorize_and_record (one DB call, primary)
       account/session missing|expired|revoked -> existing errors, nothing inserted
       lock, window re-check by the DB clock    outside -> TimestampOutOfWindow
       fence, insert: duplicate|stale -> AuthenticationReplay
       DB error -> fail closed
  -> metadata (cache if auth_version matches, else reload)
  -> grant Guardian key, network, cosigner checks   else existing errors
  -> serve route (state or state-head read)
```

Session write: read-auth lookup without the record, metadata, grant and
cosigner checks, then the fenced floor CAS, then the service's write.
