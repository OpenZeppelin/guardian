# Rust Example

Low-level reference client showing how to wire `miden-multisig-client` directly:

| Binary | Backend | Command |
| --- | --- | --- |
| `main` | Real Miden node (default `http://localhost:57291`) + PSM (`http://localhost:50051`) | `cargo run --bin main` |
| `main_mockchain` | MockChain (no node) + PSM (`http://localhost:50051`) | `cargo run --bin main_mockchain` |

Both binaries walk through creating a multisig account, registering it on PSM, pulling state as another cosigner, and executing signer updates / transactions. Use this example if you need to copy/paste minimal code rather than the full demo UI.
