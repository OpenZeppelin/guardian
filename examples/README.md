# Examples

Small projects that showcase different ways to interact with Guardian:

- [`demo/`](./demo/) – Terminal UI for hands-on experimentation (recommended starting point).
- [`operator-smoke-web/`](./operator-smoke-web/) – Minimal browser UI for local Falcon operator login and account API calls through `@openzeppelin/guardian-operator-client`.
- [`policy-module/`](./policy-module/): the built-in policies as a standalone policy module served over gRPC (TLS, per-instance tokens, Dockerfile), for the remote module guide.
- [`rust/`](./rust/) – Low-level Rust example with binaries for both local-node and mockchain flows.
- [`smoke-web/`](./smoke-web/) – Browser smoke harness for multisig workflows and wallet integrations.

All examples expect a running GUARDIAN server
(`GUARDIAN_NETWORK_TYPE=MidenLocal cargo run -p guardian-server --bin server`).
Some also require a Miden node on `http://localhost:57291`. See each
subdirectory README for specific instructions.
