# packages/miden-multisig-client — Agent notes

See repo root `AGENTS.md` §5 (TS Multisig SDK) and §7 (High-Risk Areas) for
canonical guidance. This file is local context for
`@openzeppelin/miden-multisig-client`.

This package is the TS twin of `crates/miden-multisig-client`. Silent
behavioral drift between the two is a repo `AGENTS.md` §3 rule 5 violation.
Downstream consumer is `examples/web` (browser); changes here must propagate.

## Layout

| Path | What lives there |
|------|------------------|
| `src/multisig.ts` + `src/multisig/` | `MultisigClient` — account create/load/sync, proposal coordination |
| `src/proposal/` | Proposal lifecycle (propose / sign / execute / verify) |
| `src/transaction/` + `src/transaction.ts` | Transaction-type builders, delta construction |
| `src/procedures.ts` | Transaction type → MASM procedure mapping (mirrors Rust `procedures.rs`) |
| `src/inspector.ts` | Account/proposal read paths |
| `src/raw-client.ts` | Low-level GUARDIAN HTTP calls |
| `src/lookupAuth.ts` | Resolve cosigner auth scheme (Falcon vs ECDSA) |
| `src/signer.ts` + `src/signers/` | `FalconSigner`, ECDSA signer, signer interface |
| `src/account/` | Account state, config, commitments |
| `src/types/` + `src/types.ts` | Public domain types |
| `src/utils/` | Hex/byte helpers, Word conversions |
| `src/index.ts` | Public surface — anything not exported here is internal |
| `masm/` | MASM templates (generated into TS at build time) |
| `scripts/generate-masm.mjs` | MASM → TS code generation |
| `tests/p2id-serial-vectors.test.ts` | **Cross-stack vector parity** with the Rust SDK |
| `tests/procedure-roots.test.ts` | MASM procedure root hashes (changes here are wire-affecting) |

## MASM generation

`generate-masm` runs before every `build`, `test`, `typecheck`. **Do not
hand-edit generated MASM TS** — edit the `.masm` templates and re-run. The
relevant scripts are wired into `package.json`:

```bash
npm run build       # generate:masm + tsc
npm test            # generate:masm + vitest
npm run typecheck   # generate:masm + tsc --noEmit
```

There is no separate lint script.

## High-risk areas (TS-side specifics)

- **P2ID serial vector parity** with the Rust SDK
  (`tests/p2id-serial-vectors.test.ts`). If either side regenerates vectors,
  both must update in the same PR.
- **Procedure roots** (`tests/procedure-roots.test.ts`) — changing a MASM
  template changes its root; the test pins expected roots so an accidental
  template tweak fails CI.
- **Deterministic request rebuild from signed metadata.** Issue #229 was a
  regression where the verifier rebuilt `consume_notes` requests differently
  from what cosigners signed (the `NoteIdAndArgs` → `NoteAndArgs` change).
  Same bug class as audit M-08. Any field added to a signed payload must be
  serialized identically on every reader.
- **Browser/Node parity.** The SDK runs in both environments. Avoid Node-only
  APIs (`Buffer`, `fs`, `crypto.randomBytes`) in shared modules; use
  `@noble/*` and `@miden-sdk/miden-sdk` primitives.

## Adding a new transaction type

1. Add MASM template under `masm/` if a new on-chain procedure is needed.
2. Update `procedures.ts` and add the TS type in `transaction/` or
   `transaction.ts`.
3. Build the delta in `multisig/` so bytes match what the cosigner signs.
4. Cover sign + execute flow in tests.
5. Mirror in `crates/miden-multisig-client` (Rust SDK). Update the shared
   P2ID serial vectors if you touched P2ID.
6. Exercise via `examples/web` (browser) and `examples/demo` (CLI).

## Tests

```bash
npm test                                    # full vitest run
npx vitest run src/multisig.test.ts         # one file
npm run typecheck                           # tsc --noEmit
```

## Smoke tests

For end-to-end coverage prefer the `smoke-test-ts-multisig-sdk` skill — it
drives `examples/smoke-web` against a local GUARDIAN with the exact flow the
SDK ships against.
