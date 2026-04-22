# Guardian Validation Command Matrix

Use this file to choose the minimum meaningful verification set.

## Server Only

Code in:
- `crates/server/src/services/*`
- `crates/server/src/jobs/*`
- `crates/server/src/storage/*`
- `crates/server/src/metadata/*`

Run:
```bash
cargo test -p guardian-server
```

Add when transport, auth, or integration semantics changed:
```bash
cargo test -p guardian-server --features integration
```

## Rust Client

Code in:
- `crates/client/src/*`

Run:
```bash
cargo test -p guardian-client
```

## TypeScript Guardian Client

Code in:
- `packages/guardian-client/src/*`

Run:
```bash
cd packages/guardian-client && npm test
cd packages/guardian-client && npm run build
```

## Rust Multisig SDK

Code in:
- `crates/miden-multisig-client/src/*`

Run:
```bash
cargo test -p miden-multisig-client
```

Add when the lower client or server boundary changed too:
```bash
cargo test -p guardian-client
```

## TypeScript Multisig SDK

Code in:
- `packages/miden-multisig-client/src/*`

Run:
```bash
cd packages/miden-multisig-client && npm test
cd packages/miden-multisig-client && npm run build
```

## Rust Example Surface

Code in:
- `examples/demo/*`
- Rust multisig changes with user-visible behavior

Run:
```bash
cargo test -p guardian-demo
```

Manual canary:
- `cargo run -p guardian-demo`

## Browser Example Surface

Code in:
- `examples/_shared/multisig-browser/src/*`
- `examples/smoke-web/src/*`
- `examples/web/src/*`

Run:
```bash
cd examples/smoke-web && npm run typecheck && npm run build
cd examples/web && npm run build
```

Manual canary:
- use `smoke-test-ts-multisig-sdk`

## Cross-Layer Changes

If the change touches server contract, auth, proposal lifecycle, or browser signer behavior, combine the relevant package-local commands and add at least one upstream example smoke.

## Broad Changes

Only escalate to workspace-wide checks when:
- multiple packages were edited across Rust and TypeScript
- targeted checks failed in a way that suggests wider fallout
- the task explicitly asks for a broad verification sweep

Escalation:
```bash
cargo test --workspace
```
