# Secrets and Key Management Runbook

Operational guide for the secrets Guardian relies on in production.
Companion to [`docs/architecture/infra.md`](../architecture/infra.md), which
explains *which* AWS resources hold each secret;
this doc covers *how* to bootstrap, replace, and respond to compromise.

> **Audience:** operators with AWS Secrets Manager and ECS write access for
> the target Guardian stack.

## Categories at a glance

| Category | Stored in | Lifecycle | Who reads it |
|---|---|---|---|
| `DATABASE_URL` | Secrets Manager (`<stack>/server/database-url`) | Managed by Terraform | ECS task **execution** role, at task start |
| RDS Proxy credentials (prod) | Secrets Manager (`<stack>/server/database-credentials`) | Managed by Terraform | RDS Proxy IAM role |
| ACK signing keys (prod) | Secrets Manager — IDs selected by `GUARDIAN_ACK_{FALCON,ECDSA}_SECRET_ID` env vars; default `guardian-prod/server/ack-{falcon,ecdsa}-secret-key`; Terraform sets per-stack `${stack_name}/server/ack-{falcon,ecdsa}-secret-key` | Bootstrapped once via `aws-deploy.sh bootstrap-ack-keys`; never rotated by deploys; replacement is incident/migration work | ECS task **runtime** role, at server startup |
| Operator public keys | Secrets Manager (Terraform-managed or pre-existing ARN) | Updated by editing Terraform var or rotating the secret value | ECS task runtime role, on each dashboard challenge **and each authenticated `/dashboard/*` request** (hot-reloaded — no restart needed) |
| EVM allowed chains + RPC URLs | Secrets Manager (Terraform-managed) | Updated by editing `config/evm/chains.json` and redeploying | ECS task execution role; surfaced as env to the task |

The ACK secret name is one value that travels through three places. They
have **different variable names by design** — each layer has a distinct
job — but they always carry the same string:

