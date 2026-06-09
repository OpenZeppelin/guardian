# Verified database TLS with Docker Compose

Run the Guardian server against a **TLS-enabled Postgres** and have the server
**verify the database certificate** — not just encrypt the connection. The
bundled Postgres terminates TLS with a self-signed certificate; the server
connects with `sslmode=verify-full` and validates that certificate against a CA
you generate.

This guide is the local, self-contained way to see Guardian's database TLS
verification accept a good certificate and reject a bad one. It complements
[`LOCAL_DEV.md`](../../LOCAL_DEV.md) (which uses a plaintext Postgres) and the
AWS reference deployment in
[`SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md) (which verifies the RDS
certificate the same way). For the authoritative meaning of `sslmode` /
`sslrootcert`, see [Database TLS](../../CONFIGURATION.md#database-tls).

Everything is in this directory: [`docker-compose.yml`](./docker-compose.yml),
[`.env.example`](./.env.example), and [`generate-certs.sh`](./generate-certs.sh).

> **Image version:** verified database TLS ships in the server image; run a
> `GUARDIAN_VERSION` that includes it. Older images don't parse `verify-full` /
> `sslrootcert` and fail to connect (or connect without verifying), so this guide
> only works on an image that includes the feature. Before it's released, build
> one locally — see [Run against a local build](#run-against-a-local-build).

## How it fits together

- `generate-certs.sh` creates a throwaway CA and a Postgres server cert whose
  **SAN is `postgres`** — the Compose service name the server dials — so
  `verify-full` hostname matching succeeds.
- A one-shot `db-cert-init` service fixes the key's owner/permissions into a
  shared volume (Postgres rejects a world-readable or wrong-owner key), then
  exits; Postgres starts only after it succeeds. This mirrors the init-container
  pattern the ECS deployment uses to deliver the CA bundle.
- The server mounts only `ca.pem` (the public trust anchor) and connects with
  `sslmode=verify-full&sslrootcert=/etc/guardian/tls/ca.pem`.

## Prerequisites

- Docker (with Compose) and OpenSSL.
- The repo checked out (for this Compose file and the cert script).

## 1. Generate the certificates

```bash
./generate-certs.sh
```

Writes `certs/ca.pem`, `certs/server.crt`, `certs/server.key` (gitignored).

## 2. Configure the environment

```bash
cp .env.example .env
```

Set `POSTGRES_PASSWORD` to a strong, URL-safe value. Optionally pin
`GUARDIAN_VERSION` and set `DB_SSLMODE` (default `verify-full`).

## 3. Run

```bash
docker compose up
```

Order is enforced by Compose: `db-cert-init` writes the key → Postgres starts
with `ssl=on` and becomes healthy → the server starts, runs migrations, and
opens its connection pool. The server verifies the Postgres certificate on both
the migration (libpq) and pool (rustls) connections.

## 4. Validate

A clean startup (no certificate error in the logs, migrations applied, pool
ready) means verification passed. Confirm the server is up:

```bash
curl -s localhost:3000/pubkey | jq .
```

## 5. Experiment with the verification levels

Stop the stack (`docker compose down`) and change `.env`, then `up` again:

- **`DB_SSLMODE=verify-ca`** — the chain is still verified, but the hostname is
  not. Connection still succeeds (the CA is trusted).
- **Untrusted CA** — point the trust anchor at an unrelated CA to see a refusal:
  generate a second CA (`openssl req -x509 -newkey rsa:2048 -nodes -keyout other.key -out other.pem -subj "/CN=Other" -days 1`)
  and bind-mount `other.pem` over `/etc/guardian/tls/ca.pem`. The server **fails
  fast** at startup with a certificate-verification error — it does **not** fall
  back to an unverified connection.
- **Hostname mismatch** — re-run `generate-certs.sh` after editing it to use a
  different SAN (e.g. `DNS:not-postgres`). Under `verify-full` the connection is
  **refused**; switch to `DB_SSLMODE=verify-ca` and the same cert is **accepted**
  (hostname isn't checked at that level).

## Run against a local build

Until the feature is in a published image, build the server image from the repo
and point the guide at it. From the repo root:

```bash
docker build --target server-runner -t guardian:dbtls-local .
```

Then in `.env` (the `postgres` feature is compiled in by default):

```bash
GUARDIAN_IMAGE=guardian:dbtls-local
GUARDIAN_PULL_POLICY=missing
```

`docker compose up` now runs your local image. (`GUARDIAN_PULL_POLICY=missing`
stops Compose from trying to pull the local-only tag.)

## Cleanup

```bash
docker compose down -v   # also removes the Postgres + cert volumes
```

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| Postgres exits citing key permissions | `generate-certs.sh` not run, or `certs/` not readable by `db-cert-init`; re-run step 1 |
| Server error naming `sslrootcert` | `certs/ca.pem` missing or not mounted; re-run step 1 |
| Server refuses with a certificate-verification error | trust anchor doesn't match the server cert, or (under `verify-full`) the cert SAN ≠ `postgres` |
| Works under `verify-ca`, fails under `verify-full` | hostname/SAN mismatch — the server cert's SAN must be `postgres` |

See [`TROUBLESHOOTING.md`](../../TROUBLESHOOTING.md#server-fails-to-start) for the
database-TLS failure-to-cause mapping.
