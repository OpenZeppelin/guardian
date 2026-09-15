# Guides

Task-oriented, end-to-end walkthroughs for running Guardian in a specific mode.
Each guide assembles a complete, copy-pasteable configuration in one place and
links to [`CONFIGURATION.md`](../CONFIGURATION.md) for the authoritative meaning
of each variable.

These differ from the other docs by intent:

- **Guides** (here) — "how do I run it set up like *this*?" end to end.
- [`CONFIGURATION.md`](../CONFIGURATION.md) — flat reference for every env var.
- [`SERVER_AWS_DEPLOY.md`](../SERVER_AWS_DEPLOY.md) — the ECS/Terraform deploy procedure.
- [`runbooks/`](../runbooks/) — operational procedures (secrets, incidents).

Guides use Docker Compose unless a guide says otherwise, so directory names
describe the *configuration* a guide demonstrates rather than repeating the
runner. Name a guide after what makes it distinct (its signer backends,
storage, or network), not after Compose.

## Available guides

| Guide | Mode |
|---|---|
| [Production deployment](./production/README.md) | Three tracks to the same hardened shape: AWS ECS/Fargate reference (`aws-deploy.sh` + Terraform, RDS + Secrets Manager + KMS, verified DB TLS, storage encryption, multi-replica HA); the published Docker image self-managed with no AWS (file-based ACK identity, your own TLS-verified Postgres, your ingress); and the Docker image with AWS Secrets Manager/KMS custody (only when ECS is not possible; the ECS reference is the recommended deployment) |
| [AWS-managed ACK signers](./aws-signers/README.md) | Self-hosted Compose: Postgres + Secrets Manager (Falcon) + KMS (ECDSA) |
| [Miden Dashboard UI](./miden-dashboard/README.md) | Self-hosted Compose: Postgres + Guardian server + the Miden Dashboard operator UI |
| [Observability](./observability/README.md) | Local Compose: server + Prometheus + pre-provisioned Grafana dashboard |
| [Horizontal scaling](./horizontal-scaling/README.md) | Local Compose: two replicas + round-robin proxy + shared Postgres (sessions, lease failover, fail-closed auth) |

## Adding a guide

Give each guide its own subdirectory holding a `README.md` and its committed,
runnable artifacts (e.g. `docker-compose.yml` + `.env.example`), so the guide
and the config you copy live together and the config can be smoke-tested. Keep
variable explanations in `CONFIGURATION.md` rather than restating them here.

The [Production deployment](./production/README.md) guide carries one
artifact set per track: its AWS track drives the real ECS/Terraform stack via
`scripts/aws-deploy.sh` + `infra/` (smoke = post-deploy validation against the
live stack); its self-managed Docker track ships `docker-compose.yml` +
`.env.example` + `smoke.sh` (runnable with no AWS credentials, and
`SMOKE_PULL_POLICY=missing` runs it against a locally built branch image, so it
is the candidate for a CI job; none exists yet); and its Docker + AWS custody track
(for when ECS is not possible) ships `docker-compose.aws-no-ecs.yml` + `.env.aws-no-ecs.example` (smoke =
`docker compose up` + `curl /pubkey`).
