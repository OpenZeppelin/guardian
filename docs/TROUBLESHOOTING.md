# Troubleshooting

Common Guardian failures and how to resolve them. Organised by symptom
first, then by error code.

For concepts (lifecycle, trust model, recovery flows) see
[`docs/CONCEPTS.md`](./CONCEPTS.md).
For local-dev setup see [`docs/LOCAL_DEV.md`](./LOCAL_DEV.md).

## By symptom

### Server fails to start

Most startup failures are environment misconfiguration. Check in order:

1. **`GUARDIAN_NETWORK_TYPE` unset, non-Unicode, or unrecognized.** The server exits
   before binding any port with
   `Failed to resolve network type: GUARDIAN_NETWORK_TYPE is not set; accepted values: ...`
   (or `contains non-Unicode data` / `has unrecognized value "..."` for
   malformed values and typos like `tesnet`). There is no fallback network.
   Replace the variable with `MidenLocal`, `MidenTestnet`, or
   `MidenDevnet` (short forms `local`/`testnet`/`devnet` work,
   case-insensitive).
2. **Miden node unreachable or Miden RPC settings invalid.** The initial
   node connection retries transient failures for ≈35 seconds (5 attempts
   with backoff) before failing startup with
   `Failed to create network client: failed to connect to the Miden RPC endpoint` —
   a brief node blip at boot no longer kills the server, but a genuinely
   down or wrong endpoint still does, as do TLS/certificate
   misconfigurations, which fail immediately without retrying. An invalid
   `GUARDIAN_MIDEN_RPC_ENDPOINT` (not an origin-only `http(s)` URL in the
   form `scheme://host[:port]`) also fails immediately. Embedded credentials,
   paths, queries, and fragments are rejected because tonic does not send them
   as authentication. Note that
   `GUARDIAN_MIDEN_RPC_MAX_ATTEMPTS` never affects canonicalization: those
   reads make one attempt per pass and transient failures are recovered by
   the next scheduled pass. See [CONFIGURATION.md](./CONFIGURATION.md).
3. **`DATABASE_URL` missing under `--features postgres`.** The builder
   panics with `"DATABASE_URL environment variable is required"`. Either
   set it or rebuild without the `postgres` feature.
4. **Filesystem paths not writable.** Filesystem builds use
   `GUARDIAN_STORAGE_PATH`, `GUARDIAN_METADATA_PATH`, and
   `GUARDIAN_KEYSTORE_PATH` when set, defaulting to
   `/var/guardian/storage`, `/var/guardian/metadata`, and
   `/var/guardian/keystore` respectively. Startup fails if the process
   cannot create or write to those paths — common on dev machines where
   `/var/guardian` doesn't exist or isn't owned by the running user.
   Either set the env vars to a writable location or `mkdir -p`
   `/var/guardian/{storage,metadata,keystore}` with the right
   permissions.