| Layer | Variable | Lives where | Job |
|---|---|---|---|
| 1. Deploy-time | `GUARDIAN_ACK_FALCON_SECRET_NAME` / `_ECDSA_SECRET_NAME` | Your shell when running `scripts/aws-deploy.sh` | Operator-facing override. The script passes it into Terraform. |
| 2. Terraform | `guardian_ack_falcon_secret_name` / `_ecdsa_secret_name` | [`infra/variables.tf`](../../infra/variables.tf), [`infra/data.tf:104-105`](../../infra/data.tf#L104) | Creates / looks up the Secrets Manager entry and renders the ECS task definition. |
| 3. Runtime | `GUARDIAN_ACK_FALCON_SECRET_ID` / `_ECDSA_SECRET_ID` | The ECS task env, read by [`secrets_manager.rs:10-13`](../../crates/server/src/ack/secrets_manager.rs#L10) | What the server actually consults at startup. |

Resolution order in each layer:

1. **Deploy env (`_SECRET_NAME`).** If unset, the deploy script falls
   through to `TF_VAR_guardian_ack_*_secret_name`, then to
   `${STACK_NAME}/server/ack-{falcon,ecdsa}-secret-key`
   ([`aws-deploy.sh:324-329`](../../scripts/aws-deploy.sh#L324)).
2. **Terraform variable.** If unset, defaults to
   `${stack_name}/server/ack-{falcon,ecdsa}-secret-key`. Renders the ECS
   task definition with `GUARDIAN_ACK_*_SECRET_ID` set
   ([`infra/ecs.tf:105-110`](../../infra/ecs.tf#L105)).
3. **Server runtime env (`_SECRET_ID`).** If unset (unusual — only
   happens in non-Terraform prod-mode launches), falls back to the
   code-level defaults `guardian-prod/server/ack-{falcon,ecdsa}-secret-key`.

There is a deliberate drift between the code default
(`guardian-prod/...`) and the Terraform default (`${stack_name}/...`,
no `-prod`). In the reference AWS deploy the server always reads the
Terraform-derived name because the ECS task definition always sets the
`_SECRET_ID` env var — the code default only matters for hand-rolled
prod-mode launches.

## ACK signing keys

ACK keys (one Falcon, one ECDSA) are Guardian's own response signers.
Clients pin Guardian's pubkey via `GetPubkey` on first contact and verify
every response thereafter — **stable identity matters**. Treat ACK key
replacement as a Guardian identity change, not a routine secret rotation.

For Miden multisig accounts, the Guardian commitment is also stored in
account state. Proposal execution checks the live server commitment
against that stored account commitment before using a new ACK signature.
If the Secrets Manager ACK values are replaced without moving accounts to
the new Guardian commitment, normal proposal execution for existing
accounts will fail with a Guardian commitment mismatch. A `SwitchGuardian`
proposal is the account-level migration path for changing that stored
commitment.

### Hosted ECDSA backend (AWS KMS)

The ECDSA ACK signer can be backed by AWS KMS instead of a Secrets Manager
secret, so the private key never enters the Guardian process. Set
`GUARDIAN_ACK_ECDSA_BACKEND=aws-kms` and `GUARDIAN_ACK_ECDSA_KMS_KEY_ID` to a KMS
key with spec `ECC_SECG_P256K1` and usage `SIGN_VERIFY`. Grant the ECS task role
`kms:GetPublicKey` and `kms:Sign` on the key (Terraform variable
`guardian_ack_ecdsa_kms_key_arn`). On this path the ECDSA secret in Secrets
Manager is not used and need not exist; Falcon is unaffected.

Provisioning, rotation, and deletion of the KMS key are performed with the
provider's own tooling — Guardian only signs with an existing key. Because a KMS
key is a distinct keypair, moving an existing deployment to KMS (or rotating the
KMS key) changes Guardian's ECDSA identity and is a Guardian identity change, not
a routine rotation: the same `SwitchGuardian` migration path described above
applies to existing accounts.

#### Create the key

The key spec and usage are immutable after creation, and they must match
exactly — the server fails its startup sign probe otherwise. Create the key
once, out of band, and keep its lifecycle separate from the Terraform stack so a
stack teardown never schedules the signing identity for deletion:

```bash
KEY_ID=$(aws kms create-key \
  --key-spec ECC_SECG_P256K1 \
  --key-usage SIGN_VERIFY \
  --description "Guardian <stack> ACK ECDSA signer" \
  --query KeyMetadata.KeyId --output text)

aws kms create-alias \
  --alias-name alias/<stack>-ack-ecdsa \
  --target-key-id "$KEY_ID"
```

Then pass the key ARN (or the alias ARN) to Terraform as
`guardian_ack_ecdsa_kms_key_arn`; the deploy grants the ECS task role
`kms:GetPublicKey` + `kms:Sign` on it and sets `GUARDIAN_ACK_ECDSA_BACKEND` /
`GUARDIAN_ACK_ECDSA_KMS_KEY_ID` on the server. KMS does not support automatic
rotation for asymmetric keys, which is correct here — rotating a signing
identity is the deliberate `SwitchGuardian` migration above, never automatic. To
retire a key, schedule deletion only after every account has migrated off the
old commitment.

### Bootstrap (first prod deploy)

```bash
DEPLOY_STAGE=prod STACK_NAME=<stack> ./scripts/aws-deploy.sh bootstrap-ack-keys
```

What that command does
([`scripts/aws-deploy.sh:352`](../../scripts/aws-deploy.sh#L352)):

1. Refuses to run if either secret already exists.
2. Generates key material locally via
   `cargo run --bin ack-keygen` (no key ever leaves the operator's host
   except via the `aws secretsmanager create-secret` call).
3. Creates both secrets in Secrets Manager with the generated values.

Verify. The deploy script resolves the active IDs as
`${GUARDIAN_ACK_*_SECRET_NAME:-${TF_VAR_guardian_ack_*_secret_name:-${STACK_NAME}/server/ack-*-secret-key}}`
([`aws-deploy.sh:324-329`](../../scripts/aws-deploy.sh#L324)). Mirror that
locally:

```bash
FALCON="${GUARDIAN_ACK_FALCON_SECRET_NAME:-${TF_VAR_guardian_ack_falcon_secret_name:-${STACK_NAME:-guardian}/server/ack-falcon-secret-key}}"
ECDSA="${GUARDIAN_ACK_ECDSA_SECRET_NAME:-${TF_VAR_guardian_ack_ecdsa_secret_name:-${STACK_NAME:-guardian}/server/ack-ecdsa-secret-key}}"

aws secretsmanager describe-secret --secret-id "$FALCON"
aws secretsmanager describe-secret --secret-id "$ECDSA"
```

Subsequent `aws-deploy.sh deploy` runs assert these secrets exist
([`aws-deploy.sh:331`](../../scripts/aws-deploy.sh#L331)) and fail fast
otherwise.

### Replacement

ACK replacement is **not** part of the regular deploy cycle and should not
be scheduled as a routine annual rotation. Use it when no accounts are
bound to the old Guardian commitment, when standing up a replacement
Guardian, or as part of incident response after suspected key exposure.

Before replacing ACK values for a live stack, decide how existing accounts
will be migrated:

- Prefer a `SwitchGuardian` flow that moves each account to a Guardian
  endpoint whose `GetPubkey` already returns the new commitment.
- For emergency compromise response, replacing the secret immediately
  stops the old identity from signing new ACKs, but existing accounts must
  still be moved to the new commitment before normal non-switch proposal
  execution resumes.
- Downstream clients that cache or pin Guardian identity must refetch
  `GetPubkey` after the change.

Procedure:

1. Generate new key material on a trusted host:
   ```bash
   cargo run --quiet --package guardian-server --bin ack-keygen > /tmp/ack-keys.json
   ```
2. Put new values into Secrets Manager — `update-secret` creates a new
   version without disturbing the secret ID. Reuse the same
   `$FALCON` / `$ECDSA` IDs you resolved in the Verify block above so
   multi-stack deploys hit the right secret:
   ```bash
   FALCON_VALUE=$(jq -r .falcon_secret_key /tmp/ack-keys.json)
   ECDSA_VALUE=$(jq -r .ecdsa_secret_key /tmp/ack-keys.json)

   aws secretsmanager update-secret \
     --secret-id "$FALCON" \
     --secret-string "$FALCON_VALUE"
   aws secretsmanager update-secret \
     --secret-id "$ECDSA" \
     --secret-string "$ECDSA_VALUE"
   ```
3. Force a new ECS deployment so tasks restart and import the new keys:
   ```bash
   aws ecs update-service --cluster <stack>-cluster \
     --service <stack>-server --force-new-deployment
   ```
4. Confirm the replacement:
   ```bash
   curl https://guardian.openzeppelin.com/pubkey
   ```
   Should return the new key material.
5. Securely shred `/tmp/ack-keys.json`.

### Compromise response

If you believe an ACK secret leaked:

1. **Immediately** replace the ACK values using the procedure above —
   bypass any change window.
2. Revoke any operator AWS credentials that could have read the secret
   (CloudTrail `GetSecretValue` events scoped to those secret ARNs are the
   audit trail).
3. Force-cycle all tasks (`update-service --force-new-deployment`) so the
   old keys are no longer resident in any task's filesystem keystore.
4. Move affected accounts to the new Guardian commitment with the
   account-level `SwitchGuardian` flow, or keep them paused/unavailable
   until that migration is complete.
5. Inform downstream clients to refetch the pubkey and invalidate cached
   verifiers.
6. File an incident referencing the secret ARN, the replacement timestamp,
   and the CloudTrail evidence.

## Operator public keys

Operators authenticate to the dashboard via Falcon-signed challenges
against an **allowlist** of public keys. Two ways to manage the list:

- **Terraform-managed** — set `guardian_operator_public_keys` in
  Terraform (or `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` in the deploy env);
  Terraform creates and maintains the secret. The variable is typed
  `list(string)`, so this path only supports the legacy bare-key
  array form — every entry implicitly gets `dashboard:read` only.
- **Pre-existing ARN** — set
  `guardian_operator_public_keys_secret_arn` to an ARN you manage
  externally. Terraform won't touch the contents. Use this path (or
  the local `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE`) when you need the
  object form to grant `accounts:pause` or any future permission.

The secret payload is the JSON shape consumed by
[`dashboard/allowlist.rs`](../../crates/server/src/dashboard/allowlist.rs).
See [`docs/DASHBOARD.md`](../DASHBOARD.md#enrolling-an-operator) for the
payload schema and an enrollment walkthrough.

### Adding or removing an operator

The server **rereads the operator secret on every dashboard challenge and
every authenticated `/dashboard/*` request** — no ECS restart needed for
allowlist changes.

Terraform-managed path:
1. Edit `guardian_operator_public_keys` (or the env var the deploy script
   reads from).
2. `./scripts/aws-deploy.sh deploy` — Terraform updates the secret in
   place. Effect is immediate.

Externally-managed path:
1. Update the secret with `aws secretsmanager update-secret`. That's it.

### Revoking a compromised operator

The hot-reload path makes this fast — no deploy window required:

```bash
aws secretsmanager update-secret \
  --secret-id <operator-secret-id> \
  --secret-string "$(cat new-operator-list.json)"
```

The next challenge issuance or authenticated request from any task picks
up the new list and rejects the revoked key. Active sessions belonging
to the revoked operator are rejected at their next call: the per-request
reload re-validates the operator against the current allowlist on every
authenticated `/dashboard/*` hit
([`dashboard/state.rs:284-324`](../../crates/server/src/dashboard/state.rs#L284)).

Operator sessions are in-memory per task. There is no ALB session
stickiness, so on multi-task deployments operators may be routed to a
task that did not mint their session and prompted to re-authenticate
— this is the normal failure mode, not a revocation signal. Use the
audit / CloudTrail trail (below) to confirm a revocation took effect.

## `DATABASE_URL` and RDS Proxy credentials

Both are **created and owned by Terraform** ([`infra/rds.tf:43`](../../infra/rds.tf#L43),
[`infra/rds.tf:48`](../../infra/rds.tf#L48)). Do not edit them by hand —
the next `terraform apply` will overwrite your change.

To rotate the database password:
1. Set `postgres_password` to a new value in `terraform.tfvars` (or unset
   it to let Terraform regenerate via `random_password`).
2. `terraform apply` — Terraform updates the RDS master password,
   `DATABASE_URL`, and the proxy credentials secret atomically.
3. ECS rolls the service automatically on the next deploy; force it
   sooner with `update-service --force-new-deployment`.

There is no separate read-only credential; the server connects with the
master user. This is a known production-hardening gap.

## EVM allowed chains and RPC URLs

Populated by the deploy script from
[`config/evm/chains.json`](../../config/evm/chains.json) when
`GUARDIAN_SERVER_FEATURES=postgres,evm`.

To add a chain:
1. Edit `config/evm/chains.json` — append a new entry to `chains` with
   `chainId`, `name`, `network`, and `rpcUrl`. The `entrypointAddress`
   is a single top-level field shared by every chain (exposed to the
   server as `GUARDIAN_EVM_ENTRYPOINT_ADDRESS`) — do not add it
   per-chain.
2. `./scripts/aws-deploy.sh deploy` — the script rebuilds the Secrets
   Manager values from the JSON and Terraform updates the secret
   versions.
3. ECS rolls and the new task reads the updated lists.

To rotate an RPC URL (e.g. switch provider):
1. Edit `chains.json`, redeploy. No special handling — the server treats
   chain config as a startup-time read.

## Audit trail

CloudTrail `GetSecretValue` events are scoped per-secret ARN. The
relevant principals you should see hitting each secret:

| Secret | Expected principals |
|---|---|
| `DATABASE_URL` | ECS task execution role only |
| `database-credentials` (proxy) | RDS Proxy IAM role only |
| ACK Falcon / ECDSA | ECS task runtime role (on cold start) + operators running `bootstrap-ack-keys` or emergency replacement |
| Operator pubkeys | ECS task runtime role + operators updating the list |
| EVM chains / RPCs | ECS task execution role only |

Any other principal touching these secrets is suspicious.

## What is deliberately not here

- **No KMS CMK** — Secrets Manager uses the default AWS-owned key. Move
  to a CMK before enabling cross-account access.
- **No automated rotation lambdas** — secret changes are operator-driven.
- **No envelope encryption of ACK secret values** — Secrets Manager
  protects the secret at rest; the value itself is the raw key material
  that the server imports into its filesystem keystore on startup.
