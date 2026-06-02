# Deploying GUARDIAN Server to AWS ECS

This guide covers the current AWS deployment for Guardian. The AWS stack now uses Amazon RDS for PostgreSQL and no longer supports the legacy ECS-hosted Postgres runtime.

The deployment surface supports two stage profiles:
- `DEPLOY_STAGE=dev` keeps the current low-cost, fixed-capacity behavior
- `DEPLOY_STAGE=prod` enables ECS autoscaling, RDS storage autoscaling, RDS Proxy, larger default RDS sizing, and benchmark-oriented runtime defaults

## Published Docker images

Prebuilt, versioned server images are published to the GitHub Container Registry
(GHCR) at `ghcr.io/openzeppelin/guardian`, so you can pull a known-good image
instead of building from source:

```bash
docker pull ghcr.io/openzeppelin/guardian:<version>   # e.g. v1.2.3, or latest
```

Images are multi-architecture (`linux/amd64` + `linux/arm64`) and fully
runtime-configurable — every setting and secret is supplied at run time, never
baked in (see [`docs/CONFIGURATION.md`](./CONFIGURATION.md)):

```bash
docker run --rm -p 3000:3000 -p 50051:50051 \
  --env-file ./guardian.env \
  ghcr.io/openzeppelin/guardian:<version>
```

To run the published image with a Postgres backend locally, use the registry
compose file (no local build):

```bash
cp .env.registry.example .env.registry          # then set POSTGRES_PASSWORD in .env.registry
GUARDIAN_VERSION=<version> docker compose --env-file .env.registry -f docker-compose.registry.yml up
```

The stack is driven entirely by the gitignored `.env.registry` (see
`.env.registry.example`): Compose reads `POSTGRES_PASSWORD` / `GUARDIAN_VERSION` from
it for interpolation (via `--env-file`), and the server container loads it for
runtime config. The shared repo `.env` (AWS/deploy config) is intentionally not
used here, so this example never mutates it. The repo's default
`docker-compose.yml` (and the `docker-compose.postgres.yml` override) instead
build the server from source for contributors; `docker-compose.registry.yml` pulls
the published image.

Maintainers publish a version by running the **Docker Publish** GitHub Actions
workflow (manual dispatch: pick the branch to build from and the version to tag).
The AWS deploy below still builds and pushes to ECR via `scripts/aws-deploy.sh`;
consuming the published GHCR image from the deploy flow is a separate, later change.

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/downloads) >= 1.0
- AWS CLI configured with permissions for ECS, ECR, ELB, EC2, IAM, CloudWatch, RDS, and Secrets Manager
- Docker installed locally
- `jq` installed locally when deploying with `GUARDIAN_SERVER_FEATURES=postgres,evm`

```bash
aws sts get-caller-identity
docker info
terraform version
```

## Quick Start

```bash
aws sso login --profile <your-profile>

set -a && source .env && set +a

# Optional: build/deploy ARM64 instead of X86_64
# export CPU_ARCHITECTURE=ARM64

# Optional: pin the server to a specific Miden network
export GUARDIAN_NETWORK_TYPE=MidenDevnet

# Optional: allow dashboard operators and let Terraform create the secret
# export GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON='["0x<alice-falcon-public-key>","0x<bob-falcon-public-key>"]'

# Optional: use an existing dashboard operator public keys secret instead
# export GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN='arn:aws:secretsmanager:us-east-1:123456789012:secret:guardian/operators'

# Optional: enable EVM support from config/evm/chains.json
# export GUARDIAN_SERVER_FEATURES=postgres,evm
# export GUARDIAN_EVM_CHAIN_CONFIG_FILE=config/evm/chains.json
# export GUARDIAN_CORS_ALLOWED_ORIGINS=https://accounts.openzeppelin.com

# Optional: choose the deployment profile
export DEPLOY_STAGE=dev
# export DEPLOY_STAGE=prod

# Optional: override the stack base name or public hostname
export STACK_NAME=guardian
# export SUBDOMAIN=guardian-stg

aws sts get-caller-identity
./scripts/aws-deploy.sh deploy
./scripts/aws-deploy.sh status
```

For a reviewable deployment, split image publishing, planning, and applying:

