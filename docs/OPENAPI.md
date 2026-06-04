# OpenAPI specification

Guardian's HTTP API is described by an [OpenAPI 3.1](https://spec.openapis.org/oas/v3.1.0)
specification generated directly from the server source with
[`utoipa`](https://docs.rs/utoipa). Because the spec is derived from the
same `#[utoipa::path]` annotations and `#[derive(ToSchema)]` models the
handlers use, it cannot drift from the implementation.

It covers both HTTP surfaces:

- the **client** API (tag `client`) consumed by the SDKs and packages, and
- the operator **dashboard** API (tag `dashboard`).

The EVM smart-account API (tag `evm`) is included when the `evm` Cargo
feature is enabled.

## Where it lives

- **Checked-in spec:** [`docs/openapi.json`](./openapi.json) — generated
  with the `evm` feature so it documents every route.
- **Served at runtime:** `GET /api-docs/openapi.json` on the HTTP server
  returns the spec for the routes the running binary actually mounts
  (EVM routes appear only when the server is built with `--features evm`).

Point any OpenAPI tooling — Swagger UI, ReDoc, or a client-SDK
generator — at either source.

## Regenerating the checked-in file

Run the `gen-openapi` binary and write to `docs/openapi.json`. Build with
`--features evm` so the EVM routes are included:

```sh
cargo run --features evm --bin gen-openapi -- docs/openapi.json
```

With no path argument the spec is printed to stdout instead.

Regenerate and commit `docs/openapi.json` whenever you add or change an
HTTP handler, its request/response types, or a model that appears on the
wire — the same way the proto contract is kept in sync.
