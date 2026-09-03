# Rust Example

Low-level reference clients showing how to wire `guardian-client` directly:

| Binary | Backend | Command |
| --- | --- | --- |
| `guardian-rust-example` | Real Miden node (default `http://localhost:57291`) + GUARDIAN (`http://localhost:50051`) | `cargo run --bin guardian-rust-example` |
| `recover_by_key` | GUARDIAN only (no Miden node required) | `cargo run --bin recover_by_key -- --secret-key-hex 0x<falcon-secret-key-hex>` |

`guardian-rust-example` walks through creating a multisig account, registering it on GUARDIAN, pulling state as another cosigner, and executing signer updates / transactions. Use this if you need to copy/paste minimal code rather than the full demo UI.

It creates a fresh account under a random seed and does not fund it, which is fine only against a node whose `verification_base_fee` is zero. On a fee-charging chain, fund the printed account ID with the native fee asset before the run reaches its first execute — the guarded auth procedure pays the fee before the transaction summary exists, so an unfunded vault aborts there rather than reaching signing.

`recover_by_key` demonstrates account recovery by key ([#218](https://github.com/OpenZeppelin/guardian/issues/218)): given only a Falcon signing key, it calls `lookup_account_by_key_commitment` to discover the account ID(s) the key authorizes and `get_state` to fetch the snapshot. Prerequisite: an account whose authorization set contains the key's commitment must already be configured (run `guardian-rust-example` first).
