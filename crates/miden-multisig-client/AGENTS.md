# crates/miden-multisig-client — Agent notes

See repo root `AGENTS.md` §5 (Rust Multisig SDK) and §7 (High-Risk Areas) for
canonical guidance. This file is local context for the
`miden-multisig-client` crate.

This crate is the **upstream consumer** of `guardian-client`; downstream
consumers are `examples/demo` and `examples/rust`. Any public API change must
propagate per repo `AGENTS.md` §11 Propagation Rule.

## Layout

| File | Size | What lives there |
|------|------|------------------|
| `account.rs` | ~440 lines | `MultisigClient` account create/load/register/sync; underlying `miden-client` integration |
| `builder.rs` | ~190 lines | `MultisigClient::builder()` fluent API (endpoints, data dir, key strategy) |
| `proposal.rs` | ~1200 lines | Proposal lifecycle: propose / list / sign / execute / verify. **The largest and highest-traffic module.** |
| `export.rs` | ~1000 lines | Offline proposal export/import format. **Format-compat is high-risk** — any field change is a wire-break for users running offline flows. |
| `payload.rs` | ~520 lines | Delta payload construction; what the cosigner actually signs |
| `execution.rs` | ~300 lines | Building and submitting the final Miden transaction once threshold is met |
| `procedures.rs` | ~170 lines | Transaction type → MASM procedure mapping (`P2ID`, `consume_notes`, `add_signer`, etc.) |
| `guardian_endpoint.rs` | ~70 lines | GUARDIAN pubkey commitment verification — the trust anchor for SwitchGuardian flows |
| `keystore.rs` | ~160 lines | Falcon key storage and hex (de)serialization |
| `transaction/` | dir | Per-tx-type builders |
| `client/` | dir | Internal GUARDIAN/Miden client wrappers |
| `error.rs` | ~160 lines | `MultisigError` enum; **mandatory exhaustive handling** at all call sites |
| `lib.rs` | ~100 lines | Crate root + rustdoc quick-start |

## High-risk areas (re-stating repo `AGENTS.md` §7 in local terms)

- **Falcon vs ECDSA signing paths** — the SDK supports both signature schemes
  for cosigners and the GUARDIAN ack. Past audit finding M-08 (and issue #229)
  were both **non-deterministic request rebuild from signed metadata**. When
  adding fields the cosigner signs, the verifier on every consumer
  (server, Rust SDK, TS SDK) must rebuild the exact same bytes.
- **Threshold/signature counting** — `proposal.rs` enforces the threshold.
  Off-by-one or unique-cosigner errors here mean either lost-funds risk
  (under-counting) or DoS (over-counting).
- **Offline export/import** — `export.rs` serializes proposals to a portable
  file. Cosigners on isolated networks rely on it. Adding/renaming a field
  silently breaks files created by older clients. Bump the format version
  and stay backwards-compatible on read.
- **SwitchGuardian commitment verification** — `guardian_endpoint.rs` checks
  the GUARDIAN pubkey matches the expected commitment before trust transfer.
  Do not skip this check in any new code path.

## Adding a new transaction type

1. Add the variant to `TransactionType` (in `transaction/` or `lib.rs`).
2. Map it to a MASM procedure in `procedures.rs`.
3. Build the delta in `payload.rs` so the bytes are deterministic and signed
   correctly on **both** Falcon and ECDSA paths.
4. Cover sign + execute in `proposal.rs` and `execution.rs`.
5. Mirror in `packages/miden-multisig-client` (TS SDK) — silent drift
   between the two clients is a §3 rule 5 violation.
6. Exercise via `examples/demo` (CLI) **and** `examples/web` (browser) per
   repo `AGENTS.md` §11 Propagation Rule.

## Tests

```bash
# Targeted module — fastest feedback
cargo test -p miden-multisig-client <module::path>

# Lib tests
cargo test -p miden-multisig-client --lib

# Full crate (includes P2ID serial vector tests under tests/)
cargo test -p miden-multisig-client
```

`tests/p2id_serial_vectors.rs` is the cross-stack vector check — both the TS
SDK and this crate consume the same vectors. If you change P2ID byte
construction here, regenerate vectors and update `packages/miden-multisig-client`
in the same PR.

## Smoke tests

For end-to-end coverage prefer the `smoke-test-rust-multisig-sdk` skill — it
drives `examples/demo` against a local GUARDIAN with the exact flow the SDK
ships against.