5. **Postgres migrations fail.** The Postgres path runs migrations at
   startup. If the DB user lacks `CREATE` permissions, startup fails.
   Grant `CREATE` on the schema or run migrations as a privileged user.
   For a migration that times out, crash-loops the task, or breaks
   authentication mid-deploy, see [Postgres migrations block startup or fail
   mid-deploy](#postgres-migrations-block-startup-or-fail-mid-deploy).
6. **ACK secrets missing in prod.**
   `scripts/aws-deploy.sh deploy` refuses to apply if either ACK secret
   is missing. Run `DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys`
   first (see [Secrets runbook](./runbooks/secrets.md#bootstrap-first-prod-deploy)).
7. **Operator allowlist source not set.** If you intend to use the
   dashboard, set `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID` (prod) or
   `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` (local). Without either, the
   dashboard is unreachable.
8. **Database TLS misconfigured.** With a verifying `sslmode`, startup fails
   closed before migrations run. Map the error:
   - error naming `sslmode` (`allow`/`prefer` or an unknown value) → choose an
     explicit mode: `disable`, `require`, `verify-ca`, or `verify-full`.
   - error naming `sslrootcert` (missing/unreadable/empty file, or `system`) →
     mount a readable PEM CA bundle and point `sslrootcert=<path>` at it
     (`sslrootcert=system` is unsupported).
   - connection refused with a certificate-verification error → wrong CA for the
     server, an expired certificate, or (under `verify-full`) a hostname that
     doesn't match the certificate's SAN. For AWS RDS Proxy, ensure the bundle
     includes the Amazon Trust Services roots, not only the RDS CA roots.
   - works under `verify-ca` but fails under `verify-full` → hostname/SAN
     mismatch with the endpoint. See [Database TLS](./CONFIGURATION.md#database-tls).
9. **Replay-protection state file missing or inconsistent (filesystem builds).** Startup
   fails with `Replay-protection state file ... is missing` when
   `.metadata/auth_state.json` has been deleted from a store whose
   `accounts.json` was already migrated off legacy timestamps. Starting
   anyway would reset replay protection and re-accept previously seen
   request timestamps, so the server refuses instead. Restore
   `auth_state.json` from backup together with the rest of the metadata
   directory, including `.metadata/auth_state_legacy_floor_v1` when it
   exists. If a restore predates that marker, startup re-runs floor
   repair and synthesizes an account-level floor from the highest stored
   signer timestamp for every account, including ones created after the
   per-signer migration. A newly added signer whose first request
   timestamp is at or below that floor is rejected once; the next
   strictly later timestamp succeeds. If no backup exists, recreating
   `auth_state.json` with the literal content `{}` lets the server start.
   That is an explicit operator decision to accept a replay window as
   wide as the timestamp skew allowance. If startup instead reports
   replay state with no matching account metadata, restore
   `accounts.json` and `auth_state.json` from the same backup; Guardian
   will not rewrite or discard the unmatched floors.

### Postgres migrations block startup or fail mid-deploy

Migrations run at server startup, before the connection pools are built.
Each migration runs in a single transaction, so a failure never leaves a
half-applied schema: the task exits, the orchestrator restarts it, and it
retries from the same point.

**Check which migrations are applied.** Compare the database against the
migration directory (`crates/server/migrations/`, whose directory-name
timestamps are the versions):

```sql
SELECT version FROM __diesel_schema_migrations ORDER BY version DESC LIMIT 5;
```

**`Timed out after 60s waiting for the migration advisory lock`.** Only one
replica migrates at a time, serialised by a Postgres advisory lock. This
message means another replica held it for the whole wait, or a session died
mid-migration without releasing it. Find the holder:

```sql
SELECT a.pid, a.state, now() - a.xact_start AS age, left(a.query, 120) AS query
  FROM pg_locks l
  JOIN pg_stat_activity a USING (pid)
 WHERE l.locktype = 'advisory';
```

The lock is released when its session ends, so a restart of the stuck replica
clears it. Do not unlock it manually while another replica is still migrating.

**`Failed to run migrations: ... canceling statement due to lock timeout`.**
Migrations that rewrite a hot table take an explicit table lock with a 5s
`lock_timeout`, so they fail fast rather than queueing every later writer
behind them. A concurrent long-running transaction on the locked table is the
usual cause:

```sql
SELECT pid, now() - xact_start AS age, state, left(query, 120) AS query
  FROM pg_stat_activity
 WHERE xact_start IS NOT NULL
   AND now() - xact_start > interval '5 seconds'
 ORDER BY age DESC;
```

A restart normally succeeds. Repeated failures mean a persistent long
transaction, not a broken migration.

**Authenticated requests fail on some replicas during a deploy.** A migration
that changes a table the *previous* binary writes is not backward compatible,
and the deployment keeps old tasks serving until the new one is healthy
(`deployment_minimum_healthy_percent = 100`). Between the migration committing
and the old tasks draining, requests routed to an old task fail. This is
deliberate: failing closed beats writing state the new schema cannot represent.
It is expected for `2026-07-31-000001_account_auth_state` and
`2026-08-13-000001_auth_state_per_signer`. To eliminate the window rather than
ride it out, stop the old tasks before the new one migrates, which trades the
partial errors for brief full downtime.

### Guardian public key changes unexpectedly

**Treat this as a security event** until you can confirm intentional
rotation:

1. Stop trusting deltas signed under the new key.
2. Check the deployment audit trail: who ran
   `aws secretsmanager update-secret` on the ACK secret ARNs? CloudTrail
   `GetSecretValue` events identify the principals.
3. Check task restart timing: ACK keys are read once at startup, so a
   pubkey change implies either a rotation event or a task that came up
   in a different environment (e.g. `GUARDIAN_ENV` unset → ephemeral
   filesystem keys).
4. If unrotated, follow the
   [compromise response runbook](./runbooks/secrets.md#compromise-response).

Common benign causes:
- Local dev with `GUARDIAN_KEYSTORE_PATH` pointing at a tmpfs that was
  wiped between restarts.
- Running with `GUARDIAN_ENV` unset and no `AWS_REGION` — the server
  falls back to filesystem keystore and auto-generates a fresh key.

### Signed requests are rejected at the auth layer

This section covers the auth-middleware verdicts, `401` with
`code: authentication_failed` or `code: authentication_replay`. If your
request was *authenticated* but still rejected (e.g.
`403 authorization_failed`, `403 signer_not_authorized`,
`400 commitment_mismatch`, `409 conflict_pending_*`), jump straight to
the [error code reference](#error-code-reference): the signature was
fine, the service layer rejected the operation.

The two 401 codes mean different things (issue #367 split them):

- **`authentication_replay`** (`meta.retryable: true`). The request was
  correctly signed, but its `x-timestamp` was not strictly greater than
  the last timestamp Guardian accepted *from the same signer* on that
  account. This is the replay-protection CAS, not a credential problem:
  it happens when two in-flight requests from one client land out of
  order, or when the same key is used from two processes at once. Both
  SDK clients retry it automatically (bounded, fresh timestamp and
  signature per attempt); a user-visible `authentication_replay` means
  the condition persisted through the retry budget; look for a second
  process (background poller, second tab, another host) signing with
  the same key. Replay state is scoped per `(account, signer)`, so
  other cosigners of a multisig never cause this for you.
- **`authentication_failed`** (`meta.retryable: false`, terminal) for
  three reasons:
  1. **Clock skew.** Timestamps must be within ±5 minutes of server time
     ([`metadata/auth/credentials.rs:6`](../crates/server/src/metadata/auth/credentials.rs#L6)).
     Sync the client clock (NTP) or check for container time drift.
  2. **Invalid or unauthorized signature.** The signer's public-key
     commitment is not in the account's authorized set, or the signature
     does not verify.
  3. **Modified payload after signing.** The signature covers the
     request body hash. Mutating the body (proxy reformatting, JSON
     re-serialise) invalidates the signature. Sign-then-send; do not
     transform between.

Headers required on every authenticated request: `x-pubkey`,
`x-signature`, `x-timestamp`. If any are missing the response is also
`authentication_failed`. Never retry `authentication_failed`; it will
not succeed until the underlying cause (clock, key, payload) changes.

**Mixed server/client rollout:** SDK clients retry only
`authentication_replay`; they never retry `authentication_failed`. A replay CAS
reported under the older authentication code, or received by a client without
replay-specific retry handling, can therefore surface as a terminal 401 until
both sides use the same error contract.

The same server upgrade changes HTTP `/configure` failures from a
`ConfigureResponse` carrying `success: false` to the standard
`{ code, message, meta }` error envelope. Update direct HTTP integrations that
inspect the old body shape before rolling out the server; gRPC continues to use
the shared protobuf response.

### Pending proposals never resolve

A proposal stays `pending` until enough cosigners sign and someone
promotes it via `PushDelta`. If it sits too long:

- **Threshold not met.** Count signatures: the proposal needs `n` of `m`
  per the account configuration. Use `GetDeltaProposal` to see who has
  signed.
- **Pending limit reached.** `POST /delta/proposal` returns `409` with
  `code: pending_proposals_limit` once an account has
  `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` (default `20`) pending.
  Resolve or cancel some.
- **Canonicalization backlog.** Promoting a proposal to canonical depends
  on the candidate's matching Miden update being observed. Check the
  canonicalization worker logs and Miden RPC health.
- **Storage backend write failures.** A failing metadata backend will
  return `storage_error` on signing attempts. Check disk space (filesystem)
  or DB connectivity (Postgres).

### Candidates are being retained or discarded

Delta moves `candidate` → `retained` (default) or `candidate` →
`discarded` (retention disabled, or a client abandon). The cause is one
of:

1. The corresponding Miden proof was never submitted.
2. The proof was submitted but the on-chain commitment differs from the
   one Guardian acknowledged — usually because another device advanced
   the account state in parallel.
3. RPC endpoint targets the wrong network — Guardian polled the wrong
   ledger and never saw the update.
4. The canonicalization grace period (default 10 minutes) elapsed before
   the proof landed.

A `retained` delta is not final: the dedicated reconcile pass keeps
probing the chain (backing off as the row ages) and promotes it
automatically if the transaction ever shows up, for up to
`retained_ttl_seconds` (default 24 h). The `status_reason` on the
dashboard feed says which verdict parked it (`retry_exhausted` /
`diverged`); a `diverged` row that later reconciles means the
divergence verdict was spurious (e.g. a lagging RPC node).

Recovery for the client: check the delta's status first — if it flipped
to `canonical`, the transaction landed and there is nothing to redo.
Otherwise `GET /delta/since` → replay canonical chain → rebuild the
transaction → resubmit (this supersedes the retained row).

Operator checks:
- Canonicalization worker is running (look for `jobs::canonicalization`
  log lines).
- Miden RPC endpoint reachable (`rpc_unavailable` in logs indicates it
  isn't).
- No `network_error` storms.
- `guardian_canonicalization_candidates_total{outcome=...}` breaks down
  what the worker decided per candidate (`retained` is the default
  give-up path; `diverged` and `discarded` are the delete paths when
  retention is disabled; `stale_base` means a promotion was rolled back
  because the stored state moved mid-pass and will retry next tick;
  `reconciled` / `reconcile_deferred` / `reconcile_expired` are the
  reconcile pass resolving retained rows).
- `guardian_canonicalization_candidate_age_seconds` growing without
  bound means candidates are not converging — check Miden RPC health
  and the discard outcomes above.
- `guardian_canonicalization_fast_runs_total{outcome=...}` and
  `guardian_canonicalization_fast_run_duration_seconds` expose failures and
  latency of the promotion-only pass without changing the full-pass gauges,
  age histogram, or fetched-row counter.
- `guardian_canonicalization_reconcile_runs_total{outcome=...}` and
  `guardian_canonicalization_reconcile_run_duration_seconds` do the same
  for the recoverable-delta reconcile pass.
- `RUST_LOG=server::jobs::canonicalization=debug` emits one
  `Fast-promotion pass completed` summary per fast tick, including empty passes,
  with page, candidate, account-batch, deadline, and cursor-progress fields.
- Retention and reconciliation emit stable `event` / `reason` fields for
  log-based triage (with `account_id`, `nonce`, and `age_seconds` /
  `retention_reason` / `expires_at` where applicable):
  - `event=candidate_retained reason=retry_exhausted|diverged`
  - `event=reconcile_deferred reason=chain_at_stored_base|chain_probe_unavailable|end_state_not_on_chain|base_no_longer_applies|recomputed_commitment_mismatch|no_matching_recoverable_delta`
  - `event=reconcile_skipped reason=obsolete_base`
  - `event=reconcile_promoted`
  - `event=reconcile_expired`
  - `event=reconcile_superseded`
  The `chain_at_stored_base` / `chain_probe_unavailable` deferrals are
  logged (debug/info) but deliberately not counted in
  `guardian_canonicalization_candidates_total` — a healthy steady state
  probes every due account and finds the chain unmoved, and counting
  that would dwarf every other outcome.
- `guardian_canonicalization_commitment_mismatches_total` counting up
  means a client omitted `new_commitment` or claimed one that differs
  from the recomputed value. The full pass can promote using the value it
  independently verifies; the fast pass defers the candidate to that full
  path. In either case, investigate the client.

### `commitment_mismatch` on `PushDelta`

The client tried to apply a delta on top of a state Guardian doesn't
believe is current. Always recoverable:

```bash
GET /delta/since?account_id=...&nonce=<last-known-nonce>
```

Replay the returned canonical deltas locally, then resubmit your new
delta. This is the same pattern as a Git fast-forward.

### Stale state served by Guardian

Symptoms: client reads look "behind reality" relative to Miden. Causes:

- Canonicalization worker stalled (RPC down, DB write failures).
- Operator is intentionally censoring (run the
  [provider rotation flow](./CONCEPTS.md#provider-rotation)).
- Backend lag — Postgres replication or filesystem fsync latency.

Always compare against Miden before signing high-value transactions; see
the [client verification checklist](./CONCEPTS.md#client-verification-checklist).

### Account is paused

State-transition, proposal, and EVM mutation calls against the account
(`PushDelta`, `PushDeltaProposal`, `SignDeltaProposal`, and the matching
EVM proposal/session operations) return `409 GUARDIAN_ACCOUNT_PAUSED`
(gRPC `FailedPrecondition`). Reads and `ConfigureAccount` still work.

- **Confirm:** check `GET /dashboard/accounts/{id}` — paused accounts
  report `paused_at` and `paused_reason`.
- **Resume:** an operator holding `accounts:pause` calls
  `POST /dashboard/accounts/{id}/unpause` with an optional `{"reason": "..."}`
  body. The pause/unpause cycle is idempotent and audit-logged. See
  [`DASHBOARD.md`](./DASHBOARD.md#account-pausing).
- **Don't bypass:** the pause is enforced server-side at the metadata
  layer, not in the client. There is no env var or feature flag to
  disable it.

### Rate limits triggered

Over HTTP: `429` with `code: rate_limit_exceeded` and a `Retry-After`
header. Over gRPC: `RESOURCE_EXHAUSTED` with a `retry-after` metadata key
(seconds) and the same `rate_limit_exceeded` envelope in the status
details. The sustained limit is keyed per IP alone, so heavy gRPC
traffic (the Rust SDK and benchmark harness default) can exhaust the
sustained allowance for HTTP calls from the same client, and vice versa.
The burst limit is keyed per IP and endpoint, and HTTP paths never
collide with gRPC method names, so burst buckets are not shared.
The rejection counter `guardian_rate_limit_rejections_total` carries a
`transport` label to tell the two surfaces apart. Rate-limit rejections
happen before any handler runs, so retrying after the hint is always safe.

ALB gRPC health checks (`/guardian.Guardian/GetPubkey`) are metered like
any other traffic, keyed per ALB-node address. Their volume is far below
any sane budget, but a global limit below `GUARDIAN_MAX_REPLICAS`
partitions each replica's budget to zero and would fail health checks and
cycle tasks; the prod builder refuses to start in that configuration, dev
builds only warn.

If clients report throttling at request rates well below the configured
budget, check the `Request rate limited` lines' `client_ip` field. They
are logged at `debug` (rejections are expected traffic, and their volume
tracks the flood being shed), so enable them with
`RUST_LOG=info,server::middleware::rate_limit=debug`.
The proxy's address (or `unknown`) on every line means your ingress is
not forwarding the client address, so all clients share one budget:
common with unconfigured reverse proxies (nginx `grpc_pass` needs
explicit `grpc_set_header` for forwarding headers), Kubernetes
`externalTrafficPolicy: Cluster`, or L4 balancers without client-IP
preservation. See
[PRODUCTION.md](./PRODUCTION.md#running-behind-your-own-ingress-non-aws).

Server knobs (set on the task, not per-account):

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_RATE_LIMIT_ENABLED` | `true` | Set `false` only in test environments. |
| `GUARDIAN_RATE_BURST_PER_SEC` | `10` (dev), `200` (prod) | Requests per one-second window. |
| `GUARDIAN_RATE_PER_MIN` | `60` (dev), `5000` (prod) | Sustained rate. |
| `GUARDIAN_MAX_REQUEST_BYTES` | `1048576` (1 MB) | Reject larger bodies. |

If you legitimately need higher throughput, raise these via the deploy
script or Terraform variables rather than disabling rate limiting.

### Dashboard not reachable

- **Allowlist empty.** Without at least one operator entry, every
  challenge fails. Add an operator (see
  [`docs/DASHBOARD.md`](./DASHBOARD.md#enrolling-an-operator)).
- **Stale browser session.** Operator sessions are per-task. After a
  multi-task deploy, you may be routed to a task that did not issue your
  cookie. Re-authenticate.
- **`GUARDIAN_OPERATOR_PUBLIC_KEYS_*` env not set.** No source means no
  allowlist means the dashboard refuses every login. Check task env.

### Browser dashboard returns CORS errors

By default
([`middleware/cors.rs`](../crates/server/src/middleware/cors.rs)) the
server is permissive: when `GUARDIAN_CORS_ALLOWED_ORIGINS` is unset or
empty, every origin is allowed and credentials are **not** advertised
(useful for local dev). Setting the variable switches to a strict
credentialed allowlist:

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | unset (allow-any, no credentials) | Comma-separated explicit origins (e.g. `https://accounts.openzeppelin.com,https://admin.openzeppelin.com`). Wildcard `*` is rejected because credentialed CORS requires explicit origins. |

If the browser console shows a CORS preflight failure after deploy,
either (a) the origin isn't in the allowlist, or (b) the value still
contains `*` and the server failed startup — check task logs for the
`{ALLOWED_ORIGINS_ENV} must use explicit origins` error.

## Error code reference

All Guardian error responses carry a stable `code` string. Wire strings
come from
[`crates/server/src/error.rs:206-247`](../crates/server/src/error.rs#L206).

### Authentication and authorization

| Code | HTTP | First check |
|---|---|---|
| `authentication_failed` | 401 | Clock skew, invalid/unauthorized signature, modified payload, missing headers. Terminal; never retried. |
| `authentication_replay` | 401 | Correctly signed but the timestamp lost the per-signer replay CAS. `retryable: true`; SDK clients retry it automatically with a fresh timestamp and signature. See [the auth-layer section](#signed-requests-are-rejected-at-the-auth-layer). |
| `authorization_failed` | 403 | Account credentials don't authorize the operation. |
| `signer_not_authorized` | 403 | Signer isn't on the proposal's allowed signer set. |
| `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` | 403 | Operator dashboard call requires a permission the operator doesn't have. Response body carries `missing_permissions: string[]` (lex-sorted, deduplicated) and `retryable: false`. See [`DASHBOARD.md`](./DASHBOARD.md#permission-vocabulary). |

### Resource lookup

| Code | HTTP | First check |
|---|---|---|
| `account_not_found` | 404 | Account ID typo or `/configure` never called. |
| `state_not_found` | 404 | Account configured but no state pushed. |
| `delta_not_found` | 404 | Wrong account/nonce; check `GetDeltaSince`. |
| `proposal_not_found` | 404 | Proposal expired or already executed. |
| `account_data_unavailable` | 503 | Backend transient failure; retry. |

### Conflict and concurrency

| Code | HTTP | First check |
|---|---|---|
| `account_already_exists` | 409 | `/configure` called twice for the same account. |
| `conflict_pending_delta` | 409 | A non-canonical delta is in-flight; wait for it to finalise. |
| `conflict_pending_proposal` | 409 | Pending proposals exist; resolve before pushing a direct delta. |
| `pending_proposals_limit` | 409 | Account hit `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` (default 20). |
| `proposal_already_signed` | 409 | This signer already signed this proposal. |
| `GUARDIAN_ACCOUNT_PAUSED` | 409 (gRPC `FailedPrecondition`) | Account is paused by an operator. Response body includes the operator-supplied `paused_reason`. Unpause via `POST /dashboard/accounts/{id}/unpause` (requires `accounts:pause`). See [`DASHBOARD.md`](./DASHBOARD.md#account-pausing). |
| `GUARDIAN_ACCOUNT_RELEASED` | 409 (gRPC `FailedPrecondition`) | The account switched to a different guardian (a canonicalized `switch_guardian` delta moved the guardian key away from this server) and this server released it. Response body includes `released_at`. Reads keep working; mutations stay refused until the wallet re-onboards via `/configure`. |

### Validation

| Code | HTTP | First check |
|---|---|---|
| `invalid_input` | 400 | Generic validation failure; the message explains. |
| `invalid_account_id` | 400 | Malformed account ID. |
| `invalid_delta` | 400 | Delta payload failed schema or commitment validation. |
| `invalid_commitment` | 400 | Commitment string isn't a valid hex hash. |
| `commitment_mismatch` | 400 | `prev_commitment` doesn't match server's view; use `GetDeltaSince` to catch up. |
| `invalid_proposal_signature` | 400 | Signature doesn't verify against the proposal payload. |
| `invalid_network_config` | 400 | `Configure` payload's network config is malformed. |
| `invalid_cursor` | 400 | Pagination cursor doesn't decode. |
| `invalid_limit` | 400 | Pagination limit out of range. |
| `invalid_status_filter` | 400 | Status filter string isn't in `{candidate, canonical, retained, discarded}`. |
| `unsupported_for_network` | 400 | Endpoint not available for the account's network. |
| `unsupported_evm_chain` | 400 | EVM chain ID not in the configured allowlist. |
| `invalid_evm_proposal` | 400 | EVM proposal payload validation failed. |
| `insufficient_signatures` | 400 | Threshold not met for a multi-sig execute. |

### Network and infrastructure

| Code | HTTP | First check |
|---|---|---|
| `rpc_unavailable` | 502 | Miden RPC endpoint unreachable. Check the configured endpoint and Miden node health. |
| `rpc_validation_failed` | 502 | Miden RPC returned an error during validation. |
| `network_error` | 502 | Miden network call failed mid-flight. |
| `rate_limit_exceeded` | 429 | Backoff using the `Retry-After` header; tune `GUARDIAN_RATE_*` if legitimately needed. |
| `data_unavailable` | 503 | Cross-account aggregate degraded (filesystem backend above `DEFAULT_FILESYSTEM_AGGREGATE_THRESHOLD`). Distinct from `account_data_unavailable`, which is account-scoped. |

### Server-side

| Code | HTTP | First check |
|---|---|---|
| `storage_error` | 500 | Persistence backend rejected the write. Check disk (filesystem) or DB (Postgres) health. |
| `signing_error` | 500 | ACK signer failed. Check the keystore mount and Secrets Manager IAM. |
| `configuration_error` | 500 | Server misconfiguration. Almost always means a startup-time env var was wrong. |

## Logging and observability

The server emits `tracing` logs — `text` by default, `json` when `GUARDIAN_LOG_FORMAT=json` (see [`CONFIGURATION.md`](./CONFIGURATION.md#logging)). `text` uses ANSI colors only when stdout is a TTY; `json` emits flattened JSON with span context for CloudWatch Logs Insights.

Hot-path service handlers emit request events at `debug` — enable them with `RUST_LOG=server=debug` or `RUST_LOG=server::services=debug`. At the default `info` filter each request emits one span-close line instead, carrying the span's fields (account ID, nonce, commitment, signer/match counts) and `time.busy` / `time.idle`:

```
2026-08-19T11:53:31.172132Z  INFO push_delta_proposal{account_id="0x1234…" nonce=7 commitment="0xabcd…" signer_count=2}: server::services::push_delta_proposal: close time.busy=4.1ms time.idle=112µs
```

That line is emitted whether the request succeeded or failed. This matters because the centralized error lines (`guardian error (HTTP 5xx)`, `guardian error (gRPC internal)`) are emitted from the `GuardianError` → response conversion, which runs after the service span has closed: they carry `code` and `detail` only, and the immediately preceding close line is what identifies the account. Low-volume domain milestones (`Account configured`, `Delta proposal created`, `Delta proposal signed`) keep their own `info` line.

`resolve_account` is a nested helper rather than a request boundary, so its span is `debug` and it contributes no close line at `info`.

The per-read `Commitment mismatch during state verification` (`network::miden`) is also `debug`; persistent divergence is surfaced by the canonicalization processor's streak-gated WARN (confirmed divergence), not the per-read log.

Useful filters:

```bash
# Watch canonicalization worker decisions (including confirmed divergence WARN)
RUST_LOG=server::jobs::canonicalization=debug

# Watch per-request debug events
RUST_LOG=server=debug
RUST_LOG=server::services=debug

# Watch auth verifier rejections
RUST_LOG=server::middleware::auth=debug,server::metadata::auth=debug

# Watch dashboard authz
RUST_LOG=server::dashboard=debug

# JSON output for CloudWatch
GUARDIAN_LOG_FORMAT=json RUST_LOG=info cargo run -p guardian-server
```

### Startup configuration banner

At boot, before any listener binds, the server emits a one-shot summary
of its resolved, non-secret configuration (target
`server::builder::startup`). Use it to confirm which version, backends,
network, and signers a process is actually running:

```
===== Guardian server configuration =====
Guardian server starting version="0.1.0" git_sha="<sha>" profile="release"
network network=MidenTestnet rpc_endpoint="https://rpc.testnet.miden.io"
storage backend storage=Postgres
ack signers falcon="enabled" falcon_commitment=0x… ecdsa_backend="aws-kms" ecdsa_commitment=0x…
dashboard operators=0 cursor_secret="ephemeral"
canonicalization check_interval_seconds=10 fast_promotion_enabled=true fast_promotion_interval_seconds=3 fast_promotion_window_seconds=30 max_retries=48 submission_grace_period_seconds=600
listeners http=3000 grpc=50051
compiled features features=["postgres"]
=========================================
```

Notes:

- Only backend **kinds** and ports appear — never connection strings,
  KMS key ids, keystore paths, or credentials.
- `git_sha="unknown"` means the build received no `GUARDIAN_GIT_SHA`
  build arg and had no git working tree — expected for some Docker
  builds; not an error.
- `cursor_secret="ephemeral"` corroborates the separate
  `GUARDIAN_DASHBOARD_CURSOR_SECRET` warning: multi-replica deployments
  must set a stable shared secret (see
  [`CONFIGURATION.md`](./CONFIGURATION.md)).
- `canonicalization` absent (replaced by an "optimistic mode" line)
  means deltas are accepted without on-chain verification.

In ECS, container logs flow to the CloudWatch log group named
`/ecs/<stack>-server` ([`infra/data.tf:88`](../infra/data.tf#L88)). Use ECS Exec
to attach to a live task when needed:

```bash
aws ecs execute-command --cluster <stack>-cluster \
  --task <task-id> --container <stack>-server \
  --interactive --command "/bin/sh"
```

ECS Exec requires the task role's `ssmmessages:*` actions
([`infra/iam.tf:115`](../infra/iam.tf#L115)) — already granted by default.

### What an operator should watch

- **`candidate` deltas exceeding the canonicalization grace period** —
  indicates Miden submission isn't happening.
- **`discarded` delta rate** — small numbers are normal (race conditions);
  spikes mean RPC trouble or wrong network targeting.
- **`retained` delta count** — a persistently non-zero gauge means
  give-ups are outpacing reconciliation; check Miden RPC health and the
  `reconcile_*` outcomes above.
- **`rpc_unavailable` / `rpc_validation_failed` rates** — Miden node
  health.
- **`storage_error` rate** — DB or filesystem trouble.
- **`authentication_failed` rate** — sudden spike usually means a client
  clock drift event or an attacker probing.
- **ACK pubkey on `GET /pubkey`** — should not change unless you rotated.

There are no Terraform-managed dashboards or alarms yet — building these
out remains an open production-hardening item.

## When all else fails

1. Capture the server logs around the failing request (timestamps,
   request IDs, error codes).
2. Capture the client SDK version and the request envelope it built.
3. Compare against Miden directly — if Miden agrees with the client and
   Guardian disagrees, the operator probably has a stale or corrupted
   backend.
4. Open an issue at <https://github.com/OpenZeppelin/guardian/issues>
   with the request ID, error code, and log excerpt.
