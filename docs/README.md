# Guardian Documentation

In-repo documentation for the Guardian server, its clients, and the AWS
deployment that runs it.

## Start here

- [Local development](./LOCAL_DEV.md) — running Guardian on your machine:
  filesystem vs Postgres, feature flags, examples, and end-to-end flow.
- [`spec/`](../spec/index.md) — protocol specification. Glossary
  (State, Delta, Nonce, Commitment), components, processes, and the API
  contract for each RPC.

## Architecture

- [Service architecture](./architecture/services.md) — module-level
  decomposition of the server, the Rust and TypeScript SDKs, storage
  modes, and the two authentication domains (per-account vs. operator).
- [AWS deployment architecture](./architecture/infra.md) — runtime topology
  of the AWS stack defined in [`infra/`](../infra/), with a resource
  inventory mapping every AWS resource to the Terraform file that owns it.

## Operations

- [Deploying Guardian Server to AWS ECS](./SERVER_AWS_DEPLOY.md) —
  end-to-end deploy workflow via `scripts/aws-deploy.sh`, stage profiles,
  Terraform variable reference, and troubleshooting.
- [Secrets and key management](./runbooks/secrets.md) — runbook for the
  five secret categories: bootstrap, rotation, compromise response.
- [`infra/README.md`](../infra/README.md) — Terraform-level reference for
  raw `terraform apply` workflows and variable defaults.

## SDKs and operator surfaces

- [Operator dashboard](./dashboard.md) — what the dashboard is, how
  operators enroll, the permission vocabulary, and how the local
  `operator-smoke-web` example wires it up.
- [Multisig SDK guide](./MULTISIG_SDK.md) — Rust and TypeScript multisig
  client usage: account creation, cosigner sync, proposal lifecycle, offline
  signing.

## Where else to look

- [`crates/server/proto/guardian.proto`](../crates/server/proto/guardian.proto)
  — authoritative wire contract for the gRPC API.
- [`examples/`](../examples) — runnable harnesses (`demo`, `smoke-web`,
  `operator-smoke-web`, `evm-smoke-web`, `web`) that exercise each SDK
  end-to-end.
- [`infra/`](../infra) — Terraform configuration for the AWS stack.
- [`scripts/aws-deploy.sh`](../scripts/aws-deploy.sh) — the deploy entry
  point that wires environment variables into Terraform.
