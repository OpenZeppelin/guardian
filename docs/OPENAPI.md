# OpenAPI specification

Guardian's HTTP API is described by an [OpenAPI 3.1](https://spec.openapis.org/oas/v3.1.0)
specification generated directly from the server source with
[`utoipa`](https://docs.rs/utoipa). Because the spec is derived from the
same `#[utoipa::path]` annotations and `#[derive(ToSchema)]` models the
handlers use, it cannot drift from the implementation — and CI fails if
the committed files fall out of sync (see below).

## Surfaces and files

Guardian exposes two HTTP surfaces — the **client** API (tag `client`)
consumed by SDKs/packages and the operator **dashboard** API (tag
`dashboard`) — plus the feature-gated EVM API (tag `evm`). Four specs are
committed under [`docs/`](.), generated with the `evm` feature so they
document every route:

| File | Contents | Maps to |
| --- | --- | --- |
| [`openapi.json`](./openapi.json) | combined client + dashboard + evm | served at runtime |
| [`openapi-client.json`](./openapi-client.json) | client API only | `packages/guardian-client` |
| [`openapi-dashboard.json`](./openapi-dashboard.json) | dashboard API only | `packages/guardian-operator-client` |
| [`openapi-evm.json`](./openapi-evm.json) | EVM API only | `packages/guardian-evm-client` |

Splitting per surface keeps SDK generation scoped and avoids exposing
unrelated schemas to a given client.

**Served at runtime:** `GET /api-docs/openapi.json` returns the combined
spec for the routes the running binary actually mounts (EVM routes appear
only when the server is built with `--features evm`).

Point any OpenAPI tooling — Swagger UI, ReDoc, or a client-SDK
generator — at any of these.

## Authentication

The specs declare security schemes so tools render auth correctly:

- **Client API** — three required `apiKey` headers, `x-pubkey`,
  `x-signature`, `x-timestamp` (see [`spec/api.md`](../spec/api.md)
  "Miden Request Signing"). Public endpoints (`/pubkey`) carry no
  requirement.
- **Dashboard API** — the `guardian_operator_session` cookie
  (`operator_session`). The login challenge/verify endpoints are public.
- **EVM API** — the `guardian_evm_session` cookie (`evm_session`). The
  challenge/verify endpoints are public.

## Regenerating the checked-in files

Run the `gen-openapi` binary with the `evm` feature, writing into
`docs/`:

```sh
cargo run --features evm --bin gen-openapi -- docs
```

To verify the committed files are current without writing (what CI runs):

```sh
cargo run --features evm --bin gen-openapi -- --check docs
```

Regenerate and commit the specs whenever you add or change an HTTP
handler, its request/response/query/path types, auth behavior, or a
model that appears on the wire — the same way the proto contract is kept
in sync. The `OpenAPI Spec Drift` CI job enforces this.