```bash
./scripts/aws-deploy.sh build
./scripts/aws-deploy.sh plan
./scripts/aws-deploy.sh deploy --skip-build
./scripts/aws-deploy.sh status
```

This builds and pushes `${ECR_REPO_NAME}:latest`, plans Terraform against the immutable digest currently behind that tag, then applies using the existing ECR image without rebuilding. If you push a new image after `plan`, rerun `plan` before `deploy --skip-build`.

## Terraform Variables

If you need to override defaults, use `infra/terraform.tfvars`:

```hcl
aws_region = "us-east-1"

# Optional: ECS/image architecture
# cpu_architecture = "X86_64"
# cpu_architecture = "ARM64"

# Optional: derive resource names from a base stack name
# stack_name = "guardian"

# Only set this when bypassing scripts/aws-deploy.sh. The deploy script resolves
# ECR latest to an immutable digest and passes server_image_uri via -var.
# server_image_uri = "123456789012.dkr.ecr.us-east-1.amazonaws.com/guardian-server@sha256:<digest>"

# Optional: Postgres credentials (defaults derive from stack_name)
# postgres_db       = "guardian"
# postgres_user     = "guardian"
# postgres_password = "guardian_dev_password"

# Optional: managed database sizing overrides
# Stage defaults:
# - dev  -> db.t3.micro, 20 GiB allocated, no storage autoscaling ceiling
# - prod -> db.t3.medium, 50 GiB allocated, 200 GiB max allocated
# rds_instance_class = "db.t3.medium"
# rds_allocated_storage = 50
# rds_max_allocated_storage = 200

# Optional: Miden network for the server runtime
# server_network_type = "MidenDevnet"

# Optional: dashboard operator Falcon public keys managed by Terraform
# guardian_operator_public_keys = [
#   "0x<alice-falcon-public-key>",
#   "0x<bob-falcon-public-key>",
# ]

# Optional: existing dashboard operator Falcon public keys secret
# guardian_operator_public_keys_secret_arn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:guardian/operators"

# Optional: EVM runtime configuration
# guardian_evm_allowed_chain_ids = "1,11155111"
# guardian_evm_rpc_urls = "1=https://ethereum-rpc.publicnode.com,11155111=https://ethereum-sepolia-rpc.publicnode.com"
# guardian_evm_entrypoint_address = "0x433709009b8330fda32311df1c2afa402ed8d009"
# guardian_cors_allowed_origins = "https://accounts.openzeppelin.com"

# Optional: stage/runtime capacity overrides
# deployment_stage = "prod"
# server_desired_count = 2
# server_autoscaling_enabled = true
# server_autoscaling_min_capacity = 2
# server_autoscaling_max_capacity = 6
# server_autoscaling_cpu_target = 65
# server_autoscaling_memory_target = 75
# rds_proxy_enabled = true
# rds_proxy_subnet_ids = ["subnet-xxxxxxxx", "subnet-yyyyyyyy"]
# In us-east-1, avoid subnets in us-east-1e/use1-az3 for RDS Proxy.
# guardian_rate_limit_enabled = false
# guardian_rate_burst_per_sec = 200
# guardian_rate_per_min = 5000
# guardian_db_pool_max_size = 32
# guardian_metadata_db_pool_max_size = 32

# Optional: Route 53 hosted zone ID
# route53_zone_id = "Z1234567890ABC"

# Optional: Cloudflare DNS management
# cloudflare_zone_id = "..."
# cloudflare_api_token = "..."
```

## Deploy

### Script Commands

| Command | Purpose |
| --- | --- |
| `./scripts/aws-deploy.sh build` | Build the Guardian server image and push it to ECR as `latest`. Does not run Terraform. |
| `./scripts/aws-deploy.sh plan` | Run `terraform plan` using the immutable digest currently behind ECR `latest`. Does not build, push, or apply. |
| `./scripts/aws-deploy.sh deploy` | Build and push the image, resolve ECR `latest` to an immutable digest, and run `terraform apply`. |
| `./scripts/aws-deploy.sh deploy --skip-build` | Resolve the existing ECR `latest` image to an immutable digest and run `terraform apply` without rebuilding. |
| `./scripts/aws-deploy.sh bootstrap-ack-keys` | Create the prod ACK key secrets in Secrets Manager. Refuses to overwrite existing secrets. |
| `./scripts/aws-deploy.sh status` | Print Terraform outputs for the active `STACK_NAME` and `DEPLOY_STAGE`. |
| `./scripts/aws-deploy.sh logs` | Tail the deployed server's CloudWatch log group. |
| `./scripts/aws-deploy.sh cleanup` | Run Terraform destroy for the active `STACK_NAME` and `DEPLOY_STAGE`. |

