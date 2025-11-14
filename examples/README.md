# Examples

Example implementations showing how to use the Private State Manager (PSM).

## Available Examples

### [Rust Example](./rust/)
Command-line client demonstrating PSM integration in Rust.

**Run:**
```bash
cd rust
cargo run --bin main
```

NOTE: Before running any example, you need to start the PSM server.

```bash
cargo run --package private-state-manager-server --bin server
```


### [Demo Example](./demo/)
Interactive CLI demo for PSM with Miden multisig accounts. This demo allows multiple users to collaborate in real-time by running the program in separate terminals.

**Run:**
```bash
cd demo
cargo run
```

NOTE: Before running the demo, you need to start the PSM server and a local Miden node.

