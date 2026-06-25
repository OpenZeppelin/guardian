# Horizontal scaling: two replicas behind a proxy

Run two Guardian replicas behind a round-robin proxy, sharing one Postgres, and
watch the coordination layer (issue #242) work end to end on your laptop. This
mirrors the prod topology — 2–6 ECS tasks behind a load balancer — in miniature.

```text
                         ┌─────────────┐
   client ──▶  :8080 ──▶ │ proxy/Caddy │ ──round-robin──┬──▶ server-a :3000
                         └─────────────┘                └──▶ server-b :3010
                                                              │         │
                                                              └────┬────┘
                                                                   ▼
                                                            postgres :5432
                                            (sessions · challenges · worker lease)
```

Everything that must be shared for a correct multi-replica deployment lives in
Postgres, so a session minted on one replica is honored on the other, and only
one replica ever canonicalizes. The variable meanings live in
[`../../CONFIGURATION.md`](../../CONFIGURATION.md); the operational contract is in
[`../../runbooks/horizontal-scaling.md`](../../runbooks/horizontal-scaling.md).

## Prerequisites

- Docker with Compose v2 (`docker compose`).
- That's it — no AWS. ACK signing keys are generated locally per replica, which
  is fine here because this guide exercises operator/EVM auth and coordination,
  not the multisig co-signing flow (which is the only thing that needs one
  shared ACK key — see [From this demo to production](#from-this-demo-to-production)).

## Configure and run

```sh
cp .env.example .env
# Set a real cursor secret — it MUST be identical on every replica:
#   openssl rand -hex 32   →   paste into GUARDIAN_DASHBOARD_CURSOR_SECRET
cp operators.example.json operators.json   # empty allowlist `[]`; add a key for the login walkthrough (step 7)
```

> **Building an unreleased version?** The published `latest` image does not yet
> contain these coordination changes (issue #242). Drop in the local-build
> override so the replicas build from the repo-root `Dockerfile` instead of
> pulling from the registry — Compose auto-merges it, so no extra flags are
> needed on any command:
>
> ```sh
> cp docker-compose.override.yml.example docker-compose.override.yml
> ```
>
> Once the change ships in a published image, delete
> `docker-compose.override.yml` to go back to the registry image.

```sh
docker compose up -d --build
```

The proxy is at <http://localhost:8080>. Each replica is also exposed directly —
`server-a` on `:3000`, `server-b` on `:3010` — so you can target a specific
replica during the walkthrough. Postgres is on `:5432`.

## What is shared (and why)

| Shared in Postgres | Table | Effect across replicas |
|---|---|---|
| Operator/EVM sessions | `auth_sessions` | Log in on A, your cookie works on B; logout is honored fleet-wide. |
| Login challenges | `auth_challenges` | A challenge is single-use even if issued on A and verified on B. |
| Canonicalization lease | `worker_leases` | Exactly one replica promotes candidates; the others stand by. |

Coordination is **backend-derived**: it is on because the backend is Postgres.
No environment variable enables or disables it.

## Validation walkthrough

### 1. Both replicas report shared coordination

```sh
docker compose logs server-a server-b | grep -i "coordination mode"
```

Each replica prints one line; both must read `mode=shared backend=postgres`. If
you ever see `mode=single-process backend=filesystem`, that replica is **not**
safe to run alongside others.

### 2. Exactly one canonicalization lease holder

```sh
docker compose exec postgres \
  psql -U guardian -d guardian \
  -c "select lease_name, holder_id, fence_token from worker_leases;"
```

You get a single `canonicalization` row with one `holder_id` (formatted
`{pid}-{random}`) — never two. Both replicas run the worker loop, but only the
lease holder does work; the other keeps trying to acquire and backs off.

### 3. Lease failover with a fencing-token bump

Stop the current holder and watch a different replica take over within the lease
TTL (~30s, i.e. 3× the 10s canonicalization interval):

```sh
docker compose stop server-a       # if A wasn't the holder, stop server-b instead
watch -n2 'docker compose exec -T postgres \
  psql -U guardian -d guardian \
  -c "select holder_id, fence_token, expires_at from worker_leases;"'
```

`holder_id` changes to the surviving replica and `fence_token` **increments** —
the increment is the steal signal a superseded holder uses to fence itself off
at its next write. Bring the replica back with `docker compose start server-a`;
the lease does not bounce back (the current holder keeps renewing).

### 4. Proxy request failover

The lease failover above is server-side; the proxy also has to stop routing
*client* requests to a dead replica. That is what the `health_uri` / `lb_*`
directives in the [`Caddyfile`](./Caddyfile) do — a bare `round_robin` (no health
checks) keeps sending half the traffic to the dead replica and returns `502`.
Kill a replica and hit the proxy:

```sh
docker compose stop server-b
for i in $(seq 1 4); do curl -s -o /dev/null -w "%{http_code} " \
  http://localhost:8080/pubkey; done; echo
```

Every response stays `200` — Caddy health-checks each replica and routes only to
the survivor. Bring it back with `docker compose start server-b`; Caddy re-adds
it within one health interval (~5s). (Strip the health directives from the
`Caddyfile` and the same loop returns alternating `502`s.)

### 5. Auth fails closed when the shared store is down

Pause Postgres and watch the holder step down rather than barrel ahead:

```sh
docker compose pause postgres
docker compose logs -f server-a server-b   # Ctrl-C after a few seconds
```

You will see lease renew/acquire failures and storage errors — the worker
**cancels its pass** instead of canonicalizing blind. If you have completed the
login walkthrough below, an authenticated request fails rather than silently
succeeding: authentication is **fail-closed**.

> `docker compose pause` freezes Postgres mid-connection (SIGSTOP), so an
> in-flight request *hangs until it times out* rather than getting a prompt
> `5xx`. Either way it never succeeds. To see a fast `5xx` instead (socket
> closed → connection refused), use `docker compose stop postgres` and
> `docker compose start postgres` to recover.

Recover with:

```sh
docker compose unpause postgres
```

Coordination resumes automatically; no manual intervention.

### 6. Rate-limit partitioning and `X-Forwarded-For`

Each replica enforces `global / GUARDIAN_MAX_REPLICAS`. With the default global
burst of 10 and `GUARDIAN_MAX_REPLICAS=2`, a single replica caps at ~5 req/s.
Hammer one replica directly (the challenge endpoint is unauthenticated and
rate-limited):

```sh
for i in $(seq 1 12); do
  curl -s -o /dev/null -w "%{http_code} " \
    "http://localhost:3000/auth/challenge?commitment=0xdemo"
done; echo
```

After the per-replica burst is spent you see `429`s. Through the proxy
(`:8080`), Caddy sets `X-Forwarded-For`, so the server keys the limit on your
real client IP rather than the proxy address — confirm by repeating the loop
against `http://localhost:8080/...` and seeing the same per-IP behavior.

### 7. (End-to-end) An operator session survives losing its replica

This is the headline, and it needs a real operator key to sign the challenge.
Use the [`examples/operator-smoke-web`](../../../examples/operator-smoke-web)
harness (or the operator client) pointed at the **proxy** URL
`http://localhost:8080`:

1. Generate a Falcon operator key with the harness and add its public key to
   `operators.json` (replacing the empty `[]`); the allowlist hot-reloads, so no
   restart is needed:

   ```json
   [{ "public_key": "0x<falcon-operator-pubkey>", "permissions": ["dashboard:read"] }]
   ```
2. Complete the login (`GET /auth/challenge` → sign → `POST
   /auth/verify`). The proxy round-robins, so this may land on either
   replica; the session row is written to `auth_sessions`.
3. Make an authenticated request (e.g. `GET /dashboard/accounts`) a few times —
   each may be served by a different replica, and all succeed: the cookie is
   validated against the shared store, not per-process memory.
4. Now `docker compose stop` the replica that handled your login and repeat —
   **your session still works** on the survivor. Then `POST
   /auth/logout`; the revocation is honored on every replica.

## Cleanup

```sh
docker compose down -v        # -v also drops the Postgres + keystore volumes
```

## From this demo to production

This guide stays AWS-free to be runnable; a real prod deployment differs in two
ways that do not change the coordination behavior shown above:

- **`GUARDIAN_ENV=prod`** activates the prod-stage startup guards — a filesystem
  storage backend and a rate limit that partitions to 0 req/replica are each
  refused at startup. (An unset `GUARDIAN_DASHBOARD_CURSOR_SECRET` only *warns* —
  it degrades cross-replica dashboard pagination, not custody, so a
  single-replica prod server still boots.) Note these guards live behind the ACK
  registry init, which in prod requires AWS first: set `GUARDIAN_ENV=prod`
  without `AWS_REGION` and the server refuses to start with `AWS_REGION is
  required when GUARDIAN_ENV=prod` before it ever reaches the storage or
  rate-limit checks — so observing those two specifically needs AWS configured.
- **One shared ACK signing key.** Each replica here auto-generates its own
  guardian ACK key into its local keystore — and in non-prod it does so on
  *every* startup, so the identity is not even stable across a single replica's
  restart. That is fine for auth + coordination, which is all this guide
  exercises. It is **not** enough for the multisig co-signing flow: every replica
  must present the *same* guardian identity, because each account pins the
  guardian's `/pubkey` commitment into its `openzeppelin::guardian::public_key`
  slot at configure time. Route a multisig flow through the round-robin proxy and
  the replica that did not configure the account rejects it with
  `invalid GUARDIAN public key binding`. So prod pins one ACK key via AWS Secrets
  Manager — see the [aws-signers guide](../aws-signers/README.md). Per-account
  state already lives in Postgres and needs nothing extra.

> **Smoke-testing multisig against this demo?** Until a stable non-AWS identity
> lands ([issue #289](https://github.com/OpenZeppelin/guardian/issues/289) — a
> local file/env signer key, so every replica can share one identity without
> AWS), point your client at a **single replica directly**
> (`http://localhost:3000`), never the proxy (`:8080`). Also make the client's
> Miden RPC network match the server's `GUARDIAN_NETWORK_TYPE` (e.g. devnet RPC
> ↔ `MidenDevnet`), or canonicalization will loop on an `on_chain=0x00…0`
> commitment because the account was deployed to a different network than the
> guardian verifies against.

The managed path (published Postgres image + the prod Terraform profile) sets
all of this for you; see [`../../SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md)
and the [horizontal-scaling runbook](../../runbooks/horizontal-scaling.md).