`--skip-build` is meaningful for `deploy`; `plan` never builds or pushes an image. Use `build` before `plan` for a new stack or whenever ECR does not yet contain `${ECR_REPO_NAME}:latest`.

### One-Step Deploy

```bash
./scripts/aws-deploy.sh deploy
```

The deploy script resolves the ECR `latest` tag to an immutable digest before calling Terraform, so image pushes always produce a real ECS task-definition revision instead of relying on tag reuse.
It also keeps separate local Terraform state files per `STACK_NAME` and `DEPLOY_STAGE`, using `infra/terraform.<stack>.<stage>.tfstate` by default.

AWS deployments must include the `postgres` server feature. The script defaults `GUARDIAN_SERVER_FEATURES` to `postgres`; set `GUARDIAN_SERVER_FEATURES=postgres,evm` only when deploying the optional EVM API surface.

### Reviewable Build, Plan, Apply

Use this flow when you want to inspect Terraform changes before applying them:

```bash
./scripts/aws-deploy.sh build
./scripts/aws-deploy.sh plan
./scripts/aws-deploy.sh deploy --skip-build
```

`build` creates the ECR repository if needed and pushes `${ECR_REPO_NAME}:latest`. Both `plan` and `deploy --skip-build` resolve that tag to an immutable digest before invoking Terraform. Do not rebuild or push a new `latest` between `plan` and `deploy --skip-build` unless you intend to apply a different image; rerun `plan` after any rebuild.

For `DEPLOY_STAGE=prod`, bootstrap the ACK secrets once before the first deploy:

```bash
DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys
```

The normal deploy path does not create or rotate ACK keys. It expects the prod Secrets Manager entries to already exist, and the server reads them directly at startup before importing them into the filesystem keystore.

Secret names default to `${STACK_NAME}/server/ack-{falcon,ecdsa}-secret-key`, so distinct stacks (e.g. `guardian-prod`, `guardian-prod-eu`) automatically resolve to distinct secrets and multiple Guardian deployments can coexist in the same AWS account. Override per stack by setting `GUARDIAN_ACK_FALCON_SECRET_NAME` / `GUARDIAN_ACK_ECDSA_SECRET_NAME` before `bootstrap-ack-keys` and `deploy`; they flow into Terraform variables and the ECS task definition's `GUARDIAN_ACK_FALCON_SECRET_ID` / `GUARDIAN_ACK_ECDSA_SECRET_ID` env vars.

Dashboard operator public keys use a separate optional secret. The easiest
deployment path is to pass the public keys to Terraform and let it create the
stack-scoped secret:

```bash
export GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON='["0x<alice-falcon-public-key>","0x<bob-falcon-public-key>"]'
```

or in `terraform.tfvars`:

```hcl
guardian_operator_public_keys = [
  "0x<alice-falcon-public-key>",
  "0x<bob-falcon-public-key>"
]
```

If you already manage the secret outside this stack, pass its ARN through
`GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN` or
`guardian_operator_public_keys_secret_arn`. An explicit secret ARN takes
precedence over the Terraform-managed public key list.

The ECS task role is granted read access only to the configured secret ARN. The
server rereads that secret during operator auth checks, so adding or removing a
key in the existing secret takes effect without an application restart. When
Terraform manages the secret, update the public key list and rerun deploy.

EVM deployments need the `evm` server feature plus server-owned chain config.
By default, `scripts/aws-deploy.sh` derives allowed chain IDs, RPC URLs, and the
shared EntryPoint address from `config/evm/chains.json`. It passes RPC URLs to
Terraform as a stack-scoped Secrets Manager secret and the EntryPoint address as
a normal ECS environment variable. To use an alternate JSON file, set
`GUARDIAN_EVM_CHAIN_CONFIG_FILE`.

