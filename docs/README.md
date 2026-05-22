# Guardian Documentation

In-repo documentation for the Guardian server, its clients, and the AWS
deployment that runs it.

## Start here

- [Concepts](./CONCEPTS.md) — what Guardian is, the custody model
  (3-key multisig), state and delta lifecycle, trust boundaries, failure
  and recovery model, provider rotation. **Read this first.**
- [Local development](./LOCAL_DEV.md) — running Guardian on your machine:
  filesystem vs Postgres, feature flags, examples, and end-to-end flow.
- [Troubleshooting](./TROUBLESHOOTING.md) — common failures organized by
  symptom, plus a stable error-code reference for every wire `code` the
  server returns.
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
- [Disaster recovery](./runbooks/restore.md) — restore checklist for
  RDS, ACK keys, Terraform state, and the operator allowlist.
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
