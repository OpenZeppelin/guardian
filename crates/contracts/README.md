# miden-confidential-contracts

Builder facade for Guardian's custody accounts on [Miden](https://miden.xyz).

Since the adoption of the audited upstream `AuthGuardedMultisig` component from
[`miden-standards`](https://crates.io/crates/miden-standards), this crate authors no
MASM of its own and compiles none. It calls `AuthGuardedMultisig::new` and builds the
component's storage and metadata around the result, so the procedure roots are upstream's
by construction rather than by a pin that has to be kept in step. The TypeScript builder
calls the same component through the web SDK, so both SDKs produce byte-identical
accounts. It provides:

- `MultisigGuardianConfig` / `MultisigGuardianBuilder` — the single source of truth
  for constructing guarded-multisig accounts (validation, storage layout, signature
  scheme mapping) across the server, SDKs, examples, and benchmarks.
- The MockChain behavior test suite (`tests/`) exercising the upstream component's
  authentication paths: update signers, per-procedure thresholds, guardian-key
  rotation, and replay protection.

Cross-SDK determinism (a TypeScript-built account must be byte-identical to a
Rust-built one) is pinned by `test_browser_deterministic_account_matches_rust_builder`
against the Playwright gate in `packages/miden-multisig-client/tests/browser/`.

## Running tests

```bash
cargo test -p miden-confidential-contracts --all-targets
```

## License

Released under the [MIT License](LICENSE).