You can still override the derived values by setting
`GUARDIAN_EVM_ALLOWED_CHAIN_IDS`, `GUARDIAN_EVM_RPC_URLS`, or
`GUARDIAN_EVM_ENTRYPOINT_ADDRESS` directly, or by passing existing secret ARNs
through `GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN` and
`GUARDIAN_EVM_RPC_URLS_SECRET_ARN`.

When an EVM UI runs on a different origin, set
`GUARDIAN_CORS_ALLOWED_ORIGINS` to a comma-separated list of exact origins.
Wildcard origins are rejected. When this value is configured, the server enables
credentialed CORS so browsers can include the host-only, `HttpOnly`
`guardian_evm_session` cookie.

If you still have an older local state file at `infra/terraform.tfstate`, move it manually before using the split-state workflow:

```bash
cp infra/terraform.tfstate infra/terraform.guardian.dev.tfstate
cp infra/terraform.tfstate.backup infra/terraform.guardian.dev.tfstate.backup 2>/dev/null || true
```

Use `--skip-build` when the image already exists in ECR and you only need infra/runtime changes, or when you are applying immediately after a reviewed `plan`:

```bash
./scripts/aws-deploy.sh deploy --skip-build
```

For benchmark-oriented production deploys, prefer explicit overrides rather than changing the base prod profile in code. A typical starting point is:

```bash
set -a && source .env && set +a

export DEPLOY_STAGE=prod
export STACK_NAME=guardian-prod
export TF_VAR_server_cpu=2048
export TF_VAR_server_memory=4096
export TF_VAR_server_desired_count=3
export TF_VAR_server_autoscaling_min_capacity=3
export TF_VAR_server_autoscaling_max_capacity=10
export TF_VAR_rds_instance_class=db.r6g.large
export TF_VAR_rds_allocated_storage=100
export TF_VAR_rds_max_allocated_storage=400
export TF_VAR_rds_proxy_subnet_ids='["subnet-25c1722b","subnet-4d0eca6c"]'
export TF_VAR_guardian_db_pool_max_size=64
export TF_VAR_guardian_metadata_db_pool_max_size=64
export TF_VAR_guardian_rate_limit_enabled=false

./scripts/aws-deploy.sh deploy --skip-build
```

## Validate

```bash
./scripts/aws-deploy.sh status
curl https://guardian.openzeppelin.com/pubkey
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian.openzeppelin.com:443 guardian.Guardian/GetPubkey
```

## Operations

### Logs

```bash
./scripts/aws-deploy.sh logs
```

### Status

```bash
./scripts/aws-deploy.sh status
```

The script reads the state file for the active `STACK_NAME` and `DEPLOY_STAGE`. The current default path is:

```text
infra/terraform.<stack>.<stage>.tfstate
```

You can override that with `TF_STATE_PATH` if needed.

### Destroy

```bash
./scripts/aws-deploy.sh cleanup
```

ECR repositories are not managed by Terraform:

```bash
aws ecr delete-repository --repository-name guardian-server --force --region us-east-1
```

## Resources Created

| Resource | Description |
|----------|-------------|
| ECS Cluster | Fargate cluster derived from `stack_name` |
| ECS Service | Guardian server service |
| Application Load Balancer | Internet-facing ALB derived from `stack_name` |
| Target Groups | HTTP target group for port `3000` and gRPC target group for port `50051` |
| RDS | Managed PostgreSQL instance and subnet group |
| RDS Proxy | Managed PostgreSQL proxy in the production profile |
| Secrets Manager | Secret containing `DATABASE_URL` for the server task |
| Secrets Manager | Optional operator public keys secret for dashboard auth |
| Secrets Manager | Optional EVM allowed chain IDs and RPC URLs secrets |
| Secrets Manager | Secrets containing the Falcon and ECDSA ack private keys used to seed the server keystore in prod |
| Security Groups | ALB, server, and database security groups |
| CloudWatch Log Groups | Cluster execute-command logs and server logs |
| IAM Role | ECS task execution and runtime roles |

## Outputs

