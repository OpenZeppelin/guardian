# Quickstart: Validating Security-Preserving Scalability (317)

How to check each user story once its slice is implemented. Commands assume
the repository root.

## 0. Replay-store gate (phase 2, before protocol work)

Standalone harness against a prod-class RDS instance (research R16):
3 processes, 12,000 record calls/s combined, 20,000 session keys, 10%
cross-process duplicates, 1 h with partition rollover and purge, forced
stalls past the window with and without an intervening purge.

Pass: record p99 <= 10 ms, zero doubled or lost acceptances, no rollover
errors, >= 30% DB CPU and WAL headroom, flat storage after the first hour.
Also compare the same load against the current floor UPSERT.

Filesystem restart sequence (must be rejected): accept a read timestamped
2 min ahead, restart the server, re-register the same grant, resend.

## 1. Envelope and vectors (US2)

```bash
cargo test -p guardian-shared session_request
cd packages && npm ci && npm run build -w @openzeppelin/guardian-client \
  && npm test -w @openzeppelin/guardian-client \
  && npm test -w @openzeppelin/miden-multisig-client -- session-request-vectors
```

Expect: Rust and TypeScript produce identical `envelope_hex`,
`signed_digest_hex`; each signature verifies only for its own
`(transport, operation)`.

## 2. Server replay policy (US1, US3)

```bash
# Session request auth over HTTP, nonce poll included (integration tests)
cargo test -p guardian-server --lib --features integration -- \
  testing::integration::session_http testing::integration::canonical_nonce_http
# Read records (in-memory and file stores)
cargo test -p guardian-server --lib -- coordination::read_records
# Postgres: every ignored Postgres test, including the read-record admission,
# the nonce serving call and the session floor fence; the helper enables the
# `postgres` feature and refuses an empty selection
DATABASE_URL=postgres://guardian:guardian@localhost:5432/guardian_test ./scripts/test-postgres.sh
```

Check:

- 50 parallel identical-timestamp distinct reads: all accepted.
- Resend of an accepted recorded read or a write (either S form, other replica): rejected `authentication_replay`.
- Resend of a session-signed canonical nonce poll: accepted again while the session is live and the timestamp is at most 60 s behind and 30 s ahead of server time; refused with `timestamp_out_of_window` outside that window, and with the session's code after revocation or expiry. A wallet-signed nonce poll resend is still `authentication_replay`.
- Read with `ts <= watermark` after a forced purge: rejected.
- Same credentials on another operation or transport: `authentication_failed`.
- `x-auth-format: session`: `unsupported_auth_version`, HTTP 400 / gRPC `INVALID_ARGUMENT`.
- Filesystem and Postgres produce the same outcomes.

## 3. Formal models

```bash
.claude/skills/verify-auth-replay/check.sh --deep
.claude/skills/verify-candidate-queue/check.sh
.claude/skills/verify-worker-lease/check.sh
docs/architecture/scalability/tla/run.sh
```

Expect: `expected.txt` updated so every cell passes, including the former
`session-unbounded accept FAIL:AcceptedAtMostOnce` (F2) cell. Admission
freshness, purge fence and filesystem restart are separate properties, and
disabling either check alone makes its own property fail.

## 4. Read authorization freshness (US4)

Two local server processes sharing one Postgres:

1. Start a session read loop against server B.
2. Remove the session's signer through server A (`/configure`).
3. Next read on B: `not a cosigner`. Repeat with session revoke: `session revoked`.
4. Stop Postgres: reads fail closed (no data, `internal`).
5. Pause the account: reads continue, writes refused.

## 5. SDK behavior

```bash
cargo test -p guardian-client write_sequencer
cargo test -p miden-multisig-client session
npm test -w @openzeppelin/guardian-client -- write-sequencer
```

Manual: `examples/smoke-web` and `examples/demo` with sessions (create,
sync several accounts in parallel, propose, sign, execute, revoke), Falcon
and ECDSA grant signers.

## 6. Background jobs (US7)

Seed 100,000 accounts against local Postgres
(`benchmarks/prod-server` seed command with a local profile), then measure
release sweep pass time, dashboard stats refresh duration, and metrics
refresher duration.

## 7. Target benchmark (US5, US6, success criteria)

```bash
cargo run -p guardian-prod-benchmarks -- preflight \
  --profile benchmarks/prod-server/profiles/target-317-mix.toml
# distributed ECS run; it collects the worker artifacts and aggregates them
./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/target-317-mix.toml --workers 8
# repeat with target-317-full-state.toml for the full get_state leg, and
# target-317-mix-wallet.toml for the same load under wallet credentials
```

Profile: 100,000 accounts, 20,000 readers polling every 2 s (nonce poll,
state on change), 100 writers on distinct accounts at one push per 10 s,
plus burst and hot-account legs, sustained 30 min or more, 2+ replicas,
direct DB. Run three legs: v1 strict (baseline build), v2 without
duplicate record (test flag), v2 with duplicate record. Pass: zero
first-attempt replay rejections for valid traffic, state read p95 <= 1 s
at >= 8,000 reads/s, accepted-to-canonical p95 <= 30 s (with
chain-inclusion-to-canonical reported beside it), dashboard p95 within 10%
of server-span p95.
