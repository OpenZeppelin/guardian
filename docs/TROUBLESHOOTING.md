# Troubleshooting

Common Guardian failures and how to resolve them. Organised by symptom
first, then by error code.

For concepts (lifecycle, trust model, recovery flows) see
[`docs/CONCEPTS.md`](./CONCEPTS.md).
For local-dev setup see [`docs/LOCAL_DEV.md`](./LOCAL_DEV.md).

## By symptom

### Server fails to start

Most startup failures are environment misconfiguration. Check in order:

1. **`DATABASE_URL` missing under `--features postgres`.** The builder
   panics with `"DATABASE_URL environment variable is required"`. Either
   set it or rebuild without the `postgres` feature.
2. **Filesystem paths not writable.** Filesystem builds use
   `GUARDIAN_STORAGE_PATH`, `GUARDIAN_METADATA_PATH`, and
   `GUARDIAN_KEYSTORE_PATH` when set, defaulting to
   `/var/guardian/storage`, `/var/guardian/metadata`, and
   `/var/guardian/keystore` respectively. Startup fails if the process
   cannot create or write to those paths — common on dev machines where
   `/var/guardian` doesn't exist or isn't owned by the running user.
   Either set the env vars to a writable location or `mkdir -p`
   `/var/guardian/{storage,metadata,keystore}` with the right
   permissions.
3. **Postgres migrations fail.** The Postgres path runs migrations at
   startup. If the DB user lacks `CREATE` permissions, startup fails.
   Grant `CREATE` on the schema or run migrations as a privileged user.
4. **ACK secrets missing in prod.**
   `scripts/aws-deploy.sh deploy` refuses to apply if either ACK secret
   is missing. Run `DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys`
   first (see [Secrets runbook](./runbooks/secrets.md#bootstrap-first-prod-deploy)).
5. **Operator allowlist source not set.** If you intend to use the
   dashboard, set `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID` (prod) or
   `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` (local). Without either, the
   dashboard is unreachable.

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

This section covers the auth-middleware verdict — `401` with
`code: authentication_failed`. If your request was *authenticated* but
still rejected (e.g. `403 authorization_failed`,
`403 signer_not_authorized`, `400 commitment_mismatch`,
`409 conflict_pending_*`), jump straight to the
[error code reference](#error-code-reference) — the signature was fine,
the service layer rejected the operation.

The auth middleware returns `401 authentication_failed` for three
reasons:

1. **Clock skew.** Timestamps must be within ±5 minutes of server time
   ([`metadata/auth/credentials.rs:6`](../crates/server/src/metadata/auth/credentials.rs#L6)).
   Sync the client clock (NTP) or check for container time drift.
2. **Reused timestamp.** The server enforces *strictly monotonic*
   timestamps per public key. If you replay an old request, or two
   in-flight requests share a millisecond, the second is rejected.
   Generate fresh timestamps per request and serialise concurrent
   requests from the same key.
3. **Modified payload after signing.** The signature covers the request
   body hash. Mutating the body (proxy reformatting, JSON re-serialise)
   invalidates the signature. Sign-then-send; do not transform between.

Headers required on every authenticated request: `x-pubkey`,
`x-signature`, `x-timestamp`. If any are missing the response is also
`authentication_failed`.

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

### Candidates are being discarded

Delta moves `candidate` → `discarded`. The cause is one of:

1. The corresponding Miden proof was never submitted.
2. The proof was submitted but the on-chain commitment differs from the
   one Guardian acknowledged — usually because another device advanced
   the account state in parallel.
3. RPC endpoint targets the wrong network — Guardian polled the wrong
   ledger and never saw the update.
4. The canonicalization grace period (default 10 minutes) elapsed before
   the proof landed.

Recovery for the client: `GET /delta/since` → replay canonical chain →
rebuild the transaction → resubmit.

Operator checks:
- Canonicalization worker is running (look for `jobs::canonicalization`
  log lines).
- Miden RPC endpoint reachable (`rpc_unavailable` in logs indicates it
  isn't).
- No `network_error` storms.

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

Every write call against an account returns `409 GUARDIAN_ACCOUNT_PAUSED`
(gRPC `FailedPrecondition`). Reads still work.

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

`429` with `code: rate_limit_exceeded` and a `Retry-After` header.

Server knobs (set on the task, not per-account):

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_RATE_LIMIT_ENABLED` | `true` | Set `false` only in test environments. |
| `GUARDIAN_RATE_BURST_PER_SEC` | `10` (dev), `200` (prod) | Token-bucket burst. |
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
| `authentication_failed` | 401 | Clock skew, reused timestamp, modified payload, missing headers. |
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
| `invalid_status_filter` | 400 | Status filter string isn't in `{candidate, canonical, discarded}`. |
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

The server emits structured JSON logs via `tracing`. Useful filters:

```bash
# Watch canonicalization worker decisions
RUST_LOG=server::jobs::canonicalization=debug

# Watch auth verifier rejections
RUST_LOG=server::middleware::auth=debug,server::metadata::auth=debug

# Watch dashboard authz
RUST_LOG=server::dashboard=debug
```

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