| Output | Description |
|--------|-------------|
| `alb_dns_name` | ALB DNS name |
| `alb_url` | Full ALB URL |
| `custom_domain_url` | Custom domain URL when configured |
| `grpc_endpoint` | Public gRPC endpoint when HTTPS is enabled |
| `database_endpoint` | RDS endpoint used by the server |
| `rds_proxy_endpoint` | RDS Proxy endpoint when enabled |
| `rds_instance_class` | Effective RDS instance class |
| `rds_allocated_storage` | Effective allocated RDS storage in GiB |
| `database_url_secret_arn` | Secrets Manager ARN for the server `DATABASE_URL` |
| `operator_public_keys_secret_arn` | Secrets Manager ARN used for dashboard operator public keys |
| `operator_public_keys_secret_name` | Terraform-managed operator public keys secret name, when created |
| `guardian_evm_allowed_chain_ids_secret_arn` | Secrets Manager ARN used for EVM allowed chain IDs |
| `guardian_evm_rpc_urls_secret_arn` | Secrets Manager ARN used for EVM RPC URLs |
| `guardian_evm_entrypoint_address` | Shared EVM EntryPoint address configured for the server |
| `guardian_cors_allowed_origins` | Explicit CORS origins configured for the server |
| `ack_falcon_secret_name` | Secrets Manager name for the Falcon ack key |
| `ack_ecdsa_secret_name` | Secrets Manager name for the ECDSA ack key |
| `ecs_cluster_arn` | ECS cluster ARN |
| `server_service_arn` | Server ECS service ARN |

## Stage Profiles

### Dev

- single ECS task
- no ECS autoscaling
- direct ECS to RDS connection
- no RDS Proxy
- conservative Guardian runtime limits

### Prod

- higher ECS desired count
- ECS service autoscaling
- larger default RDS instance class and base storage
- RDS storage autoscaling
- RDS Proxy between ECS and RDS
- higher Guardian runtime rate-limit and DB-pool defaults for benchmark traffic

## HTTPS And gRPC

HTTPS is enabled when `acm_certificate_arn` is set. DNS can be managed through Cloudflare, Route 53, or both depending on which variables are provided.

When HTTPS is enabled, the ALB routes standard HTTPS requests to the server HTTP port `3000` and gRPC requests for `/guardian.Guardian/*` to the server gRPC port `50051`. The public gRPC endpoint uses the same hostname on port `443`.

On Apple Silicon hosts, `CPU_ARCHITECTURE=X86_64` builds are slower because Docker builds `linux/amd64` images under emulation. Switching to `ARM64` avoids that local emulation cost, but it also changes the ECS task runtime architecture.

## Migrating An Existing ECS-Postgres Stack

The current Terraform configuration is RDS-only. There is no supported dual-mode deployment that keeps the old ECS Postgres service alive after apply.

Use this cutover flow for an existing stack:

1. Capture the current stack state:
   ```bash
   ./scripts/aws-deploy.sh status
   ```
2. Create a logical PostgreSQL backup from the existing ECS-hosted database before applying the updated stack.
3. Apply the updated RDS-backed Terraform stack:
   ```bash
   ./scripts/aws-deploy.sh deploy --skip-build
   ```
4. Restore the backup into the new RDS database.
5. Validate the public service:
   ```bash
   ./scripts/aws-deploy.sh status
   curl https://<host>/pubkey
   grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' <host>:443 guardian.Guardian/GetPubkey
   ```
6. Confirm the old Postgres ECS service and Cloud Map database-discovery resources are gone from AWS before treating the cutover as complete.

## Troubleshooting

- If the server task fails during startup, check `./scripts/aws-deploy.sh logs` first and confirm the reported `database_endpoint` matches the expected RDS host.
- If a prod deploy fails before Terraform starts, confirm the fixed prod ACK secrets exist by running `./scripts/aws-deploy.sh bootstrap-ack-keys` once and then retrying the deploy.
- If RDS subnet-group creation fails, verify the selected subnets cover at least two subnets for the database deployment.
- If gRPC works against the ALB directly but fails on the public hostname, check Cloudflare gRPC settings on the zone.

## Legacy Script

The legacy deployment logic has been replaced by the Terraform-backed `scripts/aws-deploy.sh`.
