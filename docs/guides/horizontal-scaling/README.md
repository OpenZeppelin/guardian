# Horizontal scaling: two replicas behind a proxy

Run two Guardian replicas behind a round-robin proxy, sharing one Postgres, and
watch the coordination layer (issue #242) work end to end on your laptop. This
mirrors the prod topology — 2–6 ECS tasks behind a load balancer — in miniature.

```text
              :8080  (HTTP)  ┌─────────────┐                  server-a :3000 (HTTP)
   client ──▶                │ proxy/Caddy │ ──round-robin──┬──▶       :50052 (gRPC)
              :50051 (gRPC)  └─────────────┘                └──▶ server-b :3010 (HTTP)
                                                                       :50053 (gRPC)
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
- The Rust toolchain and `jq`, used once to generate the shared ACK signing
  keys (next section).
- No AWS anywhere. The replicas share one Guardian identity through two local
  key files (`GUARDIAN_ACK_SECRET_PROVIDER=file`) — the non-AWS stable identity
  from [issue #289](https://github.com/OpenZeppelin/guardian/issues/289).

## Configure and run

```sh
cp .env.example .env
# Set a real cursor secret — it MUST be identical on every replica:
#   openssl rand -hex 32   →   paste into GUARDIAN_DASHBOARD_CURSOR_SECRET
cp operators.example.json operators.json   # empty allowlist `[]`; add a key for the login walkthrough (step 8)
```

Generate **one** ACK keypair that both replicas mount (this is the fleet's
Guardian identity — see [What is shared](#what-is-shared-and-why)). The files
must be owner-only or the server refuses to start:

```sh
mkdir -p ack-keys
cargo run --quiet -p guardian-server --bin ack-keygen \
  | { read -r json; \
      jq -rj '.falcon_secret_key' <<<"$json" > ack-keys/ack-falcon-secret-key; \
      jq -rj '.ecdsa_secret_key'  <<<"$json" > ack-keys/ack-ecdsa-secret-key; }
chmod 600 ack-keys/ack-falcon-secret-key ack-keys/ack-ecdsa-secret-key
```

`ack-keys/` is git-ignored. Treat it like any private key material — and note
that regenerating it changes the Guardian's identity, freezing any multisig
account that pinned the old one (see the
[secrets runbook](../../runbooks/secrets.md#self-hosted-stable-identity-without-aws)).

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

gRPC mirrors the HTTP layout: the proxy round-robins plaintext gRPC (h2c) on
`:50051` — which is the Rust demo CLI's default endpoint, so its `[1] Local
gRPC` choice goes through the load balancer — with the replicas reachable
directly on `:50052` (A) and `:50053` (B).

## What is shared (and why)

| Shared in Postgres | Table | Effect across replicas |
|---|---|---|
| Operator/EVM sessions | `auth_sessions` | Log in on A, your cookie works on B; logout is honored fleet-wide. |
| Login challenges | `auth_challenges` | A challenge is single-use even if issued on A and verified on B. |
| Canonicalization lease | `worker_leases` | Exactly one replica promotes candidates; the others stand by. |
| Replay protection | `account_auth_state` | A request timestamp accepted on A cannot be replayed to B; each per-account timestamp is usable exactly once fleet-wide. |

Coordination is **backend-derived**: it is on because the backend is Postgres.
No environment variable enables or disables it.

**Upgrading across schema migrations**: migrations run automatically at
startup, and the first replica to boot a new binary migrates the shared
database for the whole fleet. A replica still running the previous binary can
then fail closed — its queries name columns the migration removed, so it
serves errors instead of authenticating against stale state — until it is
replaced. Plan rolling deploys accordingly: old replicas may error (never
misbehave) during the window between the first new-binary boot and the last
replica replacement.

One shared thing lives **outside** Postgres: the ACK signing keys. Both
replicas mount the same `./ack-keys` files and load them via
`GUARDIAN_ACK_SECRET_PROVIDER=file`, so the fleet presents a single,
restart-stable Guardian identity at `/pubkey`. That matters for multisig: each
account pins the guardian's `/pubkey` commitment into its
`openzeppelin::guardian::public_key` slot at configure time, and a replica
whose identity doesn't match rejects co-signing with `invalid GUARDIAN public
key binding`. With per-replica ephemeral keys (the non-prod default), routing
multisig through the proxy would fail on every request the "wrong" replica
answered.

## Validation walkthrough

### 1. Both replicas report shared coordination

```sh
docker compose logs server-a server-b | grep -i coordination
```

Each replica prints one line; both must read `mode=shared backend=postgres`. If
you ever see `mode=single-process backend=filesystem`, that replica is **not**
safe to run alongside others.

### 2. One Guardian identity across the fleet

```sh
diff <(curl -s http://localhost:3000/pubkey) <(curl -s http://localhost:3010/pubkey) \
  && echo "identical"
```

Both replicas serve a byte-identical `/pubkey` response because they load the
same `./ack-keys` files, and the identity survives restarts: `docker compose
restart server-a` and run the diff again — still identical. (Remove the
`GUARDIAN_ACK_*` variables from `docker-compose.yml` and each replica mints its
own ephemeral key per boot; the diff then fails and multisig through the proxy
breaks — see [What is shared](#what-is-shared-and-why).)

### 3. Exactly one canonicalization lease holder

```sh
docker compose exec postgres \
  psql -U guardian -d guardian \
  -c "select lease_name, holder_id, fence_token from worker_leases;"
```

You get a single `canonicalization` row with one `holder_id` (formatted
`{pid}-{random}`) — never two. Both replicas run the worker loop, but only the
lease holder does work; the other keeps trying to acquire and backs off.

### 4. Lease failover with a fencing-token bump

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

### 5. Proxy request failover

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

### 6. Auth fails closed when the shared store is down

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

### 7. Rate-limit partitioning and `X-Forwarded-For`

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

### 8. (End-to-end) An operator session survives losing its replica

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

`./ack-keys` is a host directory, so `down -v` leaves it alone — keep it and the
Guardian identity survives a full teardown. Delete it only to discard that
identity for good (any account configured against it would need a
`SwitchGuardian` migration).

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
- **Where the shared ACK key lives.** Both setups give the fleet one guardian
  identity — the property multisig needs, since each account pins the
  guardian's `/pubkey` commitment at configure time. This guide sources it from
  two local files (`GUARDIAN_ACK_SECRET_PROVIDER=file`, the `./ack-keys` bind
  mount); prod sources the same key material from AWS Secrets Manager
  (`GUARDIAN_ENV=prod` defaults the provider to `aws`), so no secret sits on a
  task's disk. The formats are identical — the hex strings `ack-keygen` emits —
  so keys are portable between the two providers; see the
  [aws-signers guide](../aws-signers/README.md) and the
  [secrets runbook](../../runbooks/secrets.md#ack-signing-keys). Per-account
  state already lives in Postgres and needs nothing extra.

> **Smoke-testing multisig against this demo?** The shared `file`-provider
> identity makes the round-robin proxy safe for the full multisig flow —
> configure, co-sign, and execute can each land on a different replica. Point
> HTTP clients at `:8080` and gRPC clients (the Rust demo CLI) at `:50051`,
> its default. Just make the client's Miden RPC network match the server's
> `GUARDIAN_NETWORK_TYPE` (e.g. devnet RPC ↔ `MidenDevnet`), or
> canonicalization will loop on an `on_chain=0x00…0` commitment because the
> account was deployed to a different network than the guardian verifies
> against.

The managed path (published Postgres image + the prod Terraform profile) sets
all of this for you; see [`../../SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md)
and the [horizontal-scaling runbook](../../runbooks/horizontal-scaling.md).
