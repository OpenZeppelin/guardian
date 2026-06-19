# Self-hosted Docker Compose with AWS-managed ACK signers

Run the Guardian server with Docker Compose where:

- **state** lives in Postgres (bundled in the Compose stack),
- the **Falcon** ACK key lives in **AWS Secrets Manager**, and
- the **ECDSA** ACK key lives in **AWS KMS** — its private key never enters the
  server process.

This is a self-hosted alternative to the ECS reference deployment
([`SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md)): you run the published
image on your own host but use the same AWS-managed signer backends. It is
distinct from [`LOCAL_DEV.md`](../../LOCAL_DEV.md), which keeps keys on the
filesystem.

Every setting is in one place — [`.env.example`](./.env.example) and
[`docker-compose.yml`](./docker-compose.yml) in this directory. For the
authoritative meaning of any variable, see
[`CONFIGURATION.md`](../../CONFIGURATION.md).

## Prerequisites

- Docker and the AWS CLI, with credentials that can create a KMS key and a
  Secrets Manager secret.
- The repo checked out (for this Compose file and the `scripts/aws-deploy.sh`
  bootstrap helpers).

## 1. Create the KMS ECDSA key

```bash
STACK_NAME=guardian ./scripts/aws-deploy.sh bootstrap-kms-ecdsa-key
```

This creates an `ECC_SECG_P256K1` / `SIGN_VERIFY` key plus an
`alias/guardian-ack-ecdsa` alias and prints the key ARN. The alias is a stable
handle you can use as `GUARDIAN_ACK_ECDSA_KMS_KEY_ID`.

## 2. Create the Falcon secret in Secrets Manager

```bash
export TF_VAR_guardian_ack_ecdsa_kms_key_arn="<arn-from-step-1>"
STACK_NAME=guardian ./scripts/aws-deploy.sh bootstrap-ack-keys
```

With the KMS ARN exported, `bootstrap-ack-keys` generates and stores **only the
Falcon** secret (default name `guardian/server/ack-falcon-secret-key`) and skips
ECDSA, since ECDSA is KMS-backed. See
[`runbooks/secrets.md`](../../runbooks/secrets.md#hosted-ecdsa-backend-aws-kms)
for key lifecycle and the immutable-spec caveat.

## 3. Configure the environment

From this directory:

```bash
cp .env.example .env
```

Set in `.env`:

- `POSTGRES_PASSWORD` — a strong, stable, URL-safe value.
- `AWS_REGION` — the region holding the secret and key.
- `GUARDIAN_ACK_FALCON_SECRET_ID` — the Falcon secret name from step 2.
- `GUARDIAN_ACK_ECDSA_KMS_KEY_ID` — the alias or ARN from step 1.

The Compose file already pins `GUARDIAN_ENV=prod` (the switch that makes the
server load ACK keys from Secrets Manager) and `GUARDIAN_ACK_ECDSA_BACKEND=aws-kms`.

### AWS credentials for the container

`GUARDIAN_ENV=prod` means the container calls Secrets Manager and KMS, so it
needs credentials. The Compose file passes them through from your shell:

```bash
export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... AWS_SESSION_TOKEN=...
```

For a long-lived host, prefer an EC2 instance role or container credentials over
static keys (leave the `AWS_*` keys unset and the SDK picks up the role).

## 4. Run

From this directory (Compose auto-discovers `docker-compose.yml` and `.env`):

```bash
docker compose up
```

At startup the server imports the Falcon key into its keystore and runs a KMS
sign probe to confirm `kms:Sign` works — it fails fast on a misconfigured key or
missing permission rather than at first acknowledgement. The logs include
`ECDSA ACK signer ready` with the active backend.

## 5. Validate

```bash
curl -s localhost:3000/pubkey | jq .
```

You should see the Falcon and ECDSA public keys and commitments. The ECDSA
commitment is derived from the KMS key; record it — moving to a different KMS
key later is a Guardian identity change (a `SwitchGuardian` migration for
existing accounts), not a routine rotation.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `configuration_error` mentioning the sign probe at startup | KMS key wrong spec (must be `ECC_SECG_P256K1` / `SIGN_VERIFY`), or the task lacks `kms:Sign` |
| Startup fails resolving the Falcon secret | `GUARDIAN_ACK_FALCON_SECRET_ID` wrong, or credentials can't read it / wrong `AWS_REGION` |
| `AWS_REGION` errors at boot | `GUARDIAN_ENV=prod` requires `AWS_REGION` to be set |

See [`TROUBLESHOOTING.md`](../../TROUBLESHOOTING.md) for the full error-code playbook.
