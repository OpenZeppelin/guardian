# Guardian Documentation

In-repo documentation for the Guardian server, its clients, and the AWS
deployment that runs it.

If you only read one thing first, read [Concepts](./CONCEPTS.md) — it
explains what Guardian is, the custody model, and the state/delta
lifecycle that informs every other doc here.

---

## Find your path

### I want to *use* Guardian (build apps against it)

You are an SDK consumer or integrator.

1. [Concepts](./CONCEPTS.md) — primitives, lifecycle, trust model,
   client verification checklist.
2. [Quickstart](./QUICKSTART.md) — Guardian running locally in 60 seconds.
3. [Multisig SDK guide](./MULTISIG_SDK.md) — Rust + TypeScript multisig
   client: account creation, proposal lifecycle, offline signing.
4. [`spec/api.md`](../spec/api.md) — wire-level API contract (auth
   headers, request signing, data shapes).
5. [Troubleshooting](./TROUBLESHOOTING.md) — error code reference for
   anything your SDK surfaces.

### I want to *run* Guardian (deploy and operate)

You are an operator / SRE / DevOps.

1. [Concepts](./CONCEPTS.md) — same starting point; you need the trust
   model to make good ops decisions.
2. [Deploying Guardian Server to AWS ECS](./SERVER_AWS_DEPLOY.md) —
   end-to-end deploy via `scripts/aws-deploy.sh`, stage profiles.
3. [AWS deployment architecture](./architecture/infra.md) — runtime
   topology, AWS resource inventory mapped to each `.tf` file.
4. [Configuration reference](./CONFIGURATION.md) — every env var in one
   place.
5. [Secrets and key management](./runbooks/secrets.md) — bootstrap,
   rotation, compromise response for the five secret categories.
6. [Operator dashboard](./dashboard.md) — what it is, enrolling
   operators, permission vocabulary, multi-task caveats.
7. [Troubleshooting](./TROUBLESHOOTING.md) — symptoms, error codes,
   recovery procedures.

### I want to *develop on* Guardian (work in this repo)

You are a contributor.

1. [`CONTRIBUTING.md`](../CONTRIBUTING.md) — picking work, branching,
   commit style, cross-layer change rules, testing, docs, CLA.
2. [Concepts](./CONCEPTS.md) — the system you are about to change.
3. [Service architecture](./architecture/services.md) — module-level
   decomposition, storage modes, dashboard subsystem, consumer surfaces.
4. [Local development](./LOCAL_DEV.md) — four launch paths, feature
   flags, example harnesses, test invocations.
5. [Configuration reference](./CONFIGURATION.md) — what each env var
   does and which Cargo feature reads it.
6. [`AGENTS.md`](../AGENTS.md) — the contract-change workflow and
   operational guide. Mandatory reading before touching the wire
   contract.
7. [Troubleshooting](./TROUBLESHOOTING.md) — when your local server
   misbehaves.
8. [`spec/`](../spec/index.md) — the formal protocol spec: definitions,
   components, per-RPC processes.

### I want to *integrate* my own operator dashboard or harness

1. [Concepts](./CONCEPTS.md) — the trust model the dashboard sits inside.
2. [Operator dashboard](./dashboard.md) — auth domain, permission
   vocabulary, allowlist payload shapes.
3. [`examples/operator-smoke-web`](../examples/operator-smoke-web/README.md)
   — reference harness.

---

## Full index

**Start here**
- [Concepts](./CONCEPTS.md)
- [Quickstart](./QUICKSTART.md)
- [Local development](./LOCAL_DEV.md)
- [Troubleshooting](./TROUBLESHOOTING.md)

**Architecture**
- [Service architecture](./architecture/services.md)
- [AWS deployment architecture](./architecture/infra.md)

**Reference**
- [Configuration (env vars)](./CONFIGURATION.md)
- [`spec/`](../spec/index.md) — protocol specification
- [`infra/README.md`](../infra/README.md) — Terraform variables

**Operations**
- [Deploying to AWS ECS](./SERVER_AWS_DEPLOY.md)
- [Secrets and key management](./runbooks/secrets.md)
- [Operator dashboard](./dashboard.md)

**SDKs**
- [Multisig SDK guide](./MULTISIG_SDK.md)

**Contributing**
- [`CONTRIBUTING.md`](../CONTRIBUTING.md)
- [`AGENTS.md`](../AGENTS.md)
- [`SECURITY.md`](../SECURITY.md)

---

## Beyond this directory

- [`crates/server/proto/guardian.proto`](../crates/server/proto/guardian.proto)
  — authoritative wire contract for the gRPC API.
- [`examples/`](../examples) — runnable harnesses (`demo`, `smoke-web`,
  `operator-smoke-web`, `evm-smoke-web`, `web`) that exercise each SDK
  end-to-end.
- [`infra/`](../infra) — Terraform configuration for the AWS stack.
- [`scripts/aws-deploy.sh`](../scripts/aws-deploy.sh) — deploy entry
  point that wires env vars into Terraform.
