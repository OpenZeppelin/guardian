# miden-confidential-contracts

Builder facade for Guardian's custody accounts on [Miden](https://miden.xyz).

Since the adoption of the audited upstream `AuthGuardedMultisig` component from
[`miden-standards`](https://crates.io/crates/miden-standards), this crate authors no
MASM of its own. It builds the upstream component's storage and metadata, and compiles
its auth code from the copy of upstream's source that
`packages/miden-multisig-client` vendors — the same bytes the TypeScript builder
assembles, with the standards package linked the same way, so both SDKs produce
byte-identical accounts. See `GUARDED_MULTISIG_AUTH_MASM` for why the linkage rather
than the source is what had to be reconciled. It provides:

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
