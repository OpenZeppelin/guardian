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
5. [OpenAPI specification](./OPENAPI.md) — machine-readable OpenAPI 3.1
   spec ([`docs/openapi.json`](./openapi.json)) for Swagger UI / ReDoc /
   client generators.
6. [Troubleshooting](./TROUBLESHOOTING.md) — error code reference for
   anything your SDK surfaces.

### I want to *run* Guardian (deploy and operate)

You are an operator / SRE / DevOps.

1. [Concepts](./CONCEPTS.md) — same starting point; you need the trust
   model to make good ops decisions.
2. [Production guide](./PRODUCTION.md) — supported production shape,
   readiness checklist, and links to the detailed runbooks.
3. [Guides](./guides/README.md) — task-oriented, end-to-end walkthroughs
   for running Guardian in a specific mode (e.g. self-hosted Compose with
   AWS-managed signers).
4. [Deploying Guardian Server to AWS ECS](./SERVER_AWS_DEPLOY.md) —
   end-to-end deploy via `scripts/aws-deploy.sh`, stage profiles.
5. [AWS deployment architecture](./architecture/infra.md) — runtime
   topology, AWS resource inventory mapped to each `.tf` file.
6. [Configuration reference](./CONFIGURATION.md) — every env var in one
   place.
7. [Secrets and key management](./runbooks/secrets.md) — bootstrap,
   replacement, and compromise response for production secrets.
8. [Operator dashboard](./DASHBOARD.md) — what it is, enrolling
   operators, permission vocabulary, multi-task caveats.
9. [Troubleshooting](./TROUBLESHOOTING.md) — symptoms, error codes,
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
2. [Operator dashboard](./DASHBOARD.md) — auth domain, permission
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
- [OpenAPI specification](./OPENAPI.md) — HTTP API spec ([`openapi.json`](./openapi.json))
- [`spec/`](../spec/index.md) — protocol specification
- [`infra/README.md`](../infra/README.md) — Terraform variables

**Operations**
- [Production guide](./PRODUCTION.md)
- [Guides](./guides/README.md) — per-mode end-to-end walkthroughs
- [Deploying to AWS ECS](./SERVER_AWS_DEPLOY.md)
- [Secrets and key management](./runbooks/secrets.md)
- [Operator dashboard](./DASHBOARD.md)

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
